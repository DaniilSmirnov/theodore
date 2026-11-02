//! What a space's agent asks Theodore to do with the change (docs/spaces.md
//! "Tools the agent gets"): ship it, read it and its review, comment on it,
//! move its base, show progress. Each goes through the same path as the
//! space page's buttons, with their guarantees; refusals come back as
//! one line the agent can tell the user.
//!
//! ```text
//! ship ─────────────▶ ship_space (as the running turn) ─▶ PR · branch · merge
//! get_space_diff ───▶ snapshot(worktree) ─▶ diff base_sha...snapshot
//! get_review_comments ▶ drafts · findings · threads of review_pr and pr
//! add_review_comment ─▶ add_finding on review_pr (Review, "Send to agent")
//! set_base ─────────▶ fetch ─▶ merge-base ─▶ base, base_sha ─▶ live diff
//! update_progress ──▶ space.progress (cleared when the turn ends)
//! ```

use std::path::Path;
use std::time::Duration;

use theodore_contract::{
    ShipAction, ShipKind, ShipSpaceRequest, Space, SpaceProgress, SpaceProgressState,
    SpaceProgressStep, SpaceReviewRequest, SpaceStatus, models,
};
use theodore_domain::{
    Anchor, Author, Draft, Finding, FindingId, FindingState, PrDetail, PrId, PrSummary, Side,
    SkillId, Thread,
};
use theodore_ports::{
    BaseChange, NewAction, NewReviewComment, ProgressStep, ProgressStepState, ReviewItem,
    ReviewItemComment, ReviewItemKind, ReviewItemState, ReviewSource, ShipOutcome, ShipToolAction,
    SpaceDiff, SpaceToolError, SyncActionCode,
};

use crate::core::Core;
use crate::error::AppError;
use crate::usecases::space_feedback::location;
use crate::wire::ThreadRow;

/// How long `ship` waits for the host to answer an Open PR (the outbox
/// opens it) before saying it is on its way.
const PR_WAIT: Duration = Duration::from_secs(15);
/// The skill a space agent's own findings are filed under.
pub(crate) const SPACE_AGENT_SKILL: &str = "space-agent";
const MAX_STEPS: usize = 20;
const MAX_LABEL: usize = 120;
const MAX_NOTE: usize = 280;

/// The error text without the "invalid request:" prefix: the agent passes
/// it on to the user.
fn refusal(e: AppError) -> SpaceToolError {
    SpaceToolError::Unavailable(match e {
        AppError::Invalid(m) => m,
        other => other.to_string(),
    })
}

fn unavailable(m: impl Into<String>) -> SpaceToolError {
    SpaceToolError::Unavailable(m.into())
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}

impl Core {
    /// `ship`: the Ship menu's action, on behalf of the running turn `run`.
    pub async fn space_tool_ship(
        &self,
        space: &str,
        run: &str,
        action: ShipToolAction,
        title: Option<String>,
    ) -> Result<ShipOutcome, SpaceToolError> {
        let action_req = match action {
            ShipToolAction::OpenPr => ShipAction::OpenPr,
            ShipToolAction::OpenDraftPr => ShipAction::OpenDraftPr,
            ShipToolAction::Push => ShipAction::Push,
            ShipToolAction::MergeLocally => ShipAction::MergeLocally,
            ShipToolAction::KeepBranch => ShipAction::KeepBranch,
        };
        let row = self
            .ship_space_as(
                ShipSpaceRequest {
                    space: space.to_string(),
                    action: action_req,
                    title: title.filter(|t| !t.trim().is_empty()),
                },
                Some(run),
            )
            .await
            .map_err(refusal)?;
        let shipped = row
            .shipped
            .clone()
            .ok_or_else(|| unavailable("nothing was shipped"))?;
        let opening = matches!(action, ShipToolAction::OpenPr | ShipToolAction::OpenDraftPr);
        let pr = match &row.pr {
            Some(pr) => Some(pr.clone()),
            None if opening => self.wait_for_space_pr(&row.id).await,
            None => None,
        };
        let summary_row = match &pr {
            Some(pr) => self.pr_summary(pr).await,
            None => None,
        };
        let pr_number = summary_row
            .as_ref()
            .map(|s| s.pr.number)
            .or_else(|| pr.as_ref().and_then(pr_number_of));
        let pr_url = summary_row.map(|s| s.url).filter(|u| !u.is_empty());
        let summary = match (action, shipped.how, pr_number) {
            (ShipToolAction::OpenPr | ShipToolAction::OpenDraftPr, _, Some(n)) => format!(
                "Opened {}PR #{n} from {}",
                if action == ShipToolAction::OpenDraftPr {
                    "draft "
                } else {
                    ""
                },
                shipped.branch
            ),
            (ShipToolAction::OpenPr | ShipToolAction::OpenDraftPr, _, None) => format!(
                "Pushed {}; the PR is being opened and shows in the space header once the \
                 host answers",
                shipped.branch
            ),
            (ShipToolAction::Push, _, n) => format!(
                "Pushed one more commit to {}{}",
                shipped.branch,
                n.map(|n| format!(" (PR #{n})")).unwrap_or_default()
            ),
            (_, ShipKind::MergedLocally, _) => {
                format!("Merged into {} in the local folder", shipped.branch)
            }
            _ => format!("Wrote branch {} in the local folder", shipped.branch),
        };
        Ok(ShipOutcome {
            action,
            branch: shipped.branch,
            sha: shipped.sha.0,
            pr_number,
            pr_url,
            summary,
        })
    }

