//! Shipping a space (docs/spaces.md §1, §6, §12): its checkpoints squashed
//! into one commit authored by the user, then
//!
//! ```text
//! host project   Open PR ─▶ push <login>/<slug> ─▶ outbox create_space_pr ─▶ space.pr
//!                Push    ─▶ one more commit on the PR branch (never forced)
//! local folder   Merge locally ─▶ git merge into the checked-out branch
//!                Keep as branch ─▶ refs/heads/<branch> in the folder, nothing else
//! ```
//!
//! and, once the change is done (PR merged, merged locally, kept as a
//! branch), archived after `spaces.auto_archive_days` — unless a merged
//! PR's space continues on a new branch (`space_pr.rs`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use theodore_contract::{
    Ephemeral, Project, ShipAction, ShipKind, ShipSpaceRequest, Space, SpaceShipped, SpaceStatus,
    ToastLevel, models,
};
use theodore_domain::{Author, HostKind, PrRef, PrState, PrSummary, RepoRef, Sha};
use theodore_ports::{GitError, Identity, NewAction, NewPr, SpaceCommit, SyncActionCode};

use crate::core::Core;
use crate::error::{AppError, AppResult};

pub(crate) const AUTO_ARCHIVE_KEY: &str = "spaces.auto_archive_days";
pub(crate) const DEFAULT_AUTO_ARCHIVE_DAYS: f64 = 7.0;
const DAY_MS: f64 = 86_400_000.0;
/// Subject line limit for the squashed commit / PR title.
const TITLE_CHARS: usize = 72;

/// Outbox payload of `create_space_pr`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSpacePrPayload {
    pub space: String,
    pub repo: RepoRef,
    pub pr: NewPr,
}

impl Core {
    /// Open PR / Push / Merge locally / Keep as branch. Returns the space
    /// as it is now; an opened PR lands on `space.pr` once the outbox item
    /// runs.
    pub async fn ship_space(&self, req: ShipSpaceRequest) -> AppResult<Space> {
        self.ship_space_as(req, None).await
    }

    /// `ship_space` asked for by the agent of the turn `turn` (the `ship`
    /// tool): the space is running that very turn, which doesn't block it.
    pub(crate) async fn ship_space_as(
        &self,
        req: ShipSpaceRequest,
        turn: Option<&str>,
    ) -> AppResult<Space> {
        let how = crate::usecases::telemetry::wire_str(&req.action);
        let result = self.ship_space_inner(req, turn).await;
        match &result {
            Ok(_) => {
                theodore_telemetry::product(theodore_telemetry::ProductEvent::SpaceShipped { how })
            }
            Err(e) if !e.is_expected() => tracing::warn!(
                error.code = e.code(),
                how,
                error = %e,
                "shipping a space failed"
            ),
            Err(_) => {}
        }
        result
    }

