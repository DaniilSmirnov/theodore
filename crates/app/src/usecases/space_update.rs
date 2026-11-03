//! Update from base (docs/spaces.md §6): the base branch moved on, and the
//! space takes its new commits by a merge into the worktree — never a
//! rebase or a force push, so pushed history and reviewers' comments stay
//! put. Shipping still squashes.
//!
//! ```text
//! Update ─▶ fetch ─▶ checkpoint hand edits ─▶ git merge <base tip>
//!   ├─ clean ─────▶ merge commit = checkpoint ─▶ base_sha = tip ─▶ diff vs the new base
//!   └─ conflicts ─▶ space.updating ─▶ agent turn "resolve these files"
//!                     turn ends ─▶ markers left? ─┬─ no  ─▶ Theodore commits the merge
//!                                                 └─ yes ─▶ still updating, says which
//!                   Abort ─▶ merge --abort ─▶ back to updating.from
//! ```

use std::path::Path;

use theodore_contract::{
    Space, SpaceBaseStatus, SpaceMode, SpaceReviewRequest, SpaceSendRequest, SpaceStatus,
    SpaceUpdate, models,
};
use theodore_domain::Sha;
use theodore_ports::{
    GitError, GitResult, NewAction, SpaceCheckpoint, SpaceMerge, SyncActionCode, WorktreeBusy,
};

use crate::core::Core;
use crate::error::{AppError, AppResult};

/// Conflicted files named in a message; the prompt lists more.
const NAMED: usize = 5;
const LISTED: usize = 40;

/// The end of a turn in a space (`drive_turn`): a normal checkpoint, or,
/// while an update from base is resolving, the merge's completion.
pub(crate) struct TurnCheckpoint {
    pub result: GitResult<SpaceCheckpoint>,
    /// More fields for the space's patch (`base_sha`, `updating`).
    pub patch: Option<serde_json::Value>,
    /// The base before the merge this turn completed (`SpaceTurn`).
    pub base_before: Option<Sha>,
    /// Why there is no checkpoint, when conflicts are left.
    pub skipped: Option<String>,
}

impl Core {
    /// How many commits the base has that the space doesn't, from the
    /// copy on this Mac: no network (the project's refresh fetches).
    pub async fn space_base_status(&self, space: &str) -> AppResult<SpaceBaseStatus> {
        let space = self.space_row(space).await?;
        let project = self.space_project(&space).await?;
        let git = &self.0.deps.git;
        let repo = match project.path {
            // A local folder's mirror fetches from disk.
            Some(_) => self.ensure_project_cache(&project).await?,
            None => git
                .cached_repo(&project.repo)
                .await?
                .ok_or_else(|| AppError::NotFound(format!("a copy of {}", project.repo.slug())))?,
        };
        let base_tip = git.resolve_ref(&repo, &space.base).await?;
        let behind = if base_tip == space.base_sha {
            0
        } else {
            git.commit_log(&repo, &space.base_sha, &base_tip)
                .await?
                .len() as u32
        };
        Ok(SpaceBaseStatus { base_tip, behind })
    }

