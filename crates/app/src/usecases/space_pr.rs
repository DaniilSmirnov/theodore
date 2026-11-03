//! A space from its open PR to merged and beyond (docs/spaces.md §3, §6,
//! §7), without leaving the space page:
//!
//! ```text
//! Open PR ─▶ body: the repo's PR template (filled in by the agent, else
//!            template + goal), or the goal ─▶ footer theodore://space/<id>
//! Merge   ─▶ merge_space ─▶ merge_info ─▶ method ─▶ merge_pr (now · auto · queue)
//! merged  ─▶ Continue ─▶ fetch the base ─▶ branch <login>/<slug>-2
//!            worktree + ref ─▶ the base's new tip (+ turns not pushed, replayed)
//!            pr · shipped · done_at cleared, the round kept,
//!            a note for the next turn's prompt
//! ```

use std::path::Path;
use std::time::Duration;

use theodore_contract::{
    MergeInfo, MergeMethod, MergeRequest, MergeSpaceRequest, MergeSpaceResult, MergeState,
    ShipKind, Space, SpaceReviewRequest, SpaceRound, SpaceStatus, models,
};
use theodore_domain::{AgentId, PrId, PrState, Sha};
use theodore_ports::{
    AgentEvent, AgentMode, AgentRequest, DiffOptions, GitError, Identity, NewAction,
    SpaceCheckpoint, SpaceCommit, SyncActionCode,
};

use crate::core::Core;
use crate::error::{AppError, AppResult};

/// Where a repo keeps its PR template, in the order the host looks.
pub(crate) const PR_TEMPLATES: [&str; 4] = [
    ".github/pull_request_template.md",
    ".github/PULL_REQUEST_TEMPLATE.md",
    "docs/pull_request_template.md",
    "pull_request_template.md",
];

/// The hidden marker the link-back footer starts with.
const FOOTER_MARKER: &str = "<!-- theodore-space:";
/// The scheme a production build registers (`identity::url_scheme`).
const DEFAULT_SCHEME: &str = "theodore";
/// How long the agent may take to fill the template in.
const FILL_TIMEOUT: Duration = Duration::from_secs(60);
/// Files listed in the fill prompt's diff summary.
const FILL_FILES: usize = 60;

impl Core {
    /// The scheme of the PR footer's link back to the space: this build's
    /// (`theodore-staging`, …); `None` keeps `theodore`.
    pub fn set_space_link_scheme(&self, scheme: Option<&str>) {
        *self.0.space_link_scheme.lock().expect("link scheme lock") = scheme.map(str::to_string);
    }

    fn space_link(&self, space: &str) -> String {
        let scheme = self
            .0
            .space_link_scheme
            .lock()
            .expect("link scheme lock")
            .clone()
            .unwrap_or_else(|| DEFAULT_SCHEME.into());
        format!("{scheme}://space/{space}")
    }

    /// The body of a space's new PR: the repo's PR template (filled in by
    /// the agent when it can, else the template with the goal above it),
    /// or the goal; then the footer linking back to the space.
    pub(crate) async fn space_pr_body(
        &self,
        space: &Space,
        repo_path: &Path,
        head: &Sha,
        goal_body: String,
    ) -> String {
        let body = match self.pr_template(repo_path, head).await {
            None => goal_body,
            Some(template) => match self.fill_template(space, repo_path, head, &template).await {
                Some(filled) => filled,
                None => template_with_goal(&template, &space.goal),
            },
        };
        with_footer(&body, &space.id, &self.space_link(&space.id))
    }