    async fn ship_space_inner(
        &self,
        req: ShipSpaceRequest,
        turn: Option<&str>,
    ) -> AppResult<Space> {
        let _guard = self.0.ship_lock.lock().await;
        let space = self.space_row(&req.space).await?;
        let own_turn = turn.is_some() && space.run.as_deref() == turn;
        match space.status {
            SpaceStatus::Running if own_turn => {}
            SpaceStatus::Running => {
                return Err(AppError::Invalid(
                    "the agent is still working; stop it or wait for the turn to end".into(),
                ));
            }
            SpaceStatus::Archived => {
                return Err(AppError::Invalid("this space is archived".into()));
            }
            SpaceStatus::Idle | SpaceStatus::Failed => {}
        }
        if space.updating.is_some() {
            return Err(AppError::Invalid(
                "finish or abort the update from base first; nothing half-merged ships".into(),
            ));
        }
        let project = self
            .project_row(&space.project)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("project {}", space.project)))?;
        let folder = project.path.clone().map(PathBuf::from);
        match (req.action, &folder) {
            (ShipAction::OpenPr | ShipAction::OpenDraftPr | ShipAction::Push, Some(_)) => {
                return Err(AppError::Invalid(
                    "a local folder has no host to open a PR on; merge locally or keep it as a branch"
                        .into(),
                ));
            }
            (ShipAction::MergeLocally | ShipAction::KeepBranch, None) => {
                return Err(AppError::Invalid(
                    "a host project ships as a pull request".into(),
                ));
            }
            _ => {}
        }

        let repo_path = self.ensure_project_cache(&project).await?;
        // Edits made by hand in the worktree since the last turn ship too.
        // A checkpoint is a snapshot (docs/spaces.md §6): its tree is what
        // ships, whatever the agent committed itself or left uncommitted.
        let mut worktree_head = space.worktree_head.clone();
        let head = if Path::new(&space.worktree).exists() {
            let saved = self
                .0
                .deps
                .git
                .space_checkpoint(
                    Path::new(&space.worktree),
                    &space.id,
                    &format!("{}: edits before shipping", space.name),
                )
                .await;
            match saved {
                Ok(cp) => {
                    worktree_head = Some(cp.head);
                    cp.sha
                }
                // Its `.git` is gone: the last checkpoint is what ships.
                Err(GitError::NotAWorktree(_)) => space.head_sha.clone(),
                // A rebase or conflict mid-way: nothing half-done ships.
                Err(e) => return Err(e.into()),
            }
        } else {
            space.head_sha.clone()
        };
        if head != space.head_sha {
            self.apply(vec![self.patch(
                &space.id,
                serde_json::json!({ "head_sha": head, "worktree_head": worktree_head }),
            )])
            .await?;
        }
        if head == space.base_sha {
            return Err(AppError::Invalid(
                "nothing to ship yet: the space has no changes".into(),
            ));
        }
        let author = self.ship_author(&repo_path, &project).await?;

        match req.action {
            ShipAction::OpenPr | ShipAction::OpenDraftPr => {
                if space.pr.is_some() {
                    return Err(AppError::Invalid(
                        "this space already has a PR; Push sends new turns to it".into(),
                    ));
                }
                let title = title_or(req.title.as_deref(), &space);
                let message = commit_message(&title, &space.goal);
                let shipped = self
                    .push_squashed(&space, &project, &repo_path, &head, &message, &author)
                    .await?;
                // The repo's PR template when it has one, and a link back.
                let body = self
                    .space_pr_body(&space, &repo_path, &head, pr_body(&space.goal))
                    .await;
                let body = with_closes(body, space.issue);
                let payload = CreateSpacePrPayload {
                    space: space.id.clone(),
                    repo: project.repo.clone(),
                    pr: NewPr {
                        head: space.branch.clone(),
                        base: space.base.clone(),
                        title,
                        body,
                        draft: req.action == ShipAction::OpenDraftPr,
                    },
                };
                self.apply_with_outbox(
                    vec![self.patch(&space.id, serde_json::json!({ "shipped": shipped }))],
                    "create_space_pr",
                    to_json(&payload)?,
                )
                .await?;
            }
            ShipAction::Push => {
                let Some(pr) = space.pr.clone() else {
                    return Err(AppError::Invalid("open a PR before pushing".into()));
                };
                if space.shipped.as_ref().is_some_and(|s| s.head == head) {
                    return Err(AppError::Invalid("nothing new to push".into()));
                }
                let title = match req.title.as_deref().map(str::trim) {
                    Some(t) if !t.is_empty() => t.to_string(),
                    _ => self.push_title(&space).await,
                };
                let shipped = self
                    .push_squashed(&space, &project, &repo_path, &head, &title, &author)
                    .await?;
                self.apply(vec![
                    self.patch(&space.id, serde_json::json!({ "shipped": shipped })),
                ])
                .await?;
                if let Some(pr_ref) = self.pr_ref(&pr).await {
                    self.refresh_pr(&pr, &pr_ref).await;
                }
            }
            ShipAction::MergeLocally | ShipAction::KeepBranch => {
                let folder = folder.expect("checked above");
                let title = title_or(req.title.as_deref(), &space);
                self.ship_locally(
                    &space,
                    &folder,
                    &repo_path,
                    &head,
                    &commit_message(&title, &space.goal),
                    &author,
                    req.action,
                )
                .await?;
            }
        }
        self.space_row(&space.id).await
    }

    /// The squashed commit pushed to `space.branch`, never forced:
    ///
    /// | last ship | remote branch | new commit |
    /// | --- | --- | --- |
    /// | none | absent | `base` + head's tree |
    /// | none | exists | refused: the name is taken |
    /// | `s` | at `s.sha` or absent | `s.sha` + head's tree |
    /// | `s` | moved to `t` | `t` + the turns since `s.head` (3-way) |
    ///
    /// Nothing new since `s` (a retried Open PR) pushes nothing.
    ///
    /// A space adopted from an existing branch or PR (`Space::fork_sha`,
    /// docs/spaces.md §3 "New space from an issue/branch/PR") never has a
    /// `none`: its fork point stands in for `s` from the very first ship,
    /// so the squash lands on top of it (or 3-ways past it, if the branch
    /// moved on since) instead of being refused as "already exists".
    async fn push_squashed(
        &self,
        space: &Space,
        project: &Project,
        repo_path: &Path,
        head: &Sha,
        message: &str,
        author: &Identity,
    ) -> AppResult<SpaceShipped> {
        let git = &self.0.deps.git;
        let tip = git
            .remote_branch(repo_path, &project.repo, &space.branch)
            .await?;
        let synthetic = space.fork_sha.as_ref().map(|fork_sha| SpaceShipped {
            how: ShipKind::Pr,
            branch: space.branch.clone(),
            sha: fork_sha.clone(),
            head: fork_sha.clone(),
            pushes: 0,
            at: space.created_at,
        });
        let last = space
            .shipped
            .as_ref()
            .filter(|s| s.how == ShipKind::Pr)
            .or(synthetic.as_ref());
        let commit = |parent: &Sha, replay_from: Option<Sha>| SpaceCommit {
            head: head.clone(),
            parent: parent.clone(),
            replay_from,
            merge_parent: merge_parent(parent, space),
            message: message.to_string(),
            author: author.clone(),
        };
        let (sha, push) = match (last, &tip) {
            (None, Some(_)) => {
                return Err(AppError::Invalid(format!(
                    "a branch named {} already exists on the host; nothing was pushed",
                    space.branch
                )));
            }
            (None, None) => (
                git.space_commit(repo_path, &space.id, &commit(&space.base_sha, None))
                    .await?,
                true,
            ),
            (Some(s), tip) if &s.head == head => (s.sha.clone(), tip.as_ref() != Some(&s.sha)),
            (Some(s), None) => (
                git.space_commit(repo_path, &space.id, &commit(&s.sha, None))
                    .await?,
                true,
            ),
            (Some(s), Some(t)) if t == &s.sha => (
                git.space_commit(repo_path, &space.id, &commit(&s.sha, None))
                    .await?,
                true,
            ),
            (Some(s), Some(t)) => (
                git.space_commit(repo_path, &space.id, &commit(t, Some(s.head.clone())))
                    .await
                    .map_err(refused)?,
                true,
            ),
        };
        if push {
            git.push_branch(repo_path, &project.repo, &sha, &space.branch)
                .await
                .map_err(refused)?;
        }
        Ok(SpaceShipped {
            how: ShipKind::Pr,
            branch: space.branch.clone(),
            sha,
            head: head.clone(),
            pushes: last.map_or(0, |s| s.pushes) + u32::from(push),
            at: self.0.deps.clock.now(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn ship_locally(
        &self,
        space: &Space,
        folder: &Path,
        repo_path: &Path,
        head: &Sha,
        message: &str,
        author: &Identity,
        action: ShipAction,
    ) -> AppResult<SpaceShipped> {
        let git = &self.0.deps.git;
        let last = space.shipped.as_ref();
        if last.is_some_and(|s| &s.head == head) {
            return Err(AppError::Invalid(
                "this space was already shipped; there are no new turns".into(),
            ));
        }
        // After an earlier ship the next commit builds on it, so a second
        // merge only brings the turns since.
        let parent = last.map_or(&space.base_sha, |s| &s.sha);
        let sha = git
            .space_commit(
                repo_path,
                &space.id,
                &SpaceCommit {
                    head: head.clone(),
                    parent: parent.clone(),
                    replay_from: None,
                    merge_parent: merge_parent(parent, space),
                    message: message.to_string(),
                    author: author.clone(),
                },
            )
            .await?;
        let (how, branch) = if action == ShipAction::MergeLocally {
            git.merge_into_folder(
                folder,
                repo_path,
                &space.id,
                &format!("Merge space {}", space.name),
            )
            .await
            .map_err(refused)?;
            let branch = git
                .working_state(folder)
                .await
                .ok()
                .and_then(|w| w.current_branch)
                .unwrap_or_default();
            (ShipKind::MergedLocally, branch)
        } else {
            git.write_folder_branch(folder, repo_path, &space.id, &space.branch)
                .await
                .map_err(refused)?;
            (ShipKind::Branch, space.branch.clone())
        };
        let now = self.0.deps.clock.now();
        let shipped = SpaceShipped {
            how,
            branch,
            sha,
            head: head.clone(),
            pushes: last.map_or(0, |s| s.pushes) + 1,
            at: now,
        };
        self.apply(vec![self.patch(
            &space.id,
            serde_json::json!({ "shipped": shipped, "done_at": now }),
        )])
        .await?;
        Ok(shipped)
    }

    /// The user's git identity; for GitHub, failing that, the signed-in
    /// account with its noreply address.
    pub(crate) async fn ship_author(
        &self,
        repo_path: &Path,
        project: &Project,
    ) -> AppResult<Identity> {
        // A local folder's own config (a per-repo identity) counts first.
        let config_from = project.path.as_deref().map_or(repo_path, Path::new);
        if let Some(id) = self.0.deps.git.user_identity(config_from).await? {
            return Ok(id);
        }
        if project.repo.host == HostKind::Github
            && let Ok(me) = self.0.deps.host.viewer_for(HostKind::Github).await
        {
            return Ok(Identity {
                name: me.name.clone().unwrap_or_else(|| me.login.clone()),
                email: format!("{}@users.noreply.github.com", me.login),
            });
        }
        Err(AppError::Invalid(
            "set git's user.name and user.email so the commit can be authored by you".into(),
        ))
    }

    /// The default Push message: your latest message since the last push.
    async fn push_title(&self, space: &Space) -> String {
        let since = space.shipped.as_ref().map_or(0, |s| s.at);
        let store = self.0.deps.store.clone();
        let key = crate::usecases::spaces::space_chat_key(&space.id).0;
        let rows =
            tokio::task::spawn_blocking(move || store.query(models::CHAT_MESSAGE, "pr", &key))
                .await
                .ok()
                .and_then(|r| r.ok())
                .unwrap_or_default();
        let latest = rows
            .into_iter()
            .filter_map(|r| serde_json::from_value::<theodore_contract::ChatMessage>(r.data).ok())
            .filter(|m| m.author == Author::Me && m.at >= since)
            .max_by_key(|m| m.at);
        latest
            .map(|m| first_line(&m.text))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| format!("Update {}", space.name))
    }

    /// The outbox opened (or found) the space's PR: link it and fetch it so
    /// it shows in Pull requests and the Inbox right away.
    pub(crate) async fn space_pr_opened(&self, space: &str, pr_ref: &PrRef) {
        let pr = pr_ref.id();
        let _ = self
            .apply(vec![self.patch(
                space,
                serde_json::json!({ "pr": pr, "updated_at": self.0.deps.clock.now() }),
            )])
            .await;
        self.refresh_pr(&pr, pr_ref).await;
        self.emit(Ephemeral::Toast {
            level: ToastLevel::Info,
            text: format!("Opened PR #{}", pr_ref.number),
        });
    }

    /// Marks spaces done once their PR merged, and archives done spaces
    /// after `spaces.auto_archive_days` (0 = never). Run by the poller.
    pub(crate) async fn sweep_spaces(&self) {
        let Ok(spaces) = self.list_spaces(None).await else {
            return;
        };
        let days = self
            .setting_f64_scoped(AUTO_ARCHIVE_KEY, None)
            .await
            .unwrap_or(DEFAULT_AUTO_ARCHIVE_DAYS);
        let now = self.0.deps.clock.now();
        for space in spaces {
            if matches!(space.status, SpaceStatus::Archived | SpaceStatus::Running) {
                continue;
            }
            let mut done_at = space.done_at;
            if done_at.is_none()
                && let Some(pr) = &space.pr
            {
                // Offline, only the host's answer would tell; the poller's
                // probe notices it's back, and the next sweep asks then.
                if let Some(pr_ref) = self.space_pr_ref(&space, pr).await
                    && (self.host_online() || pr_ref.repo.host == HostKind::Local)
                    && self.pr_state(pr).await != Some(PrState::Merged)
                {
                    self.refresh_pr(pr, &pr_ref).await;
                }
                // Not if Continue moved the space to a new branch meanwhile.
                if self.pr_state(pr).await == Some(PrState::Merged)
                    && self.mark_space_done(&space.id, pr, now).await
                {
                    done_at = Some(now);
                }
            }
            if let Some(done) = done_at
                && days > 0.0
                && (now - done) as f64 >= days * DAY_MS
                && let Err(e) = self.archive_space(&space.id).await
            {
                self.emit(Ephemeral::Toast {
                    level: ToastLevel::Warn,
                    text: format!("{}: auto-archive failed: {e}", space.name),
                });
            }
        }
    }

    /// The host ref of a space's PR: the stored row's, else rebuilt from
    /// the project and the number (the first fetch after opening it may
    /// have failed).
    async fn space_pr_ref(&self, space: &Space, pr: &theodore_domain::PrId) -> Option<PrRef> {
        if let Some(r) = self.pr_ref(pr).await {
            return Some(r);
        }
        let number = pr.0.rsplit_once('#')?.1.parse().ok()?;
        let project = self.project_row(&space.project).await.ok()??;
        Some(PrRef {
            repo: project.repo,
            number,
            host_id: None,
        })
    }

    pub(crate) async fn pr_state(&self, pr: &theodore_domain::PrId) -> Option<PrState> {
        let data = self.get_row(models::PR, &pr.0).await.ok()??;
        serde_json::from_value::<PrSummary>(data)
            .ok()
            .map(|s| s.state)
    }

    fn patch(&self, space: &str, data: serde_json::Value) -> NewAction {
        NewAction {
            model: models::SPACE.into(),
            model_id: space.to_string(),
            action: SyncActionCode::C,
            data: Some(data),
        }
    }
}

/// After the first ship, the space's base (moved on by Update from base)
/// as the new commit's second parent: the git port leaves it out when the
/// shipped history already has it.
fn merge_parent(parent: &Sha, space: &Space) -> Option<Sha> {
    (parent != &space.base_sha).then(|| space.base_sha.clone())
}

/// A safety refusal reads as the user's mistake to fix, not a git failure.
fn refused(e: GitError) -> AppError {
    match e {
        GitError::Refused(m) => AppError::Invalid(m),
        other => AppError::Git(other),
    }
}

fn to_json<T: Serialize>(v: &T) -> AppResult<serde_json::Value> {
    serde_json::to_value(v).map_err(|e| AppError::Invalid(e.to_string()))
}

/// `<login>/<slug>` (docs/spaces.md §12), `space/<slug>` without a usable
/// login. Logins keep `[A-Za-z0-9._-]`; anything else becomes `-`.
pub(crate) fn branch_for(login: Option<&str>, slug: &str) -> String {
    let branch = match login_segment(login) {
        Some(l) => format!("{l}/{slug}"),
        None => format!("space/{slug}"),
    };
    if crate::usecases::spaces::is_valid_branch(&branch) {
        branch
    } else {
        format!("space/{slug}")
    }
}

/// A login as a branch name segment: `[A-Za-z0-9._-]` kept, anything else
/// `-`; `None` when nothing usable is left.
pub(crate) fn login_segment(login: Option<&str>) -> Option<String> {
    login
        .map(|l| {
            l.chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                        c
                    } else {
                        '-'
                    }
                })
                .collect::<String>()
                .trim_matches(['-', '.'])
                .to_string()
        })
        .filter(|l| !l.is_empty())
}