    /// Update from base: merges the base's tip into the space. Clean, the
    /// merge is the new checkpoint; on conflicts the agent gets a turn to
    /// resolve them. While one is resolving, this completes it instead
    /// (the files were fixed by hand).
    pub async fn update_space_from_base(&self, space: &str) -> AppResult<Space> {
        // Not while a ship reads the checkpoint, or an Undo moves it.
        let _guard = self.0.ship_lock.lock().await;
        let space = self.space_row(space).await?;
        match space.status {
            SpaceStatus::Running => {
                return Err(AppError::Invalid(
                    "the agent is still working; wait for the turn to end or stop it".into(),
                ));
            }
            SpaceStatus::Archived => {
                return Err(AppError::Invalid("this space is archived".into()));
            }
            SpaceStatus::Idle | SpaceStatus::Failed => {}
        }
        if space.pr.is_some() && space.done_at.is_some() {
            // The merged branch is finished: Continue starts the next one
            // on the base's tip (space_pr.rs).
            return Err(AppError::Invalid(
                "its PR merged; Continue moves it onto the new base".into(),
            ));
        }
        if space.setup.as_ref().is_some_and(|s| {
            matches!(
                s.status,
                theodore_contract::SetupStatus::Running | theodore_contract::SetupStatus::Waiting
            )
        }) {
            return Err(AppError::Invalid("the space is still setting up".into()));
        }
        let project = self.space_project(&space).await?;
        // Fetches the base (best effort offline: the copy on this Mac).
        let repo_path = self.ensure_project_cache(&project).await?;
        let git = &self.0.deps.git;
        let wt = git
            .space_worktree(&repo_path, &space.id, &space.base_sha)
            .await?;
        if let Some(update) = &space.updating {
            return self.complete_update(&space, update, &wt.path).await;
        }
        let tip = git.resolve_ref(&repo_path, &space.base).await?;
        if tip == space.base_sha {
            return Err(AppError::Invalid(format!(
                "already up to date with {}",
                space.base
            )));
        }
        if let Some(busy) = git.worktree_busy(&wt.path).await? {
            return Err(AppError::Invalid(format!(
                "can't update: {busy} in the space's worktree; finish or abort it first"
            )));
        }
        // Hand edits since the last turn get a checkpoint of their own: the
        // merge wants a clean tree, and Abort comes back to it.
        // The merge then makes it `HEAD` itself (`space_merge`).
        let from = git
            .space_checkpoint(
                &wt.path,
                &space.id,
                &format!("{}: edits before updating from {}", space.name, space.base),
            )
            .await?
            .sha;
        let now = self.0.deps.clock.now();
        match git
            .space_merge(&wt.path, &space.id, &tip, &merge_message(&space))
            .await?
        {
            SpaceMerge::Merged(head) => {
                self.apply(vec![patch(
                    &space.id,
                    serde_json::json!({
                        "head_sha": head,
                        // A merge commit: the checkpoint is `HEAD`.
                        "worktree_head": null,
                        "base_sha": tip,
                        "status": SpaceStatus::Idle,
                        "error": null,
                        "updated_at": now,
                    }),
                )])
                .await?;
                self.refresh_space_diff(&space.id).await?;
            }
            SpaceMerge::Conflicts(conflicts) => {
                let prompt =
                    conflict_prompt(&space.base, &tip, &conflicts, self.0.deps.tools.is_some());
                self.apply(vec![patch(
                    &space.id,
                    serde_json::json!({
                        "head_sha": from,
                        "worktree_head": null,
                        "updating": SpaceUpdate {
                            base_tip: tip,
                            from: from.clone(),
                            conflicts,
                            started_at: now,
                        },
                        "updated_at": now,
                    }),
                )])
                .await?;
                if from != space.head_sha {
                    self.refresh_space_diff(&space.id).await?;
                }
                // Resolving means editing: an Ask space edits for this turn
                // on (the mode is sticky, as for any send).
                self.space_send(SpaceSendRequest {
                    space: space.id.clone(),
                    text: prompt,
                    agent: None,
                    model: None,
                    mode: (space.mode == SpaceMode::Ask).then_some(SpaceMode::Edit),
                    attachments: None,
                })
                .await?;
            }
            SpaceMerge::NotMerging => {
                return Err(AppError::Invalid(format!(
                    "the merge of {} didn't start",
                    space.base
                )));
            }
        }
        self.space_row(&space.id).await
    }

    /// Abort the update from base: the merge is thrown away, with what was
    /// resolved so far, and the space is back at the checkpoint it started
    /// from, on its old base.
    pub async fn abort_space_update(&self, space: &str) -> AppResult<Space> {
        let _guard = self.0.ship_lock.lock().await;
        let space = self.space_row(space).await?;
        if space.status == SpaceStatus::Running {
            return Err(AppError::Invalid(
                "the agent is still working; stop it before aborting the update".into(),
            ));
        }
        let Some(update) = space.updating.clone() else {
            return Err(AppError::Invalid("no update from base in progress".into()));
        };
        let project = self.space_project(&space).await?;
        let repo_path = self.ensure_project_cache(&project).await?;
        let git = &self.0.deps.git;
        let wt = git
            .space_worktree(&repo_path, &space.id, &space.base_sha)
            .await?;
        git.space_merge_abort(&wt.path, &space.id, &update.from)
            .await
            .map_err(|e| match e {
                GitError::Busy(busy) => AppError::Invalid(format!(
                    "can't abort: {busy} in the space's worktree; finish or abort it first"
                )),
                e => e.into(),
            })?;
        self.apply(vec![patch(
            &space.id,
            serde_json::json!({
                "updating": null,
                "head_sha": update.from,
                // Abort resets hard to it: `HEAD` is the checkpoint.
                "worktree_head": null,
                "status": SpaceStatus::Idle,
                "error": null,
                // Its session resolved files that are gone now.
                "agent_session": null,
                "updated_at": self.0.deps.clock.now(),
            }),
        )])
        .await?;
        if update.from != space.head_sha {
            self.refresh_space_diff(&space.id).await?;
        }
        self.space_row(&space.id).await
    }

