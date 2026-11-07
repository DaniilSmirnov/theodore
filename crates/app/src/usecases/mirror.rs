//! "Try in my folder": mirroring a space into a local folder project's own
//! working tree (docs/spaces.md "Mirror into a local folder"), for a
//! project that can't run from a separate worktree — fixed ports, one
//! local database, a docker compose stack, an IDE run config bound to the
//! folder's own path.
//!
//! ```text
//! Mirror on  ─▶ folder clean ─▶ remember its HEAD + branch
//!            ─▶ write the space's current tree into the folder (only changed files)
//! turn ends / "Sync now" ─▶ re-sync: only the paths that changed are rewritten
//! Mirror off ─▶ restore the folder to the remembered HEAD exactly (only changed files)
//!            ─▶ refuse if the folder was edited meanwhile (show which), unless Discard
//! ```
//!
//! At most one space of a project mirrors into its folder at a time:
//! turning mirroring on for another space first turns it off for whichever
//! one had it (the same refusal applies — an edited folder blocks the
//! switch too, until it's resolved there). The folder's branch, `HEAD` and
//! index are never touched, only its working tree files, and only the
//! paths a sync actually changed (`crates/git/src/mirror.rs`). Edits made
//! in the folder are never synced back into the space.
//!
//! No file watcher: mirroring re-syncs after every turn (a turn is
//! already the space's unit of change) and whenever "Sync now" is
//! clicked — simpler than watching the worktree, and just as current in
//! practice.

use std::path::{Path, PathBuf};

use theodore_contract::{MirrorStatus, Space, SpaceMirror, UnmirrorSpaceRequest, models};
use theodore_ports::{GitError, SyncActionCode};

use crate::core::Core;
use crate::error::{AppError, AppResult};

/// Shown as a sentence fragment: "the folder has 2 uncommitted changes".
fn dirty_folder_refusal(dirty_files: u32) -> Option<String> {
    (dirty_files > 0).then(|| {
        format!(
            "the folder has {dirty_files} uncommitted change{}; commit or stash {} first",
            if dirty_files == 1 { "" } else { "s" },
            if dirty_files == 1 { "it" } else { "them" },
        )
    })
}

/// Up to 5 paths, "and N more" past that (same shape as a ship conflict
/// list, `crates/git/src/ship.rs`).
fn path_list(paths: &[String]) -> String {
    const SHOWN: usize = 5;
    let mut s = paths
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > SHOWN {
        s.push_str(&format!(" and {} more", paths.len() - SHOWN));
    }
    s
}

/// A safety refusal reads as the user's mistake to fix, not a git failure
/// (same mapping as shipping, `crates/app/src/usecases/ship.rs`).
fn refused(e: GitError) -> AppError {
    match e {
        GitError::Refused(m) => AppError::Invalid(m),
        other => AppError::Git(other),
    }
}