    /// The first PR template the change itself has (`PR_TEMPLATES`).
    async fn pr_template(&self, repo_path: &Path, head: &Sha) -> Option<String> {
        for path in PR_TEMPLATES {
            if let Ok(Some(bytes)) = self.0.deps.git.show(repo_path, head, path).await {
                let text = String::from_utf8_lossy(&bytes).trim().to_string();
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
        None
    }

    /// One quick read-only run of the space's agent (its cheapest model)
    /// that fills `template` in from the goal and the diff. `None` on any
    /// failure, a timeout, or an answer that dropped the template.
    async fn fill_template(
        &self,
        space: &Space,
        repo_path: &Path,
        head: &Sha,
        template: &str,
    ) -> Option<String> {
        let files = self
            .0
            .deps
            .git
            .diff(repo_path, &space.base_sha, head, &DiffOptions::default())
            .await
            .ok()?
            .files;
        let mut summary: Vec<String> = files
            .iter()
            .take(FILL_FILES)
            .map(|f| format!("- {} (+{} −{})", f.path, f.additions, f.deletions))
            .collect();
        if files.len() > FILL_FILES {
            summary.push(format!("- … and {} more", files.len() - FILL_FILES));
        }
        let prompt = fill_prompt(template, &space.goal, &summary.join("\n"));
        let req = AgentRequest {
            agent: space.agent,
            prompt,
            system: Some(
                "You write pull request descriptions. Answer with the description only.".into(),
            ),
            cwd: Path::new(&space.worktree).to_path_buf(),
            mode: AgentMode::ReadOnly,
            mcp: None,
            model: (space.agent == AgentId::Claude).then(|| "haiku".to_string()),
            effort: None,
            stall_after_ms: self.stall_after_ms().await,
            env: vec![],
            user_mcp: false,
            attachments: vec![],
            resume: None,
            stream_text: false,
        };
        let run = async {
            let mut run = self.0.deps.agents.run(req).await.ok()?;
            let mut text = String::new();
            while let Some(event) = run.events.recv().await {
                match event {
                    AgentEvent::Text { text: t } => text.push_str(&t),
                    AgentEvent::Done { final_text, .. } => {
                        if !final_text.is_empty() {
                            text = final_text;
                        }
                        return Some(text);
                    }
                    AgentEvent::Error { .. } => return None,
                    _ => {}
                }
            }
            None
        };
        let filled = tokio::time::timeout(FILL_TIMEOUT, run).await.ok()??;
        let filled = unfence(&filled);
        keeps_template(template, &filled).then_some(filled)
    }

    /// Merges the space's PR (docs/spaces.md §3): `method` if the repo
    /// allows it, else the host's default; auto-merge when it can't merge
    /// yet and the host offers it; the merge queue when the base has one.
    /// Refused with turns not pushed yet: they'd be left out.
    pub async fn merge_space(&self, req: MergeSpaceRequest) -> AppResult<MergeSpaceResult> {
        let space = self.space_row(&req.space).await?;
        let pr = space
            .pr
            .clone()
            .ok_or_else(|| AppError::Invalid("this space has no PR to merge".into()))?;
        if space.status == SpaceStatus::Running {
            return Err(AppError::Invalid(
                "the agent is still working; let the turn end and push it first".into(),
            ));
        }
        if space
            .shipped
            .as_ref()
            .is_some_and(|s| s.how == ShipKind::Pr && s.head != space.head_sha)
        {
            return Err(AppError::Invalid(
                "some turns aren't pushed yet; push them first so the merge has them".into(),
            ));
        }
        if self.pr_state(&pr).await == Some(PrState::Merged) {
            return Err(AppError::Invalid("the PR is already merged".into()));
        }
        let info = self.merge_info(pr.clone()).await?;
        let method = merge_method(req.method, &info)?;
        let (auto, queue) = merge_mode(&info).map_err(AppError::Invalid)?;
        self.merge_pr(MergeRequest {
            pr,
            method,
            title: None,
            body: None,
            auto,
            queue,
        })
        .await?;
        Ok(MergeSpaceResult {
            method,
            auto,
            queue,
        })
    }

    /// Continue after merge (docs/spaces.md §7): the space moves to a new
    /// branch on the base's fresh tip, which has the merged work; turns
    /// that weren't in the PR come along. Turns and transcript stay;
    /// `pr`, `shipped` and `done_at` clear, so it's drafting again and
    /// never auto-archives for the old PR.
    pub async fn continue_space(&self, id: &str) -> AppResult<Space> {
        let _guard = self.0.ship_lock.lock().await;
        let space = self.space_row(id).await?;
        match space.status {
            SpaceStatus::Running => {
                return Err(AppError::Invalid(
                    "the agent is still working; stop it or wait for the turn to end".into(),
                ));
            }
            SpaceStatus::Archived => {
                return Err(AppError::Invalid(
                    "this space is archived; unarchive it first".into(),
                ));
            }
            SpaceStatus::Idle | SpaceStatus::Failed => {}
        }
        if space.updating.is_some() {
            return Err(AppError::Invalid(
                "an update from the base is still merging; complete or abort it first".into(),
            ));
        }
        let pr = space.pr.clone().ok_or_else(|| {
            AppError::Invalid("only a space whose PR merged can continue on a new branch".into())
        })?;
        if self.pr_state(&pr).await != Some(PrState::Merged) {
            return Err(AppError::Invalid("its PR hasn't merged yet".into()));
        }
        let project = self.space_project(&space).await?;
        let cache = self
            .project_cache(&project.repo, project.path.as_deref())
            .await?;
        if !cache.fetched {
            return Err(AppError::Invalid(format!(
                "can't reach the host to fetch {}; continue once you're online",
                space.base
            )));
        }
        let repo_path = cache.path;
        let git = &self.0.deps.git;
        let tip = git
            .remote_branch(&repo_path, &project.repo, &space.base)
            .await?
            .ok_or_else(|| AppError::Invalid(format!("{} is gone from the host", space.base)))?;

        let wt = git
            .space_worktree(&repo_path, &space.id, &space.base_sha)
            .await?;
        // Edits by hand since the last turn are a turn not pushed yet.
        let head = git
            .space_checkpoint(
                &wt.path,
                &space.id,
                &format!("{}: edits before continuing", space.name),
            )
            .await?
            .sha;
        let pushed = space
            .shipped
            .as_ref()
            .filter(|s| s.how == ShipKind::Pr)
            .map(|s| s.head.clone());
        let start = match pushed {
            // Turns after the last push weren't merged: replayed onto the
            // new tip (three-way, no worktree).
            Some(pushed) if pushed != head => git
                .space_commit(
                    &repo_path,
                    &space.id,
                    &SpaceCommit {
                        head: head.clone(),
                        parent: tip.clone(),
                        replay_from: Some(pushed),
                        // The new parent is the base's tip itself.
                        merge_parent: None,
                        message: format!("{}: turns after the merged PR", space.name),
                        author: Identity {
                            name: "Theodore".into(),
                            email: "theodore@localhost".into(),
                        },
                    },
                )
                .await
                .map_err(|e| match e {
                    GitError::Refused(m) => AppError::Invalid(format!(
                        "the turns that weren't in the PR conflict with the new {}: {m}",
                        space.base
                    )),
                    other => AppError::Git(other),
                })?,
            _ => tip.clone(),
        };
        // A snapshot on the base's tip (docs/spaces.md §6): `HEAD` goes to
        // the tip, the unpushed turns stay as uncommitted edits on it — the
        // replayed commit's parent is the tip, like any checkpoint's.
        let to = SpaceCheckpoint {
            sha: start.clone(),
            head: tip.clone(),
        };
        git.space_reset(&wt.path, &space.id, &to).await?;

        let branch = self.continued_branch(&space, &repo_path).await?;
        let now = self.0.deps.clock.now();
        let mut rounds = space.rounds.clone();
        rounds.push(SpaceRound {
            pr: pr.clone(),
            branch: space.branch.clone(),
            base_sha: space.base_sha.clone(),
            head_sha: space
                .shipped
                .as_ref()
                .map_or_else(|| space.head_sha.clone(), |s| s.head.clone()),
            at: now,
        });
        let note = continue_note(&pr, &space.branch, &space.base, &branch);
        self.apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: space.id.clone(),
            action: SyncActionCode::C,
            data: Some(serde_json::json!({
                "branch": branch,
                "base_sha": tip,
                "head_sha": start,
                "worktree_head": (start != tip).then_some(&tip),
                "worktree": wt.path.display().to_string(),
                "pr": null,
                "shipped": null,
                "done_at": null,
                "status": SpaceStatus::Idle,
                "error": null,
                "rounds": rounds,
                "continue_note": note,
                "updated_at": now,
            })),
        }])
        .await?;
        self.space_review(SpaceReviewRequest {
            space: space.id.clone(),
            group: false,
        })
        .await?;
        self.space_row(&space.id).await
    }

    /// The next round's branch: `<branch>-2`, `-3`… — one no other space
    /// has, no earlier round used, and the host doesn't have.
    async fn continued_branch(&self, space: &Space, repo_path: &Path) -> AppResult<String> {
        let project = self.space_project(space).await?;
        let mut taken: Vec<String> = self
            .list_spaces(Some(space.project.clone()))
            .await?
            .into_iter()
            .filter(|s| s.id != space.id)
            .map(|s| s.branch)
            .collect();
        taken.extend(space.rounds.iter().map(|r| r.branch.clone()));
        taken.push(space.branch.clone());
        for candidate in next_branches(&space.branch, !space.rounds.is_empty()).take(50) {
            if taken.contains(&candidate) {
                continue;
            }
            // Best effort: a lookup that fails leaves it to the push, which
            // refuses a taken name anyway.
            match self
                .0
                .deps
                .git
                .remote_branch(repo_path, &project.repo, &candidate)
                .await
            {
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => return Ok(candidate),
            }
        }
        Err(AppError::Invalid(format!(
            "no free branch name after {}",
            space.branch
        )))
    }

    /// The poller's sweep found the space's PR merged: marks it done unless
    /// the space moved on meanwhile (Continue cleared `pr`).
    pub(crate) async fn mark_space_done(&self, space: &str, pr: &PrId, now: i64) -> bool {
        let _guard = self.0.ship_lock.lock().await;
        let Ok(fresh) = self.space_row(space).await else {
            return false;
        };
        if fresh.pr.as_ref() != Some(pr) || fresh.done_at.is_some() {
            return false;
        }
        self.apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: space.to_string(),
            action: SyncActionCode::C,
            data: Some(serde_json::json!({ "done_at": now })),
        }])
        .await
        .is_ok()
    }
}