/// First non-empty line, trimmed, cut to a subject line's length.
fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() <= TITLE_CHARS {
        return line.to_string();
    }
    let cut: String = line.chars().take(TITLE_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

/// An edited title, else the goal's first line, else the space's name.
fn title_or(edited: Option<&str>, space: &Space) -> String {
    let edited = edited.map(first_line).filter(|t| !t.is_empty());
    edited
        .or_else(|| Some(first_line(&space.goal)).filter(|t| !t.is_empty()))
        .unwrap_or_else(|| space.name.clone())
}

/// Title, then the whole goal when it says more than the title does.
fn commit_message(title: &str, goal: &str) -> String {
    let body = pr_body(goal);
    if body.is_empty() || body == title {
        title.to_string()
    } else {
        format!("{title}\n\n{body}")
    }
}

fn pr_body(goal: &str) -> String {
    let goal = goal.trim();
    if goal.lines().filter(|l| !l.trim().is_empty()).count() <= 1
        && goal.chars().count() <= TITLE_CHARS
    {
        String::new()
    } else {
        goal.to_string()
    }
}

/// The PR body, plus "Closes #<n>" when the space started from a host issue
/// (`Space::issue`, docs/spaces.md §3 "New space from an issue"): the host
/// closes it itself once the PR merges, same as typing it by hand.
fn with_closes(body: String, issue: Option<u64>) -> String {
    match issue {
        None => body,
        Some(n) if body.is_empty() => format!("Closes #{n}"),
        Some(n) => format!("{body}\n\nCloses #{n}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branches_are_named_after_the_login() {
        assert_eq!(branch_for(Some("ada"), "fix-login"), "ada/fix-login");
        assert_eq!(branch_for(Some("Ada L."), "x"), "Ada-L/x");
        assert_eq!(branch_for(None, "fix-login"), "space/fix-login");
        assert_eq!(branch_for(Some("@@"), "x"), "space/x");
    }

    #[test]
    fn titles_come_from_the_first_line() {
        assert_eq!(first_line("\n  Retry on 429s  \nmore"), "Retry on 429s");
        let long = "a".repeat(100);
        let t = first_line(&long);
        assert_eq!(t.chars().count(), TITLE_CHARS);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn a_short_goal_is_the_whole_message() {
        assert_eq!(commit_message("Retry", "Retry"), "Retry");
        assert_eq!(pr_body("Retry"), "");
        assert_eq!(
            commit_message("Retry", "Retry\nwith backoff"),
            "Retry\n\nRetry\nwith backoff"
        );
    }

    #[test]
    fn closes_is_appended_only_when_the_space_has_an_issue() {
        assert_eq!(with_closes(String::new(), None), "");
        assert_eq!(with_closes(String::new(), Some(42)), "Closes #42");
        assert_eq!(
            with_closes("Retry\nwith backoff".into(), Some(42)),
            "Retry\nwith backoff\n\nCloses #42"
        );
    }
}
