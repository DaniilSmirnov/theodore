//! Sending while a space's agent works (docs/spaces.md §8). A message sent
//! during a turn is stored as a queued `chat_message` and runs as its own
//! turn when the queue drains; "Send now" writes it into the running turn
//! instead, when the agent's CLI takes messages mid-run (Claude Code).
//!
//! ```text
//! send ─▶ running? ── no ──▶ turn now
//!            └─ yes ─▶ queued (seq n) ──▶ turn ends ─┬─ ok ─────────▶ next queued runs
//!                                                    └─ stopped / failed / restart
//!                                                          └─▶ held: "Send" or Cancel
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use theodore_contract::{
    ChatMessage, Ephemeral, QueuedAction, QueuedSend, Space, SpaceQueuedRequest, SpaceSendRequest,
    SpaceStatus, ToastLevel, models,
};
use theodore_domain::Author;
use theodore_ports::{NewAction, Steer, SyncActionCode};

use crate::core::Core;
use crate::error::{AppError, AppResult};
use crate::usecases::spaces::space_chat_key;

/// The queue's in-memory side: one lock per space, and the running turns
/// that can take a steered message.
#[derive(Default)]
pub(crate) struct QueueState {
    /// Held while a send decides between "now" and "queued" and while a
    /// turn ends and hands over to the next queued message, so the queue's
    /// order holds against a send that comes in between.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// By run id: the turn's steer handle while its agent runs, and the
    /// messages steered into it so far.
    steers: Mutex<HashMap<String, Steered>>,
}

#[derive(Default)]
struct Steered {
    steer: Option<Steer>,
    messages: Vec<String>,
}

/// How a turn ended, for the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnEnd {
    /// Finished on its own: the next queued message runs.
    Done,
    /// Stopped or failed: the queue is held for you.
    Interrupted,
}

impl Core {
    /// The space's queue lock (see [`QueueState::locks`]).
    pub(crate) async fn space_queue_lock(&self, space: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = self
            .0
            .space_queue
            .locks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(space.to_string())
            .or_default()
            .clone();
        lock.lock_owned().await
    }

    /// Stores `req` as the last queued message of the space. Caller holds
    /// the space's queue lock and has validated the message.
    pub(crate) async fn queue_message(
        &self,
        space: &Space,
        req: &SpaceSendRequest,
        text: String,
        attachments: Vec<theodore_contract::Attachment>,
    ) -> AppResult<String> {
        let queue = self.queued_messages(&space.id).await?;
        let seq = queue
            .iter()
            .filter_map(|m| m.queued.as_ref())
            .map(|q| q.seq)
            .max()
            .unwrap_or(0)
            + 1;
        let id = uuid::Uuid::new_v4().to_string();
        let message = ChatMessage {
            id: id.clone(),
            pr: space_chat_key(&space.id),
            author: Author::Me,
            text,
            refs: vec![],
            at: self.0.deps.clock.now(),
            failed: None,
            attachments,
            queued: Some(QueuedSend {
                seq,
                agent: req.agent,
                model: req.model.clone(),
                mode: req.mode,
                held: false,
            }),
        };
        self.apply(vec![NewAction {
            model: models::CHAT_MESSAGE.into(),
            model_id: id.clone(),
            action: SyncActionCode::I,
            data: Some(to_json(&message)?),
        }])
        .await?;
        Ok(id)
    }

    /// The space's queued messages, in queue order.
    pub(crate) async fn queued_messages(&self, space: &str) -> AppResult<Vec<ChatMessage>> {
        let store = self.0.deps.store.clone();
        let key = space_chat_key(space).0;
        let rows =
            tokio::task::spawn_blocking(move || store.query(models::CHAT_MESSAGE, "pr", &key))
                .await
                .map_err(AppError::join)??;
        let mut queue: Vec<ChatMessage> = rows
            .into_iter()
            .filter_map(|r| serde_json::from_value(r.data).ok())
            .filter(|m: &ChatMessage| m.queued.is_some())
            .collect();
        queue.sort_by_key(|m| m.queued.as_ref().map(|q| q.seq));
        Ok(queue)
    }