/// The next turn after Continue hears that its PR merged; later ones don't.
pub(crate) fn with_continue_note(space: &Space, prompt: String) -> String {
    match &space.continue_note {
        Some(note) => format!("{note}\n\n{prompt}"),
        None => prompt,
    }
}

fn continue_note(pr: &PrId, old: &str, base: &str, new: &str) -> String {
    let number = pr.0.rsplit_once('#').map_or(pr.0.as_str(), |(_, n)| n);
    format!(
        "Note: the previous pull request (#{number}, branch {old}) was merged into {base}. \
         This space now starts from the updated {base}, which has that work, and continues \
         on a new branch, {new}."
    )
}

/// `branch-2`, `branch-3`…; a continued space's branch (`continued`)
/// already ends in its round's number, which counts on from there.
fn next_branches(branch: &str, continued: bool) -> impl Iterator<Item = String> {
    let (stem, from) = match branch.rsplit_once('-') {
        Some((stem, n)) if continued && !stem.is_empty() => match n.parse::<u32>() {
            Ok(n) => (stem.to_string(), n + 1),
            Err(_) => (branch.to_string(), 2),
        },
        _ => (branch.to_string(), 2),
    };
    (from..)
        .map(move |n| format!("{stem}-{n}"))
        .filter(|b| crate::usecases::spaces::is_valid_branch(b))
}