    /// The space's PR once the outbox opened it, `None` after `PR_WAIT`.
    async fn wait_for_space_pr(&self, space: &str) -> Option<PrId> {
        let started = std::time::Instant::now();
        loop {
            if let Ok(row) = self.space_row(space).await
                && let Some(pr) = row.pr
            {
                return Some(pr);
            }
            if started.elapsed() >= PR_WAIT {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn pr_summary(&self, pr: &PrId) -> Option<PrSummary> {
        let data = self.get_row(models::PR, &pr.0).await.ok()??;
        serde_json::from_value(data).ok()
    }

    /// `get_space_diff`: `base_sha...` the worktree as it is now, so edits
    /// of the running turn are in it. Only `file` when set.
    pub async fn space_tool_diff(
        &self,
        space: &str,
        file: Option<&str>,
    ) -> Result<SpaceDiff, SpaceToolError> {
        let row = self.space_row(space).await.map_err(refusal)?;
        let wt = Path::new(&row.worktree);
        let git = &self.0.deps.git;
        let (repo, head) = if wt.exists() {
            let head = git
                .space_snapshot(wt)
                .await
                .map_err(|e| unavailable(e.to_string()))?;
            (wt.to_path_buf(), head)
        } else {
            // Archived: the last checkpoint, from the cache.
            let project = self.space_project(&row).await.map_err(refusal)?;
            let repo = self.ensure_project_cache(&project).await.map_err(refusal)?;
            (repo, row.head_sha.clone())
        };
        let revision = git
            .diff(&repo, &row.base_sha, &head, &Default::default())
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        let files = revision
            .files
            .into_iter()
            .filter(|f| file.is_none_or(|p| f.path == p || f.old_path.as_deref() == Some(p)))
            .collect();
        Ok(SpaceDiff {
            base: row.base.clone(),
            base_sha: row.base_sha.clone(),
            uncommitted: head != row.head_sha,
            head,
            files,
        })
    }

    /// `get_review_comments`: drafts, findings and threads on the space's
    /// own review and its PR, file by file. Ignored findings and those
    /// already turned into a draft are left out.
    pub async fn space_tool_comments(
        &self,
        space: &str,
        file: Option<&str>,
    ) -> Result<Vec<ReviewItem>, SpaceToolError> {
        let row = self.space_row(space).await.map_err(refusal)?;
        let mut items = Vec::new();
        let sources = [
            (row.review_pr.clone(), ReviewSource::Space),
            (row.pr.clone(), ReviewSource::Pr),
        ];
        for (pr, on) in sources {
            let Some(pr) = pr else { continue };
            for d in self.rows_of::<Draft>(models::DRAFT, &pr).await {
                items.push(draft_item(&d, on));
            }
            for f in self.rows_of::<Finding>(models::FINDING, &pr).await {
                if !matches!(f.state, FindingState::Ignored | FindingState::Drafted) {
                    items.push(finding_item(&f, on));
                }
            }
            for t in self.threads_of(&pr).await {
                items.push(thread_item(&t, on));
            }
        }
        items.retain(|i| file.is_none_or(|p| i.path.as_deref() == Some(p)));
        items.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        Ok(items)
    }

    async fn rows_of<T: serde::de::DeserializeOwned>(&self, model: &str, pr: &PrId) -> Vec<T> {
        let store = self.0.deps.store.clone();
        let (model, pr) = (model.to_string(), pr.0.clone());
        tokio::task::spawn_blocking(move || store.query(&model, "pr", &pr))
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| serde_json::from_value(r.data).ok())
            .collect()
    }

    /// The host's threads (inside `pr_detail`), plus any just posted and
    /// not fetched back yet (`thread` rows).
    async fn threads_of(&self, pr: &PrId) -> Vec<Thread> {
        let mut threads: Vec<Thread> = match self.get_row(models::PR_DETAIL, &pr.0).await {
            Ok(Some(data)) => serde_json::from_value::<PrDetail>(data)
                .map(|d| d.threads)
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        for r in self.rows_of::<ThreadRow>(models::THREAD, pr).await {
            if !threads.iter().any(|t| t.id == r.thread.id) {
                threads.push(r.thread);
            }
        }
        threads
    }

    /// `add_review_comment`: a finding on the space's branch review, at
    /// `line` of `file` on the new side. Returns its id.
    pub async fn space_tool_comment(
        &self,
        space: &str,
        agent: theodore_domain::AgentId,
        c: NewReviewComment,
    ) -> Result<FindingId, SpaceToolError> {
        let row = self.space_row(space).await.map_err(refusal)?;
        let review = match row.review_pr.clone() {
            Some(pr) => pr,
            None => {
                self.space_review(SpaceReviewRequest {
                    space: row.id.clone(),
                    group: false,
                })
                .await
                .map_err(refusal)?
                .pr
            }
        };
        let body = c.body.trim();
        if body.is_empty() {
            return Err(unavailable("the comment is empty"));
        }
        if c.line == 0 {
            return Err(unavailable("lines start at 1"));
        }
        let title = clip(body.lines().next().unwrap_or_default(), 80);
        let finding = Finding {
            id: FindingId(uuid::Uuid::new_v4().to_string()),
            pr: review.clone(),
            skill: SkillId(SPACE_AGENT_SKILL.into()),
            agents: vec![agent],
            anchor: Anchor {
                rev: row.head_sha.clone(),
                path: c.file.trim().trim_start_matches("./").to_string(),
                side: Side::New,
                line: c.line,
                start_line: None,
                start_side: None,
            },
            severity: c.severity,
            title,
            body: body.to_string(),
            state: FindingState::Open,
            fix: None,
            outdated: false,
        };
        self.add_finding(review, finding).await.map_err(refusal)
    }

    /// `set_base`: the space diffs against the merge-base with `branch`
    /// from now on. The worktree, the turns and any PR stay as they are.
    pub async fn space_tool_set_base(
        &self,
        space: &str,
        run: &str,
        branch: &str,
    ) -> Result<BaseChange, SpaceToolError> {
        let branch = branch.trim().trim_start_matches("refs/heads/");
        if !crate::usecases::spaces::is_valid_branch(branch) {
            return Err(unavailable(format!("bad branch name {branch:?}")));
        }
        let guard = self.0.ship_lock.lock().await;
        let row = self.space_row(space).await.map_err(refusal)?;
        busy(&row, run)?;
        if branch == row.branch {
            return Err(unavailable(
                "that is the space's own branch; pick the branch it should merge into",
            ));
        }
        let note = row.pr.as_ref().map(|pr| {
            format!(
                "the space's PR{} still targets {}: Theodore doesn't retarget it, change its \
                 base on the host if it should follow",
                pr_number_of(pr)
                    .map(|n| format!(" #{n}"))
                    .unwrap_or_default(),
                row.base
            )
        });
        if branch == row.base {
            return Ok(BaseChange {
                base: row.base.clone(),
                base_sha: row.base_sha.0.clone(),
                previous: row.base.clone(),
                note: Some(format!("{branch} already is the base")),
            });
        }
        let project = self.space_project(&row).await.map_err(refusal)?;
        let repo = self.ensure_project_cache(&project).await.map_err(refusal)?;
        let git = &self.0.deps.git;
        let tip = git
            .resolve_ref(&repo, &format!("refs/heads/{branch}"))
            .await
            .map_err(|_| {
                unavailable(format!(
                    "there is no branch {branch} {}",
                    if project.path.is_some() {
                        "in the folder"
                    } else {
                        "on the host"
                    }
                ))
            })?;
        let base_sha = git
            .merge_base(&repo, &tip, &row.head_sha)
            .await
            .map_err(|_| unavailable(format!("the space and {branch} share no history")))?;
        self.apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: row.id.clone(),
            action: SyncActionCode::C,
            data: Some(serde_json::json!({
                "base": branch,
                "base_sha": base_sha,
                "updated_at": self.0.deps.clock.now(),
            })),
        }])
        .await
        .map_err(|e| unavailable(e.to_string()))?;
        drop(guard);
        // The live diff follows the new fork point.
        self.space_review(SpaceReviewRequest {
            space: row.id.clone(),
            group: false,
        })
        .await
        .map_err(refusal)?;
        Ok(BaseChange {
            base: branch.to_string(),
            base_sha: base_sha.0,
            previous: row.base,
            note,
        })
    }