impl Core {
    /// "Mirror into `<folder>`": on fresh, or resuming a paused one. Turns
    /// mirroring off first for any other space of this project that has
    /// it (the only way "at most one at a time" can be violated is a
    /// stale row from before a switch finished, so this keeps it true
    /// without the UI tracking it itself).
    pub async fn mirror_space(&self, space_id: &str) -> AppResult<Space> {
        let space = self.space_row(space_id).await?;
        if space
            .mirror
            .as_ref()
            .is_some_and(|m| m.status == MirrorStatus::On)
        {
            return Ok(space);
        }
        let project = self.space_project(&space).await?;
        let folder =
            project.path.clone().map(PathBuf::from).ok_or_else(|| {
                AppError::Invalid("mirroring needs a local folder project".into())
            })?;
        // First: what another space mirrored here isn't the folder's own
        // change, and it refuses if the folder was edited meanwhile.
        for other in self.list_spaces(Some(project.id.clone())).await? {
            if other.id != space.id && other.mirror.is_some() {
                self.unmirror_space_inner(&other, &folder, false, true)
                    .await?;
            }
        }
        let state = self.0.deps.git.working_state(&folder).await?;
        match &space.mirror {
            // Resuming: the folder holds this space's own files, as git sees
            // it dirty. Only what was edited since stands in the way.
            Some(mirror) => {
                let repo_path = self.ensure_project_cache(&project).await?;
                let to = mirror
                    .mirrored_head
                    .as_ref()
                    .unwrap_or(&mirror.remembered_head);
                let edited = self
                    .0
                    .deps
                    .git
                    .mirror_edited(&folder, &repo_path, &space.id, to, &mirror.touched)
                    .await
                    .map_err(refused)?;
                if !edited.is_empty() {
                    return Err(AppError::Invalid(format!(
                        "the folder was edited since mirroring: {}; commit or undo that first",
                        path_list(&edited)
                    )));
                }
            }
            None => {
                if let Some(why) = dirty_folder_refusal(state.dirty_files) {
                    return Err(AppError::Invalid(why));
                }
            }
        }

        let now = self.0.deps.clock.now();
        let mirror = match space.mirror.clone() {
            // Resuming: the restore target stays what it was when it was
            // first turned on, not the folder's state right now.
            Some(existing) => SpaceMirror {
                status: MirrorStatus::On,
                updated_at: now,
                ..existing
            },
            None => {
                let remembered_head = self.0.deps.git.resolve_ref(&folder, "HEAD").await?;
                SpaceMirror {
                    status: MirrorStatus::On,
                    remembered_head,
                    remembered_branch: state.current_branch,
                    mirrored_head: None,
                    touched: Vec::new(),
                    updated_at: now,
                }
            }
        };
        self.apply(vec![self.mirror_patch(&space.id, &mirror)])
            .await?;
        self.sync_mirror(&space.id).await
    }

    /// "Sync now", and the automatic re-sync after a turn: writes the
    /// space's current checkpoint into the folder, only the paths that
    /// changed since the last sync. A no-op, not an error, when mirroring
    /// is off or paused — nothing auto-writes into a folder.
    pub async fn sync_mirror(&self, space_id: &str) -> AppResult<Space> {
        let space = self.space_row(space_id).await?;
        let Some(mirror) = space.mirror.clone() else {
            return Ok(space);
        };
        if mirror.status != MirrorStatus::On {
            return Ok(space);
        }
        let from = mirror
            .mirrored_head
            .clone()
            .unwrap_or_else(|| space.base_sha.clone());
        let to = space.head_sha.clone();
        if from == to {
            return Ok(space);
        }
        let project = self.space_project(&space).await?;
        let folder =
            project.path.clone().map(PathBuf::from).ok_or_else(|| {
                AppError::Invalid("mirroring needs a local folder project".into())
            })?;
        let repo_path = self.ensure_project_cache(&project).await?;
        let changed = self
            .0
            .deps
            .git
            .mirror_sync(&folder, &repo_path, &space.id, &to, &from)
            .await
            .map_err(refused)?;
        let mut touched = mirror.touched;
        for p in changed {
            if !touched.contains(&p) {
                touched.push(p);
            }
        }
        let new_mirror = SpaceMirror {
            mirrored_head: Some(to),
            touched,
            updated_at: self.0.deps.clock.now(),
            ..mirror
        };
        self.apply(vec![self.mirror_patch(&space.id, &new_mirror)])
            .await?;
        self.space_row(&space.id).await
    }

    /// "Mirror off": restores the folder to what it was before mirroring
    /// began. Refused, with the folder untouched, when it disagrees with
    /// what mirroring wrote there — `req.discard` overwrites those edits
    /// instead of refusing.
    pub async fn unmirror_space(&self, req: UnmirrorSpaceRequest) -> AppResult<Space> {
        let space = self.space_row(&req.space).await?;
        if space.mirror.is_none() {
            return Err(AppError::Invalid(
                "mirroring is already off for this space".into(),
            ));
        }
        let project = self.space_project(&space).await?;
        let folder =
            project.path.clone().map(PathBuf::from).ok_or_else(|| {
                AppError::Invalid("mirroring needs a local folder project".into())
            })?;
        self.unmirror_space_inner(&space, &folder, req.discard, false)
            .await
    }