/// `asked` if the repo allows it, else the host's default, else the first
/// allowed method.
fn merge_method(asked: Option<MergeMethod>, info: &MergeInfo) -> AppResult<MergeMethod> {
    let allowed =
        |m: &MergeMethod| info.allowed_methods.is_empty() || info.allowed_methods.contains(m);
    if let Some(m) = asked {
        return if allowed(&m) {
            Ok(m)
        } else {
            Err(AppError::Invalid(format!(
                "the repo doesn't allow {} merges",
                crate::usecases::telemetry::wire_str(&m)
            )))
        };
    }
    Ok(info
        .default_method
        .filter(allowed)
        .or_else(|| info.allowed_methods.first().copied())
        .unwrap_or(MergeMethod::Squash))
}

/// `(auto, queue)` for the PR's merge state, or why it can't merge.
fn merge_mode(info: &MergeInfo) -> Result<(bool, bool), String> {
    if info.in_queue {
        return Err("the PR is already in the merge queue".into());
    }
    if info.auto_merge_enabled {
        return Err("auto-merge is already on: it merges once checks and reviews pass".into());
    }
    if info.merge_queue {
        return Ok((false, true));
    }
    let ready = info.mergeable != Some(false)
        && matches!(info.state, MergeState::Clean | MergeState::Unstable);
    if ready {
        return Ok((false, false));
    }
    match info.state {
        MergeState::Dirty => Err(
            "the PR conflicts with its base; ask the agent to merge the base in, then push".into(),
        ),
        _ if info.auto_merge_allowed => Ok((true, false)),
        MergeState::Blocked => Err(
            "required checks or reviews haven't passed yet, and the repo has no auto-merge".into(),
        ),
        MergeState::Behind => Err("the branch is behind its base; update it first".into()),
        _ => Err("the host is still working out whether it can merge; try again shortly".into()),
    }
}

