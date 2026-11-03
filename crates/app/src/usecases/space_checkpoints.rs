//! Going back in a space (docs/spaces.md §3 "Undo, rewind, revert"). A
//! checkpoint is a snapshot of the worktree on top of its `HEAD`
//! (`GitPort::space_checkpoint`), and every entry of `turn_log` records
//! both, so any of them can be restored — writing only what differs.
//!
//! ```text
//! turn_log   t1 ── t2 ── t3 ── t4
//! Undo                         t4 undone          ─▶ space at t4.before
//! Back to here on t2      t3 ── t4 undone         ─▶ space at t3.before
//! Revert a.ts ─▶ restore it ─▶ checkpoint ─▶ entry {reverted: [a.ts]} (undoable)
//!                                         └─▶ next prompt: "the user reverted a.ts…"
//! ```

use theodore_contract::{
    ChatMessage, RevertSpaceFileRequest, RewindSpaceRequest, ShipKind, Space, SpaceRevertFrom,
    SpaceReviewRequest, SpaceStatus, SpaceTurn, models,
};
use theodore_domain::Author;
use theodore_ports::{GitError, NewAction, SpaceCheckpoint, SyncActionCode};

use crate::core::Core;
use crate::error::{AppError, AppResult};
use crate::usecases::spaces::space_chat_key;

impl Core {
    /// "Back to here" on an agent reply: the space goes back to right
    /// after that turn, and every later turn is marked undone. Same
    /// refusals as Undo.
    pub async fn rewind_space(&self, req: RewindSpaceRequest) -> AppResult<Space> {
        let space = self.space_row(&req.space).await?;
        refusal(&space).map_err(AppError::Invalid)?;
        let log = &space.turn_log;
        let k = log
            .iter()
            .position(|t| t.reply == req.reply && !t.undone)
            .ok_or_else(|| AppError::Invalid("that turn is undone or not in this space".into()))?;
        let i = (k + 1..log.len())
            .find(|&i| !log[i].undone)
            .ok_or_else(|| AppError::Invalid("this is already the space's last turn".into()))?;
        self.rewind_space_from(&space.id, Some(i)).await
    }