    /// `update_progress`: replaces the tracker above the composer; no
    /// steps clears it.
    pub async fn space_tool_progress(
        &self,
        space: &str,
        run: &str,
        steps: Vec<ProgressStep>,
        note: Option<String>,
    ) -> Result<(), SpaceToolError> {
        let row = self.space_row(space).await.map_err(refusal)?;
        if row.run.as_deref() != Some(run) {
            return Err(unavailable("this turn is over"));
        }
        let steps: Vec<SpaceProgressStep> = steps
            .into_iter()
            .filter(|s| !s.label.trim().is_empty())
            .take(MAX_STEPS)
            .map(|s| SpaceProgressStep {
                label: clip(&s.label, MAX_LABEL),
                state: match s.state {
                    ProgressStepState::Pending => SpaceProgressState::Pending,
                    ProgressStepState::Active => SpaceProgressState::Active,
                    ProgressStepState::Done => SpaceProgressState::Done,
                },
            })
            .collect();
        let progress = (!steps.is_empty()).then(|| SpaceProgress {
            steps,
            note: note.map(|n| clip(&n, MAX_NOTE)).filter(|n| !n.is_empty()),
            at: self.0.deps.clock.now(),
        });
        self.apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: row.id,
            action: SyncActionCode::C,
            data: Some(serde_json::json!({ "progress": progress })),
        }])
        .await
        .map_err(|e| unavailable(e.to_string()))?;
        Ok(())
    }
}