/// `body` ending in the link-back footer, exactly once: an old footer is
/// replaced, never repeated.
pub(crate) fn with_footer(body: &str, space: &str, link: &str) -> String {
    let body = match body.find(FOOTER_MARKER) {
        Some(i) => &body[..i],
        None => body,
    }
    .trim_end();
    let footer = format!("{FOOTER_MARKER}{space} -->\n<sub>From a Theodore space: {link}</sub>");
    if body.is_empty() {
        footer
    } else {
        format!("{body}\n\n{footer}")
    }
}

/// The template as is, with the goal above it (the fill fell back).
pub(crate) fn template_with_goal(template: &str, goal: &str) -> String {
    let goal = goal.trim();
    if goal.is_empty() {
        template.to_string()
    } else {
        format!("{goal}\n\n{template}")
    }
}

fn fill_prompt(template: &str, goal: &str, files: &str) -> String {
    format!(
        "Fill in this repository's pull request template for the change in the current \
         directory. Keep every heading and checklist of the template; replace its \
         placeholder text and HTML comments with what this change does. Don't invent \
         anything you can't see; leave a checklist item unchecked when unsure. Answer with \
         the filled-in markdown only, no preamble, no code fence.\n\n\
         What the change is for:\n{goal}\n\n\
         Files changed:\n{files}\n\n\
         The template:\n{template}"
    )
}

/// The answer without a fence around the whole of it.
fn unfence(text: &str) -> String {
    let t = text.trim();
    if let Some(rest) = t.strip_prefix("```")
        && let Some(inner) = rest.strip_suffix("```")
    {
        // Drop the info string (`markdown`) on the opening line.
        return inner
            .split_once('\n')
            .map_or(inner, |(_, body)| body)
            .trim()
            .to_string();
    }
    t.to_string()
}