    /// "Complete" (Update while resolving): commits the merge if no
    /// conflicts are left in the worktree.
    async fn complete_update(
        &self,
        space: &Space,
        update: &SpaceUpdate,
        worktree: &Path,
    ) -> AppResult<Space> {
        let (finish, extra) = self.finish_update(space, update, worktree).await?;
        let mut data = serde_json::json!({ "updated_at": self.0.deps.clock.now() });
        merge_fields(&mut data, extra);
        let left = match &finish {
            Finish::Left(paths) => Some(paths.clone()),
            Finish::Merged(cp) | Finish::Abandoned(cp) => {
                data["head_sha"] = serde_json::json!(cp.sha);
                data["worktree_head"] = serde_json::json!(cp.head);
                data["status"] = serde_json::json!(SpaceStatus::Idle);
                data["error"] = serde_json::Value::Null;
                None
            }
        };
        self.apply(vec![patch(&space.id, data)]).await?;
        if let Some(paths) = left {
            return Err(AppError::Invalid(conflicts_left(&paths)));
        }
        self.refresh_space_diff(&space.id).await?;
        self.space_row(&space.id).await
    }

    /// Tries to commit the merge in progress; the patch says what the
    /// space becomes (`base_sha`, `updating`), `head_sha` aside.
    async fn finish_update(
        &self,
        space: &Space,
        update: &SpaceUpdate,
        worktree: &Path,
    ) -> GitResult<(Finish, serde_json::Value)> {
        let git = &self.0.deps.git;
        match git
            .space_merge_finish(worktree, &space.id, &update.base_tip, &merge_message(space))
            .await?
        {
            SpaceMerge::Merged(head) => Ok((
                Finish::Merged(SpaceCheckpoint {
                    sha: head.clone(),
                    head,
                }),
                serde_json::json!({ "base_sha": update.base_tip, "updating": null }),
            )),
            SpaceMerge::Conflicts(conflicts) => Ok((
                Finish::Left(conflicts.clone()),
                serde_json::json!({
                    "updating": SpaceUpdate { conflicts, ..update.clone() },
                }),
            )),
            // Aborted in the worktree: the space stays on its old base,
            // with whatever else was edited.
            SpaceMerge::NotMerging => {
                let head = git
                    .space_checkpoint(worktree, &space.id, &format!("{}: edits", space.name))
                    .await?;
                Ok((
                    Finish::Abandoned(head),
                    serde_json::json!({ "updating": null }),
                ))
            }
        }
    }

    async fn refresh_space_diff(&self, space: &str) -> AppResult<()> {
        self.space_review(SpaceReviewRequest {
            space: space.to_string(),
            group: false,
        })
        .await
        .map(|_| ())
    }
}

enum Finish {
    /// The merge commit: the checkpoint and `HEAD` both.
    Merged(SpaceCheckpoint),
    Left(Vec<String>),
    Abandoned(SpaceCheckpoint),
}

/// The checkpoint at the end of a turn in `cwd` (docs/spaces.md §6): while
/// an update from base is resolving, Theodore — not the agent — commits
/// the merge once no conflicts are left; otherwise a normal checkpoint.
pub(crate) async fn turn_checkpoint(
    core: &Core,
    space: &Space,
    cwd: &Path,
    message: &str,
) -> TurnCheckpoint {
    let Some(update) = &space.updating else {
        return TurnCheckpoint {
            result: core
                .0
                .deps
                .git
                .space_checkpoint(cwd, &space.id, message)
                .await,
            patch: None,
            base_before: None,
            skipped: None,
        };
    };
    match core.finish_update(space, update, cwd).await {
        Ok((Finish::Merged(head), patch)) => TurnCheckpoint {
            result: Ok(head),
            patch: Some(patch),
            base_before: Some(space.base_sha.clone()),
            skipped: None,
        },
        Ok((Finish::Abandoned(head), patch)) => TurnCheckpoint {
            result: Ok(head),
            patch: Some(patch),
            base_before: None,
            skipped: None,
        },
        Ok((Finish::Left(paths), patch)) => TurnCheckpoint {
            result: Err(GitError::Busy(WorktreeBusy::Conflicts)),
            patch: Some(patch),
            base_before: None,
            skipped: Some(conflicts_left(&paths)),
        },
        Err(e) => TurnCheckpoint {
            result: Err(e),
            patch: None,
            base_before: None,
            skipped: None,
        },
    }
}