    /// Send / Cancel / Edit on one queued message.
    pub async fn space_queued(&self, req: SpaceQueuedRequest) -> AppResult<()> {
        let _queue = self.space_queue_lock(&req.space).await;
        let message = self
            .queued_messages(&req.space)
            .await?
            .into_iter()
            .find(|m| m.id == req.message)
            .ok_or_else(|| AppError::NotFound(format!("queued message {}", req.message)))?;
        match req.action {
            QueuedAction::Cancel => {
                self.apply(vec![NewAction {
                    model: models::CHAT_MESSAGE.into(),
                    model_id: message.id,
                    action: SyncActionCode::D,
                    data: None,
                }])
                .await?;
            }
            QueuedAction::Edit { text } => {
                let text = text.trim().to_string();
                if text.is_empty() && message.attachments.is_empty() {
                    return Err(AppError::Invalid("empty message".into()));
                }
                self.patch_message(&message.id, serde_json::json!({ "text": text }))
                    .await?;
            }
            QueuedAction::Send => {
                let space = self.space_row(&req.space).await?;
                match space.status {
                    SpaceStatus::Archived => {
                        return Err(AppError::Invalid("this space is archived".into()));
                    }
                    // Back in line: it goes when the running turn ends.
                    SpaceStatus::Running => {
                        if let Some(mut queued) = message.queued.clone() {
                            queued.held = false;
                            self.patch_message(
                                &message.id,
                                serde_json::json!({ "queued": queued }),
                            )
                            .await?;
                        }
                    }
                    SpaceStatus::Idle | SpaceStatus::Failed => {
                        self.check_setup_done(&space)?;
                        self.start_queued(space, message).await?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Starts the turn of a queued message, which stops being queued.
    /// Caller holds the space's queue lock; the space isn't running.
    async fn start_queued(&self, space: Space, message: ChatMessage) -> AppResult<String> {
        let queued = message.queued.clone();
        let req = SpaceSendRequest {
            space: space.id.clone(),
            text: message.text.clone(),
            agent: queued.as_ref().and_then(|q| q.agent),
            model: queued.as_ref().and_then(|q| q.model.clone()),
            mode: queued.as_ref().and_then(|q| q.mode),
            attachments: None,
        };
        let message = ChatMessage {
            // Sent now: it sorts after the turn it waited for.
            at: self.0.deps.clock.now(),
            queued: None,
            ..message
        };
        self.start_turn(space, &req, message, true).await
    }

    /// [`start_queued`](Self::start_queued) behind a named future type: a
    /// turn's end starts the next turn, whose end starts the next…
    fn start_queued_boxed(
        &self,
        space: Space,
        message: ChatMessage,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = AppResult<String>> + Send + '_>> {
        Box::pin(self.start_queued(space, message))
    }

    /// A turn ended: its final space patch, then the queue — the next
    /// message runs, or the queue is held for you. One step under the
    /// space's queue lock, so no send slips in between.
    pub(crate) async fn end_turn(&self, space: &str, actions: Vec<NewAction>, end: TurnEnd) {
        let _queue = self.space_queue_lock(space).await;
        let _ = self.apply(actions).await;
        match end {
            TurnEnd::Interrupted => self.hold_queue(space).await,
            TurnEnd::Done => self.drain_queue(space).await,
        }
    }

    /// Runs the first waiting message, if the space is free for it.
    async fn drain_queue(&self, space_id: &str) {
        let Ok(space) = self.space_row(space_id).await else {
            return;
        };
        if !matches!(space.status, SpaceStatus::Idle | SpaceStatus::Failed) {
            return;
        }
        let Ok(queue) = self.queued_messages(space_id).await else {
            return;
        };
        let Some(next) = queue
            .into_iter()
            .find(|m| m.queued.as_ref().is_some_and(|q| !q.held))
        else {
            return;
        };
        let name = space.name.clone();
        if let Err(e) = self.start_queued_boxed(space, next).await {
            // It can't start (agent gone, worktree trouble): you decide.
            self.hold_queue(space_id).await;
            self.emit(Ephemeral::Toast {
                level: ToastLevel::Warn,
                text: format!("{name}: the queued message didn't start: {e}"),
            });
        }
    }

    /// Every queued message of the space waits for your "Send".
    pub(crate) async fn hold_queue(&self, space: &str) {
        let Ok(queue) = self.queued_messages(space).await else {
            return;
        };
        let actions: Vec<NewAction> = queue
            .into_iter()
            .filter_map(|m| {
                let mut q = m.queued?;
                (!q.held).then(|| {
                    q.held = true;
                    NewAction {
                        model: models::CHAT_MESSAGE.into(),
                        model_id: m.id,
                        action: SyncActionCode::C,
                        data: Some(serde_json::json!({ "queued": q })),
                    }
                })
            })
            .collect();
        if !actions.is_empty() {
            let _ = self.apply(actions).await;
        }
    }

    /// On start: the turns those messages waited for never finished, so
    /// nothing is sent on its own; each waits for your "Send".
    pub(crate) async fn hold_queues_after_restart(&self, spaces: &[Space]) {
        for s in spaces {
            self.hold_queue(&s.id).await;
        }
    }

    /// "Send now": into the running turn when its agent takes messages
    /// mid-run and the message needs nothing the run can't change (another
    /// agent, model or mode, attachments); otherwise the usual send, so
    /// queued while a turn runs.
    pub async fn space_send_now(&self, req: SpaceSendRequest) -> AppResult<String> {
        let queue = self.space_queue_lock(&req.space).await;
        let space = self.space_row(&req.space).await?;
        let text = req.text.trim().to_string();
        let plain = req.attachments.as_ref().is_none_or(|a| a.is_empty())
            && req.agent.is_none_or(|a| a == space.agent)
            && req
                .model
                .as_ref()
                .is_none_or(|m| Some(m) == space.model.as_ref())
            && req.mode.is_none_or(|m| m == space.mode);
        let steer = match (&space.run, space.status) {
            (Some(run), SpaceStatus::Running) if plain && !text.is_empty() => self
                .0
                .space_queue
                .steers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(run)
                .and_then(|s| s.steer.clone())
                .map(|steer| (run.clone(), steer)),
            _ => None,
        };
        let Some((run, steer)) = steer else {
            drop(queue);
            return self.space_send(req).await;
        };
        let worktree = std::path::PathBuf::from(&space.worktree);
        let claude_home = &self.0.deps.claude_home;
        let ask = crate::usecases::skills::expand_skill(&text, |id| {
            crate::usecases::skills::space_skill_content(id, &worktree, claude_home)
        })
        .unwrap_or_else(|| text.clone());
        let id = uuid::Uuid::new_v4().to_string();
        let at = self.0.deps.clock.now();
        if !steer.send(ask).await {
            // The run ended meanwhile: it waits for the next turn instead.
            drop(queue);
            return self.space_send(req).await;
        }
        if let Some(s) = self
            .0
            .space_queue
            .steers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_mut(&run)
        {
            s.messages.push(id.clone());
        }
        let message = ChatMessage {
            id: id.clone(),
            pr: space_chat_key(&space.id),
            author: Author::Me,
            text,
            refs: vec![],
            at,
            failed: None,
            attachments: vec![],
            queued: None,
        };
        self.apply(vec![NewAction {
            model: models::CHAT_MESSAGE.into(),
            model_id: id.clone(),
            action: SyncActionCode::I,
            data: Some(to_json(&message)?),
        }])
        .await?;
        Ok(id)
    }

    /// The running agent of `run` takes steered messages through `steer`.
    pub(crate) fn steer_run(&self, run: &str, steer: Option<Steer>) {
        let mut steers = self
            .0
            .space_queue
            .steers
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        steers.entry(run.to_string()).or_default().steer = steer;
    }

    /// The agent of `run` exited: nothing more can be steered into it.
    pub(crate) fn unsteer_run(&self, run: &str) {
        if let Some(s) = self
            .0
            .space_queue
            .steers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_mut(run)
        {
            s.steer = None;
        }
    }

    /// The messages steered into `run`; forgets the run.
    pub(crate) fn take_steered(&self, run: &str) -> Vec<String> {
        self.0
            .space_queue
            .steers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(run)
            .map(|s| s.messages)
            .unwrap_or_default()
    }

    async fn patch_message(&self, id: &str, data: serde_json::Value) -> AppResult<()> {
        self.apply(vec![NewAction {
            model: models::CHAT_MESSAGE.into(),
            model_id: id.to_string(),
            action: SyncActionCode::C,
            data: Some(data),
        }])
        .await?;
        Ok(())
    }
}

/// Queued messages are left out of the transcript a turn replays.
pub(crate) fn is_queued(m: &ChatMessage) -> bool {
    m.queued.is_some()
}

fn to_json<T: serde::Serialize>(v: &T) -> AppResult<serde_json::Value> {
    serde_json::to_value(v).map_err(|e| AppError::Invalid(e.to_string()))
}
