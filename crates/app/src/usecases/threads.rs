//! `reply_thread`, `resolve_thread` (plan §6.5, §6.7): outbox ops over
//! `HostAdapter::reply`/`resolve`. The outbox worker (`outbox.rs`) runs
//! them, refreshing the PR's `pr_detail` on success.

use theodore_contract::{ReplyRequest, ResolveRequest, models};
use theodore_domain::{PrId, ThreadId};
use theodore_ports::{NewAction, SyncActionCode};

use crate::core::Core;
use crate::error::{AppError, AppResult};
use crate::outbox::{ReplyPayload, ResolvePayload};

impl Core {
    pub async fn reply_thread(&self, req: ReplyRequest) -> AppResult<()> {
        let payload = ReplyPayload {
            pr: req.pr,
            thread: req.thread,
            body: req.body,
            marker: uuid::Uuid::new_v4().to_string(),
        };
        let data = serde_json::to_value(&payload).map_err(|e| AppError::Invalid(e.to_string()))?;
        self.enqueue_outbox("reply_thread", data).await?;
        Ok(())
    }

    /// Shows the thread (un)resolved at once, then tells the host. A
    /// resolve doesn't change the PR's REST resource, so the follow-up
    /// conditional fetch comes back 304 and would leave the old state up
    /// until something else changed; the outbox puts it back if the host
    /// refuses.
    pub async fn resolve_thread(&self, req: ResolveRequest) -> AppResult<()> {
        self.set_thread_resolved(&req.pr, &req.thread, req.resolved)
            .await;
        let payload = ResolvePayload {
            pr: req.pr,
            thread: req.thread,
            resolved: req.resolved,
        };
        let data = serde_json::to_value(&payload).map_err(|e| AppError::Invalid(e.to_string()))?;
        self.enqueue_outbox("resolve_thread", data).await?;
        Ok(())
    }

    /// Sets one thread's `resolved` in the stored `pr_detail`; nothing if
    /// the PR or the thread isn't there.
    pub(crate) async fn set_thread_resolved(&self, pr: &PrId, thread: &ThreadId, resolved: bool) {
        let Ok(mut detail) = self.load_pr_detail(pr).await else {
            return;
        };
        let Some(t) = detail.threads.iter_mut().find(|t| &t.id == thread) else {
            return;
        };
        if t.resolved == resolved {
            return;
        }
        t.resolved = resolved;
        let Ok(data) = serde_json::to_value(&detail) else {
            return;
        };
        let _ = self
            .apply(vec![NewAction {
                model: models::PR_DETAIL.into(),
                model_id: pr.0.clone(),
                action: SyncActionCode::U,
                data: Some(data),
            }])
            .await;
    }
}