    /// The space back to where `turn_log[from]` started (`None`: the last
    /// turn not undone yet); it and every later turn are marked undone,
    /// and later prompts leave them out.
    pub(crate) async fn rewind_space_from(
        &self,
        space: &str,
        from: Option<usize>,
    ) -> AppResult<Space> {
        // Not while a ship reads the checkpoint, or a queued send starts.
        let _guard = self.0.ship_lock.lock().await;
        let _queue = self.space_queue_lock(space).await;
        let space = self.space_row(space).await?;
        let i = rewindable(&space, from).map_err(AppError::Invalid)?;
        let turn = space.turn_log[i].clone();
        let to = SpaceCheckpoint {
            sha: turn.before.clone(),
            // A checkpoint from before snapshots was `HEAD` itself.
            head: turn
                .before_head
                .clone()
                .unwrap_or_else(|| turn.before.clone()),
        };
        let project = self.space_project(&space).await?;
        let repo_path = self.ensure_project_cache(&project).await?;
        let wt = self
            .0
            .deps
            .git
            .space_worktree(&repo_path, &space.id, &space.base_sha)
            .await?;
        self.0
            .deps
            .git
            .space_reset(&wt.path, &space.id, &to)
            .await
            .map_err(|e| busy_refusal("undo", e))?;
        let mut log = space.turn_log.clone();
        for t in log[i..].iter_mut() {
            t.undone = true;
        }
        // A turn that completed an update from base: the base before the
        // earliest one undone is back.
        let base_before = log[i..].iter().find_map(|t| t.base_before.clone());
        let mut data = serde_json::json!({
                "status": SpaceStatus::Idle,
                "head_sha": to.sha,
                "worktree_head": turn.before_head,
                "error": null,
                "turn_log": log,
                // The agent's session still holds the undone turns: the
                // next one starts over from the transcript, which leaves
                // them out.
                "agent_session": null,
                "updated_at": self.0.deps.clock.now(),
        });
        if let Some(base) = base_before {
            data["base_sha"] = serde_json::json!(base);
        }
        self.apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: space.id.clone(),
            action: SyncActionCode::C,
            data: Some(data),
        }])
        .await?;
        if to.sha != space.head_sha {
            self.space_review(SpaceReviewRequest {
                space: space.id.clone(),
                group: false,
            })
            .await?;
        }
        self.space_row(&space.id).await
    }

    /// "Revert to base" / "Revert the last turn's change" on a file of the
    /// space's diff: the files are restored in the worktree, checkpointed
    /// as a `turn_log` entry of their own (so Undo brings them back), and
    /// the next turn's prompt tells the agent not to redo it.
    pub async fn revert_space_file(&self, req: RevertSpaceFileRequest) -> AppResult<Space> {
        let _guard = self.0.ship_lock.lock().await;
        let _queue = self.space_queue_lock(&req.space).await;
        let space = self.space_row(&req.space).await?;
        if space.updating.is_some() {
            return Err(AppError::Invalid(
                "finish or abort the update from base first".into(),
            ));
        }
        match space.status {
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
        let mut paths: Vec<String> = Vec::new();
        for p in &req.paths {
            let p = p.trim().to_string();
            if !p.is_empty() && !paths.contains(&p) {
                paths.push(p);
            }
        }
        let Some(first) = paths.first().cloned() else {
            return Err(AppError::Invalid("no file to revert".into()));
        };
        let (from, said) = match req.from {
            SpaceRevertFrom::Base => (space.base_sha.clone(), "the base"),
            SpaceRevertFrom::LastTurn => {
                let last = space
                    .turn_log
                    .iter()
                    .rev()
                    .find(|t| !t.undone && t.reverted.is_empty())
                    .ok_or_else(|| AppError::Invalid("there is no turn to revert".into()))?;
                (last.before.clone(), "before the last turn")
            }
        };
        let project = self.space_project(&space).await?;
        let repo_path = self.ensure_project_cache(&project).await?;
        let wt = self
            .0
            .deps
            .git
            .space_worktree(&repo_path, &space.id, &space.base_sha)
            .await?;
        let git = &self.0.deps.git;
        git.space_restore_paths(&wt.path, &paths, &from)
            .await
            .map_err(|e| busy_refusal("revert", e))?;
        let cp = git
            .space_checkpoint(
                &wt.path,
                &space.id,
                &format!("{}: revert {first}", space.name),
            )
            .await
            .map_err(|e| busy_refusal("revert", e))?;
        if cp.sha == space.head_sha {
            return Err(AppError::Invalid(format!(
                "{first} is already as it was {said}"
            )));
        }

        let id = uuid::Uuid::new_v4().to_string();
        let now = self.0.deps.clock.now();
        let message = ChatMessage {
            id: id.clone(),
            pr: space_chat_key(&space.id),
            author: Author::Me,
            text: match req.from {
                SpaceRevertFrom::Base => format!("Reverted {} to the base.", quoted(&paths)),
                SpaceRevertFrom::LastTurn => {
                    format!("Reverted the last turn's change to {}.", quoted(&paths))
                }
            },
            refs: vec![],
            at: now,
            failed: None,
            attachments: vec![],
            queued: None,
        };
        let mut log = space.turn_log.clone();
        log.push(SpaceTurn {
            message: id.clone(),
            reply: id.clone(),
            before: space.head_sha.clone(),
            after: cp.sha.clone(),
            undone: false,
            check: None,
            steered: vec![],
            base_before: None,
            before_head: space.worktree_head.clone(),
            after_head: Some(cp.head.clone()),
            reverted: paths,
            plan: None,
        });
        self.apply(vec![
            NewAction {
                model: models::CHAT_MESSAGE.into(),
                model_id: id,
                action: SyncActionCode::I,
                data: Some(
                    serde_json::to_value(&message).map_err(|e| AppError::Invalid(e.to_string()))?,
                ),
            },
            NewAction {
                model: models::SPACE.into(),
                model_id: space.id.clone(),
                action: SyncActionCode::C,
                data: Some(serde_json::json!({
                    "head_sha": cp.sha,
                    "worktree_head": cp.head,
                    "turn_log": log,
                    "updated_at": now,
                })),
            },
        ])
        .await?;
        self.space_review(SpaceReviewRequest {
            space: space.id.clone(),
            group: false,
        })
        .await?;
        self.space_row(&space.id).await
    }
}