/// A filled-in template still has the template's headings, or (with
/// none) is at least half its length.
fn keeps_template(template: &str, filled: &str) -> bool {
    let headings: Vec<&str> = template
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('#'))
        .collect();
    if headings.is_empty() {
        let len = |s: &str| s.chars().filter(|c| !c.is_whitespace()).count();
        return len(filled) * 2 >= len(template) && !filled.trim().is_empty();
    }
    headings
        .iter()
        .all(|h| filled.lines().any(|l| l.trim() == *h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(state: MergeState) -> MergeInfo {
        MergeInfo {
            mergeable: Some(true),
            state,
            allowed_methods: vec![MergeMethod::Merge, MergeMethod::Squash],
            auto_merge_enabled: false,
            in_queue: false,
            required_approvals: None,
            auto_merge_allowed: false,
            merge_queue: false,
            default_method: None,
        }
    }

    #[test]
    fn the_footer_is_added_once() {
        let link = "theodore://space/abc";
        let once = with_footer("Fix it", "abc", link);
        assert_eq!(
            once,
            "Fix it\n\n<!-- theodore-space:abc -->\n<sub>From a Theodore space: theodore://space/abc</sub>"
        );
        assert_eq!(with_footer(&once, "abc", link), once);
        assert!(with_footer("", "abc", link).starts_with(FOOTER_MARKER));
    }

    #[test]
    fn continued_branches_count_on() {
        let first: Vec<_> = next_branches("ada/fix-login", false).take(2).collect();
        assert_eq!(first, ["ada/fix-login-2", "ada/fix-login-3"]);
        // A slug that ends in a number isn't a round number on round one.
        let n: Vec<_> = next_branches("ada/retry-429", false).take(1).collect();
        assert_eq!(n, ["ada/retry-429-2"]);
        let later: Vec<_> = next_branches("ada/fix-login-3", true).take(1).collect();
        assert_eq!(later, ["ada/fix-login-4"]);
    }

    #[test]
    fn the_method_is_the_asked_one_or_the_hosts_default() {
        let mut i = info(MergeState::Clean);
        assert_eq!(merge_method(None, &i).unwrap(), MergeMethod::Merge);
        i.default_method = Some(MergeMethod::Squash);
        assert_eq!(merge_method(None, &i).unwrap(), MergeMethod::Squash);
        i.default_method = Some(MergeMethod::Rebase);
        assert_eq!(
            merge_method(None, &i).unwrap(),
            MergeMethod::Merge,
            "not allowed"
        );
        assert!(merge_method(Some(MergeMethod::Rebase), &i).is_err());
        assert_eq!(
            merge_method(Some(MergeMethod::Squash), &i).unwrap(),
            MergeMethod::Squash
        );
    }

    #[test]
    fn a_pr_that_cant_merge_yet_auto_merges_when_it_can() {
        assert_eq!(merge_mode(&info(MergeState::Clean)), Ok((false, false)));
        assert_eq!(merge_mode(&info(MergeState::Unstable)), Ok((false, false)));
        assert!(merge_mode(&info(MergeState::Blocked)).is_err());
        let mut i = info(MergeState::Blocked);
        i.auto_merge_allowed = true;
        assert_eq!(merge_mode(&i), Ok((true, false)));
        i.state = MergeState::Dirty;
        assert!(merge_mode(&i).unwrap_err().contains("conflicts"));
        let mut q = info(MergeState::Clean);
        q.merge_queue = true;
        assert_eq!(merge_mode(&q), Ok((false, true)));
    }

    #[test]
    fn a_fill_must_keep_the_templates_headings() {
        let t = "## Summary\n<!-- what -->\n\n## Testing\n- [ ] tests";
        assert!(keeps_template(
            t,
            "## Summary\nRetries 429s.\n\n## Testing\n- [x] tests"
        ));
        assert!(!keeps_template(t, "ok"));
        assert_eq!(unfence("```markdown\n## Summary\nx\n```"), "## Summary\nx");
        assert!(!keeps_template("Describe the change.", ""));
    }
}