/// Copies `extra`'s fields into the object `into`.
pub(crate) fn merge_fields(into: &mut serde_json::Value, extra: serde_json::Value) {
    if let (Some(into), serde_json::Value::Object(extra)) = (into.as_object_mut(), extra) {
        into.extend(extra);
    }
}

fn patch(space: &str, data: serde_json::Value) -> NewAction {
    NewAction {
        model: models::SPACE.into(),
        model_id: space.to_string(),
        action: SyncActionCode::C,
        data: Some(data),
    }
}

fn merge_message(space: &Space) -> String {
    format!("{}: merge {}", space.name, space.base)
}

fn short(sha: &Sha) -> &str {
    &sha.as_str()[..sha.as_str().len().min(7)]
}

/// The space's error while conflicts are left.
fn conflicts_left(paths: &[String]) -> String {
    let mut named = paths
        .iter()
        .take(NAMED)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > NAMED {
        named.push_str(&format!(" and {} more", paths.len() - NAMED));
    }
    format!("conflicts left in {named}; send the agent another message, or abort the update")
}

/// The turn that asks the agent to resolve the merge's conflicts.
pub(crate) fn conflict_prompt(base: &str, tip: &Sha, paths: &[String], tools: bool) -> String {
    let mut list: String = paths
        .iter()
        .take(LISTED)
        .map(|p| format!("- {p}\n"))
        .collect();
    if paths.len() > LISTED {
        list.push_str(&format!(
            "- and {} more (`git diff --name-only --diff-filter=U`)\n",
            paths.len() - LISTED
        ));
    }
    let check = if tools {
        "- Then run the project's check with run_check and fix what the merge broke."
    } else {
        "- Then build or test what you can and fix what the merge broke."
    };
    format!(
        "Theodore is bringing this space up to date with {base}: it merged {base} ({tip}) \
         into the worktree, and git stopped on conflicts in {n}:\n\
         {list}\n\
         Resolve them:\n\
         - In each file, replace every conflict block with the code it should be. Between \
         <<<<<<< and ||||||| is this space's version, between ||||||| and ======= what both \
         started from, between ======= and >>>>>>> {base}'s version.\n\
         - Keep both intents: what this space set out to do, and what changed on {base}. \
         Where they can't both hold, keep {base}'s behavior, rework this space's change on \
         top of it, and say so.\n\
         - Look past the markers: code {base} moved or renamed may need this space's change \
         too.\n\
         - Leave git to Theodore: don't commit, abort, rebase or reset. Theodore commits the \
         merge once no conflict markers are left.\n\
         {check}\n\n\
         End with what you resolved and how.",
        tip = short(tip),
        n = if paths.len() == 1 {
            "1 file".to_string()
        } else {
            format!("{} files", paths.len())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_conflict_prompt_lists_the_files_and_names_the_base() {
        let p = conflict_prompt(
            "main",
            &Sha::new("abcdef1234"),
            &["src/a.rs".into(), "b.txt".into()],
            true,
        );
        assert!(
            p.contains("it merged main (abcdef1) into the worktree"),
            "{p}"
        );
        assert!(
            p.contains("conflicts in 2 files:\n- src/a.rs\n- b.txt\n"),
            "{p}"
        );
        assert!(p.contains("Keep both intents"));
        assert!(p.contains("run_check"));
        assert!(p.contains("don't commit, abort, rebase or reset"));
        let one = conflict_prompt("dev", &Sha::new("abc"), &["x".into()], false);
        assert!(one.contains("in 1 file:"));
        assert!(!one.contains("run_check"));
    }

    #[test]
    fn a_long_conflict_list_is_cut() {
        let many: Vec<String> = (0..50).map(|i| format!("f{i}")).collect();
        let p = conflict_prompt("main", &Sha::new("abc"), &many, false);
        assert!(p.contains("- f39\n- and 10 more"));
        assert_eq!(
            conflicts_left(&many[..7]),
            "conflicts left in f0, f1, f2, f3, f4 and 2 more; send the agent another \
             message, or abort the update"
        );
    }
}