/// An unfinished rebase or merge in the worktree: the user's to finish.
fn busy_refusal(what: &str, e: GitError) -> AppError {
    match e {
        GitError::Busy(busy) => AppError::Invalid(format!(
            "can't {what}: {busy} in the space's worktree; finish or abort it first"
        )),
        e => e.into(),
    }
}

fn quoted(paths: &[String]) -> String {
    paths
        .iter()
        .map(|p| format!("`{p}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Why the space can't go back at all now: a turn runs, it is archived,
/// or an update from base is resolving.
fn refusal(space: &Space) -> Result<(), String> {
    match space.status {
        SpaceStatus::Running => {
            return Err("the agent is still working; stop it before undoing".into());
        }
        SpaceStatus::Archived => return Err("this space is archived".into()),
        SpaceStatus::Idle | SpaceStatus::Failed => {}
    }
    if space.updating.is_some() {
        return Err("finish or abort the update from base first".into());
    }
    Ok(())
}

/// The index in `turn_log` a rewind would start undoing at — `from`, or
/// the last turn not undone yet — or why it can't: not while a turn runs,
/// not on an archived space, not once the space moved past its last turn,
/// and not over a turn that already shipped.
pub(crate) fn rewindable(space: &Space, from: Option<usize>) -> Result<usize, String> {
    refusal(space)?;
    let log = &space.turn_log;
    let last = log
        .iter()
        .rposition(|t| !t.undone)
        .ok_or_else(|| "there is no turn to undo".to_string())?;
    let i = from.unwrap_or(last);
    if i > last || log[i].undone {
        return Err("that turn is already undone".into());
    }
    if log[last].after != space.head_sha {
        return Err("the space moved since its last turn".into());
    }
    if let Some(shipped) = &space.shipped {
        // Shipped from a checkpoint at or before turn `i`'s start: nothing
        // being undone has left the space. Anything else (a later
        // checkpoint, or one we can't place) was shipped.
        let mut earlier = log[..i]
            .iter()
            .filter(|t| !t.undone)
            .flat_map(|t| [&t.before, &t.after]);
        let before_turn = shipped.head == log[i].before
            || shipped.head == space.base_sha
            || earlier.any(|h| *h == shipped.head);
        if !before_turn {
            return Err(match shipped.how {
                ShipKind::Pr => "this turn is already pushed; ask the agent to revert it instead",
                ShipKind::MergedLocally => {
                    "this turn is already merged; ask the agent to revert it instead"
                }
                ShipKind::Branch => {
                    "this turn is already written as a branch; ask the agent to revert it instead"
                }
            }
            .into());
        }
    }
    Ok(i)
}

/// Files you reverted since the agent's last turn go into its next prompt,
/// one line each: its session may still hold the change, and it shouldn't
/// put it back on its own.
pub(crate) fn with_reverts(space: &Space, prompt: String) -> String {
    let mut reverts: Vec<&SpaceTurn> = space
        .turn_log
        .iter()
        .rev()
        .filter(|t| !t.undone)
        .take_while(|t| !t.reverted.is_empty())
        .collect();
    if reverts.is_empty() {
        return prompt;
    }
    reverts.reverse();
    let notes: Vec<String> = reverts
        .iter()
        .map(|t| {
            format!(
                "The user reverted {} since your last turn; don't redo that change unless asked.",
                quoted(&t.reverted)
            )
        })
        .collect();
    format!("{}\n\n{prompt}", notes.join("\n"))
}