    /// Does the restore and clears `mirror`; shared by `unmirror_space`
    /// and `mirror_space`'s switch-off-the-other-one step (`switching`:
    /// the refusal then names the space whose mirror it is, since the one
    /// being switched to has no Discard of its own to offer).
    async fn unmirror_space_inner(
        &self,
        space: &Space,
        folder: &Path,
        discard: bool,
        switching: bool,
    ) -> AppResult<Space> {
        let Some(mirror) = space.mirror.clone() else {
            return self.space_row(&space.id).await;
        };
        if !mirror.touched.is_empty() && !discard {
            let project = self.space_project(space).await?;
            let repo_path = self.ensure_project_cache(&project).await?;
            let to = mirror
                .mirrored_head
                .clone()
                .unwrap_or_else(|| mirror.remembered_head.clone());
            let edited = self
                .0
                .deps
                .git
                .mirror_edited(folder, &repo_path, &space.id, &to, &mirror.touched)
                .await
                .map_err(refused)?;
            if !edited.is_empty() && switching {
                let whose = space.title.as_deref().unwrap_or(&space.name);
                return Err(AppError::Invalid(format!(
                    "the folder was edited since \"{whose}\" mirrored into it: {}; \
                     open that space and turn its mirror off (Discard overwrites \
                     the edits), or commit or undo them first",
                    path_list(&edited)
                )));
            }
            if !edited.is_empty() {
                return Err(AppError::Invalid(format!(
                    "the folder was edited since mirroring: {}; Discard to overwrite",
                    path_list(&edited)
                )));
            }
        }
        self.0
            .deps
            .git
            .mirror_restore(folder, &mirror.remembered_head, &mirror.touched)
            .await
            .map_err(refused)?;
        self.apply(vec![theodore_ports::NewAction {
            model: models::SPACE.into(),
            model_id: space.id.clone(),
            action: SyncActionCode::C,
            data: Some(serde_json::json!({ "mirror": serde_json::Value::Null })),
        }])
        .await?;
        self.space_row(&space.id).await
    }

    /// Resyncs a space whose turn just ended, if it mirrors into a folder
    /// — a toast on a real failure, silence otherwise (most spaces don't
    /// mirror at all).
    pub(crate) async fn sync_mirror_after_turn(&self, space: &Space) {
        if space.mirror.is_none() {
            return;
        }
        if let Err(e) = self.sync_mirror(&space.id).await {
            self.emit(theodore_contract::Ephemeral::Toast {
                level: theodore_contract::ToastLevel::Warn,
                text: format!("{}: mirror sync failed: {e}", space.name),
            });
        }
    }

    /// Every space whose mirror was `On` is shown as `Paused` from here:
    /// nothing auto-writes into a folder on startup (docs/spaces.md
    /// "Mirror into a local folder").
    pub(crate) async fn pause_resumed_mirrors(&self) {
        let Ok(spaces) = self.list_spaces(None).await else {
            return;
        };
        for space in spaces {
            if let Some(m) = &space.mirror
                && m.status == MirrorStatus::On
            {
                let paused = SpaceMirror {
                    status: MirrorStatus::Paused,
                    ..m.clone()
                };
                let _ = self
                    .apply(vec![self.mirror_patch(&space.id, &paused)])
                    .await;
            }
        }
    }

    fn mirror_patch(&self, space: &str, mirror: &SpaceMirror) -> theodore_ports::NewAction {
        theodore_ports::NewAction {
            model: models::SPACE.into(),
            model_id: space.to_string(),
            action: SyncActionCode::C,
            data: Some(serde_json::json!({ "mirror": mirror })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirty_folder_message_pluralizes() {
        assert_eq!(dirty_folder_refusal(0), None);
        assert!(
            dirty_folder_refusal(1)
                .unwrap()
                .contains("1 uncommitted change;")
        );
        assert!(
            dirty_folder_refusal(2)
                .unwrap()
                .contains("2 uncommitted changes;")
        );
    }

    #[test]
    fn path_list_caps_at_five_and_counts_the_rest() {
        let few = vec!["a.txt".to_string(), "b.txt".to_string()];
        assert_eq!(path_list(&few), "a.txt, b.txt");
        let many: Vec<String> = (0..7).map(|i| format!("f{i}.txt")).collect();
        assert_eq!(
            path_list(&many),
            "f0.txt, f1.txt, f2.txt, f3.txt, f4.txt and 2 more"
        );
    }
}