/// Refuses while something other than the calling turn holds the space.
fn busy(row: &Space, run: &str) -> Result<(), SpaceToolError> {
    match row.status {
        SpaceStatus::Archived => Err(unavailable("this space is archived")),
        SpaceStatus::Running if row.run.as_deref() != Some(run) => Err(unavailable(
            "another turn or the space's check is running; try again when it ends",
        )),
        _ => Ok(()),
    }
}

/// `12` of `github:acme/widget#12`.
fn pr_number_of(pr: &PrId) -> Option<u64> {
    pr.0.rsplit_once('#')?.1.parse().ok()
}

fn author_name(a: &Author) -> String {
    match a {
        Author::Me => "you".into(),
        Author::Agent(agent) => agent.as_str().into(),
    }
}

fn anchored(a: &Anchor) -> (String, Option<String>, Option<u32>) {
    (location(a), Some(a.path.clone()), Some(a.line))
}

fn draft_item(d: &Draft, on: ReviewSource) -> ReviewItem {
    let (location, path, line) = anchored(&d.anchor);
    ReviewItem {
        kind: ReviewItemKind::Draft,
        id: d.id.0.clone(),
        on,
        location,
        path,
        line,
        state: if d.outdated {
            ReviewItemState::Outdated
        } else {
            ReviewItemState::Draft
        },
        severity: None,
        title: None,
        comments: vec![ReviewItemComment {
            author: author_name(&d.source),
            body: d.body.clone(),
        }],
    }
}

fn finding_item(f: &Finding, on: ReviewSource) -> ReviewItem {
    let (location, path, line) = anchored(&f.anchor);
    let author = f
        .agents
        .iter()
        .map(|a| a.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    ReviewItem {
        kind: ReviewItemKind::Finding,
        id: f.id.0.clone(),
        on,
        location,
        path,
        line,
        state: if f.outdated {
            ReviewItemState::Outdated
        } else {
            ReviewItemState::Open
        },
        severity: Some(f.severity),
        title: Some(f.title.clone()),
        comments: vec![ReviewItemComment {
            author,
            body: f.body.clone(),
        }],
    }
}

fn thread_item(t: &Thread, on: ReviewSource) -> ReviewItem {
    let (location, path, line) = match &t.anchor {
        Some(a) => anchored(a),
        None => ("pull request".to_string(), None, None),
    };
    ReviewItem {
        kind: ReviewItemKind::Thread,
        id: t.id.0.clone(),
        on,
        location,
        path,
        line,
        state: if t.resolved {
            ReviewItemState::Resolved
        } else if t.outdated {
            ReviewItemState::Outdated
        } else {
            ReviewItemState::Open
        },
        severity: None,
        title: None,
        comments: t
            .comments
            .iter()
            .map(|c| ReviewItemComment {
                author: c.author.login.clone(),
                body: c.body.clone(),
            })
            .collect(),
    }
}
