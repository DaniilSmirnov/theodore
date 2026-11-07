//! Outbox worker (plan §6.7): resumable host writes. One item at a time,
//! retried with backoff that depends on the error kind.
//!
//! ```text
//! host error                         outcome
//! ─────────────────────────────────  ─────────────────────────────────────────
//! Network (incl. 5xx, timeouts)      retry: 2 s doubling to 5 min, then 3 h;
//! RateLimited                        retry at reset_at;
//!                                    either gives up after MAX_ATTEMPTS (~a day)
//! Auth (token rejected)              pause 30 s, banner; waits for a reconnect
//! Validation · Forbidden · NotFound  fail now: compensate, toast the host's
//! Conflict · Protocol · Unsupported    message, item `failed` (not pending)
//! ```

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use theodore_contract::{Ephemeral, ToastLevel, models};
use theodore_domain::{
    Anchor, Comment, CommentId, Person, PrId, PrRef, ReviewEvent, Sha, ThreadId, ViewedState,
};
use theodore_ports::{
    HostError, MergeMethod, NewAction, OutboxItem, ReviewComment, ReviewSubmission,
    SubmittedReview, SyncActionCode,
};

use crate::core::{Core, Inner};
use crate::wire::ThreadRow;

const POLL_IDLE_MS: u64 = 50;
const BASE_BACKOFF_MS: i64 = 2_000;
const MAX_BACKOFF_MS: i64 = 5 * 60_000;
/// Retries past this many go every `SLOW_BACKOFF_MS` instead: the first
/// dozen (~30 min) cover a blip, the rest a long outage.
const FAST_ATTEMPTS: u32 = 12;
const SLOW_BACKOFF_MS: i64 = 3 * 60 * 60_000;
/// Failed attempts after which a transient error gives up: 12 fast + 7
/// slow retries ≈ 22 h. Without a cap one stuck write retried for days and
/// held off "Restart to update" (`is_idle`) the whole time.
pub(crate) const MAX_ATTEMPTS: u32 = 20;
const AUTH_RETRY_MS: i64 = 30_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetViewedPayload {
    pub pr: PrId,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftComment {
    pub draft_id: String,
    pub marker: String,
    pub anchor: Anchor,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitReviewPayload {
    pub pr: PrId,
    pub head: Sha,
    pub event: ReviewEvent,
    pub body: String,
    pub comments: Vec<DraftComment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyPayload {
    pub pr: PrId,
    pub thread: ThreadId,
    pub body: String,
    /// Idempotency marker embedded in the posted body (plan §6.7): a retry
    /// after a timeout checks for it via `find_marker` before reposting.
    #[serde(default = "new_marker")]
    pub marker: String,
}

fn new_marker() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvePayload {
    pub pr: PrId,
    pub thread: ThreadId,
    pub resolved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergePayload {
    pub pr: PrId,
    pub method: MergeMethod,
    pub title: Option<String>,
    pub body: Option<String>,
    pub auto: bool,
    pub queue: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewersPayload {
    pub pr: PrId,
    pub add: Vec<String>,
    pub add_teams: Vec<String>,
    pub remove: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftPayload {
    pub pr: PrId,
    pub draft: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateBranchPayload {
    pub pr: PrId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactPayload {
    pub pr: PrId,
    pub comment: CommentId,
    pub emoji: String,
    pub on: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditCommentPayload {
    pub pr: PrId,
    pub comment: CommentId,
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerunCheckPayload {
    pub pr: PrId,
    /// `Check::id`, resolved back to the full `Check` (the adapter needs
    /// its `url` too) from the stored `pr_detail` row when the item runs.
    pub check: String,
}

enum Outcome {
    /// Transient: due again at `.0`; `.1` is why this attempt failed.
    RetryAt(i64, String),
    AuthPaused(i64),
    /// Permanent: the host's message (or why it can't be done).
    Fail(String),
}

pub(crate) async fn run(inner: Arc<Inner>) {
    let core = Core(inner.clone());
    loop {
        // Signed out where sign-in is required: writes wait in the outbox.
        if !core.account_hold().await {
            return;
        }
        let now = inner.deps.clock.now();
        let store = inner.deps.store.clone();
        let item = tokio::task::spawn_blocking(move || store.outbox_take(now))
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten();

        match item {
            Some(item) => {
                let span = tracing::info_span!(
                    "outbox_item",
                    op = %item.op,
                    item = item.id,
                    attempt = item.attempts
                );
                tracing::Instrument::instrument(process_item(&core, item), span).await
            }
            None => {
                // `not_before` is measured on the injected `Clock` (so tests
                // control backoff deterministically), but this idle tick is
                // real wall time: a fake clock only moves on an explicit
                // `advance()`, and tying the tick to it too would mean a
                // tick armed before that call can wait on a target that's
                // already unreachable once the jump lands.
                tokio::select! {
                    _ = inner.shutdown.cancelled() => return,
                    _ = inner.outbox_notify.notified() => {},
                    _ = tokio::time::sleep(Duration::from_millis(POLL_IDLE_MS)) => {},
                }
            }
        }
    }
}

async fn process_item(core: &Core, item: OutboxItem) {
    let result = match item.op.as_str() {
        "set_viewed" => do_set_viewed(core, &item).await,
        "submit_review" => do_submit_review(core, &item).await,
        "reply_thread" => do_reply_thread(core, &item).await,
        "resolve_thread" => do_resolve_thread(core, &item).await,
        "merge_pr" => do_merge_pr(core, &item).await,
        "set_reviewers" => do_set_reviewers(core, &item).await,
        "set_draft" => do_set_draft(core, &item).await,
        "update_branch" => do_update_branch(core, &item).await,
        "react" => do_react(core, &item).await,
        "edit_comment" => do_edit_comment(core, &item).await,
        "rerun_check" => do_rerun_check(core, &item).await,
        "triage" => do_triage(core, &item).await,
        "create_space_pr" => do_create_space_pr(core, &item).await,
        "create_config_pr" => do_create_config_pr(core, &item).await,
        other => Err(Outcome::Fail(format!("unknown outbox op {other}"))),
    };
    let store = core.0.deps.store.clone();
    match result {
        Ok(()) => {
            let id = item.id;
            let _ = tokio::task::spawn_blocking(move || store.outbox_done(id)).await;
            if let Some(waiter) = core.0.outbox_waiters.lock().await.remove(&id) {
                let _ = waiter.send(None);
            }
            // A write got through: the host is reachable and the token
            // good — clear an offline / auth / rate-limit banner now rather
            // than at the next poll.
            core.note_host::<()>(&Ok(()));
        }
        Err(Outcome::RetryAt(not_before, error)) => {
            let id = item.id;
            tracing::info!(retry_at = not_before, error = %error, "outbox item will retry");
            let _ = tokio::task::spawn_blocking(move || store.outbox_retry(id, not_before, &error))
                .await;
        }
        Err(Outcome::AuthPaused(not_before)) => {
            tracing::warn!(
                error.code = "host.auth",
                "outbox paused: the host token was rejected"
            );
            let id = item.id;
            let error = HostError::Auth.to_string();
            let _ = tokio::task::spawn_blocking(move || store.outbox_retry(id, not_before, &error))
                .await;
            core.emit(Ephemeral::HostStatus {
                online: true,
                rate_limited_until: None,
                auth_ok: false,
                server_trouble: false,
            });
        }
        Err(Outcome::Fail(message)) => {
            let id = item.id;
            tracing::warn!(
                error.code = "outbox.gave_up",
                error = %message,
                "outbox gave up on a host write"
            );
            let error = message.clone();
            let _ = tokio::task::spawn_blocking(move || store.outbox_fail(id, &error)).await;
            // A caller still waiting reports it itself (its command fails
            // with this message): one toast, not two.
            let waiter = core.0.outbox_waiters.lock().await.remove(&id);
            match waiter.map(|w| w.send(Some(message.clone()))) {
                Some(Ok(())) => {}
                _ => core.emit(Ephemeral::Toast {
                    level: ToastLevel::Error,
                    text: message,
                }),
            }
        }
    }
}

fn backoff_ms(attempts: u32) -> i64 {
    if attempts >= FAST_ATTEMPTS {
        return SLOW_BACKOFF_MS;
    }
    let scale = 1i64 << attempts.min(10);
    (BASE_BACKOFF_MS.saturating_mul(scale)).min(MAX_BACKOFF_MS)
}

/// The host's own words for a toast: the error's message, or its full
/// text when the host sent none (a bare 404).
fn host_message(err: &HostError) -> String {
    match err {
        HostError::Validation(m)
        | HostError::Forbidden(m)
        | HostError::NotFound(m)
        | HostError::Conflict(m)
            if !m.trim().is_empty() =>
        {
            m.clone()
        }
        other => other.to_string(),
    }
}

async fn classify_host_error(core: &Core, item: &OutboxItem, err: HostError) -> Outcome {
    let retry = |not_before: i64, err: &HostError| {
        // This failure is attempt `attempts + 1`: the last one allowed
        // makes a transient error final.
        if item.attempts + 1 >= MAX_ATTEMPTS {
            Outcome::Fail(format!("gave up after {MAX_ATTEMPTS} attempts: {err}"))
        } else {
            Outcome::RetryAt(not_before, err.to_string())
        }
    };
    match err {
        HostError::RateLimited { reset_at } => {
            // The UI's rate-limit banner counts down to this.
            core.emit(Ephemeral::HostStatus {
                online: true,
                rate_limited_until: Some(reset_at),
                auth_ok: true,
                server_trouble: false,
            });
            retry(reset_at, &err)
        }
        HostError::Network(_) | HostError::Server(_) => {
            let now = core.0.deps.clock.now();
            retry(now + backoff_ms(item.attempts), &err)
        }
        HostError::Auth => {
            let now = core.0.deps.clock.now();
            Outcome::AuthPaused(now + AUTH_RETRY_MS)
        }
        // A malformed answer (`Protocol`) is no blip either: the same
        // request gets the same answer, so retrying only loops.
        HostError::Validation(_)
        | HostError::Forbidden(_)
        | HostError::Conflict(_)
        | HostError::NotFound(_)
        | HostError::Protocol(_)
        | HostError::Unsupported(_) => Outcome::Fail(host_message(&err)),
    }
}

async fn do_set_viewed(core: &Core, item: &OutboxItem) -> Result<(), Outcome> {
    let payload: SetViewedPayload =
        serde_json::from_value(item.payload.clone()).map_err(|e| Outcome::Fail(e.to_string()))?;
    let pr_ref = core
        .pr_ref(&payload.pr)
        .await
        .ok_or_else(|| Outcome::Fail("pr not found".into()))?;
    // The item carries no state: it sends what the row says now, so toggles
    // that rode on it while it waited end on the host as they did locally.
    let row_id = format!("{}:{}", payload.pr.0, payload.path);
    let store = core.0.deps.store.clone();
    let row = tokio::task::spawn_blocking(move || store.get(models::FILE_VIEWED, &row_id))
        .await
        .ok()
        .and_then(|r| r.ok())
        .flatten()
        .and_then(|r| serde_json::from_value::<theodore_contract::FileViewedRow>(r.data).ok());
    let viewed = row.is_none_or(|r| r.state == ViewedState::Viewed);
    match core
        .0
        .deps
        .host
        .set_viewed(&pr_ref, &payload.path, viewed)
        .await
    {
        Ok(()) => Ok(()),
        Err(e) => {
            let outcome = classify_host_error(core, item, e).await;
            if matches!(outcome, Outcome::Fail(_)) {
                compensate_set_viewed(core, &payload, viewed).await;
            }
            Err(outcome)
        }
    }
}

/// Puts the row back the way the host still has it: the opposite of the
/// state the failed write tried to send.
async fn compensate_set_viewed(core: &Core, payload: &SetViewedPayload, sent: bool) {
    let row_id = format!("{}:{}", payload.pr.0, payload.path);
    let data = serde_json::to_value(theodore_contract::FileViewedRow {
        pr: payload.pr.clone(),
        path: payload.path.clone(),
        state: if sent {
            ViewedState::Unviewed
        } else {
            ViewedState::Viewed
        },
    })
    .expect("serializable");
    let action = NewAction {
        model: models::FILE_VIEWED.into(),
        model_id: row_id,
        action: SyncActionCode::U,
        data: Some(data),
    };
    let _ = core.apply(vec![action]).await;
}

async fn do_submit_review(core: &Core, item: &OutboxItem) -> Result<(), Outcome> {
    let payload: SubmitReviewPayload =
        serde_json::from_value(item.payload.clone()).map_err(|e| Outcome::Fail(e.to_string()))?;
    let pr_ref = core
        .pr_ref(&payload.pr)
        .await
        .ok_or_else(|| Outcome::Fail("pr not found".into()))?;

    // A comment whose marker is already on the PR was posted before (a
    // timeout, an earlier submit that kept its draft): never post it
    // again (plan §6.7). One read of the PR's comments per attempt.
    let markers: Vec<String> = payload.comments.iter().map(|c| c.marker.clone()).collect();
    let posted = match core.0.deps.host.find_markers(&pr_ref, &markers).await {
        Ok(found) => found,
        Err(e) => return Err(classify_host_error(core, item, e).await),
    };
    let comments: Vec<ReviewComment> = payload
        .comments
        .iter()
        .filter(|c| !posted.contains(&c.marker))
        .map(|c| ReviewComment {
            anchor: c.anchor.clone(),
            body: c.body.clone(),
            marker: c.marker.clone(),
        })
        .collect();
    // Every comment of a body-less Comment review is on the PR already:
    // there is nothing left to post, and an empty review is refused.
    if comments.is_empty()
        && !payload.comments.is_empty()
        && payload.body.trim().is_empty()
        && payload.event == ReviewEvent::Comment
    {
        archive_drafts(core, payload.comments.iter().map(|c| c.draft_id.clone())).await;
        refresh_pr_detail(core, &payload.pr, &pr_ref).await;
        return Ok(());
    }

    let submission = ReviewSubmission {
        event: payload.event,
        body: payload.body.clone(),
        comments,
        commit: payload.head.clone(),
    };
    match core.0.deps.host.submit_review(&pr_ref, &submission).await {
        Ok(submitted) => {
            apply_submitted_review(core, &payload, &submitted).await;
            // The UI reads threads from `pr_detail`: refetch it so the new
            // threads show now, not on the next poll (same as a reply).
            refresh_pr_detail(core, &payload.pr, &pr_ref).await;
            Ok(())
        }
        Err(e) => Err(classify_host_error(core, item, e).await),
    }
}

async fn do_reply_thread(core: &Core, item: &OutboxItem) -> Result<(), Outcome> {
    let payload: ReplyPayload =
        serde_json::from_value(item.payload.clone()).map_err(|e| Outcome::Fail(e.to_string()))?;
    let pr_ref = core
        .pr_ref(&payload.pr)
        .await
        .ok_or_else(|| Outcome::Fail("pr not found".into()))?;

    // After a timeout, the reply may already have been posted: look it up
    // by its marker before posting it again (plan §6.7), same as
    // `do_submit_review`.
    if item.attempts > 0 {
        match core.0.deps.host.find_marker(&pr_ref, &payload.marker).await {
            Ok(Some(_)) => {
                refresh_pr_detail(core, &payload.pr, &pr_ref).await;
                return Ok(());
            }
            Ok(None) => {}
            Err(e) => return Err(classify_host_error(core, item, e).await),
        }
    }

    match core
        .0
        .deps
        .host
        .reply_with_marker(&pr_ref, &payload.thread, &payload.body, &payload.marker)
        .await
    {
        Ok(_) => {
            refresh_pr_detail(core, &payload.pr, &pr_ref).await;
            Ok(())
        }
        Err(e) => Err(classify_host_error(core, item, e).await),
    }
}

async fn do_resolve_thread(core: &Core, item: &OutboxItem) -> Result<(), Outcome> {
    let payload: ResolvePayload =
        serde_json::from_value(item.payload.clone()).map_err(|e| Outcome::Fail(e.to_string()))?;
    let pr_ref = core
        .pr_ref(&payload.pr)
        .await
        .ok_or_else(|| Outcome::Fail("pr not found".into()))?;
    match core
        .0
        .deps
        .host
        .resolve(&pr_ref, &payload.thread, payload.resolved)
        .await
    {
        Ok(()) => {
            refresh_pr_detail(core, &payload.pr, &pr_ref).await;
            // The host just took it: a fetch that raced the write and still
            // reads the old state doesn't get to undo it.
            core.set_thread_resolved(&payload.pr, &payload.thread, payload.resolved)
                .await;
            Ok(())
        }
        Err(e) => {
            let outcome = classify_host_error(core, item, e).await;
            if matches!(outcome, Outcome::Fail(_)) {
                core.set_thread_resolved(&payload.pr, &payload.thread, !payload.resolved)
                    .await;
            }
            Err(outcome)
        }
    }
}

async fn do_merge_pr(core: &Core, item: &OutboxItem) -> Result<(), Outcome> {
    let payload: MergePayload =
        serde_json::from_value(item.payload.clone()).map_err(|e| Outcome::Fail(e.to_string()))?;
    let pr_ref = core
        .pr_ref(&payload.pr)
        .await
        .ok_or_else(|| Outcome::Fail("pr not found".into()))?;
    let result = if payload.queue {
        core.0.deps.host.enqueue(&pr_ref).await
    } else if payload.auto {
        core.0
            .deps
            .host
            .enable_auto_merge(&pr_ref, payload.method)
            .await
    } else {
        core.0
            .deps
            .host
            .merge(
                &pr_ref,
                payload.method,
                payload.title.clone(),
                payload.body.clone(),
            )
            .await
            .map(|_sha| ())
    };
    match result {
        Ok(()) => {
            refresh_pr_detail(core, &payload.pr, &pr_ref).await;
            Ok(())
        }
        Err(e) => Err(classify_host_error(core, item, e).await),
    }
}

async fn do_set_reviewers(core: &Core, item: &OutboxItem) -> Result<(), Outcome> {
    let payload: ReviewersPayload =
        serde_json::from_value(item.payload.clone()).map_err(|e| Outcome::Fail(e.to_string()))?;
    let pr_ref = core
        .pr_ref(&payload.pr)
        .await
        .ok_or_else(|| Outcome::Fail("pr not found".into()))?;
    if (!payload.add.is_empty() || !payload.add_teams.is_empty())
        && let Err(e) = core
            .0
            .deps
            .host
            .request_reviewers(&pr_ref, &payload.add, &payload.add_teams)
            .await
    {
        return Err(classify_host_error(core, item, e).await);
    }
    for user in &payload.remove {
        if let Err(e) = core.0.deps.host.remove_reviewer(&pr_ref, user).await {
            return Err(classify_host_error(core, item, e).await);
        }
    }
    refresh_pr_detail(core, &payload.pr, &pr_ref).await;
    Ok(())
}

async fn do_set_draft(core: &Core, item: &OutboxItem) -> Result<(), Outcome> {
    let payload: DraftPayload =
        serde_json::from_value(item.payload.clone()).map_err(|e| Outcome::Fail(e.to_string()))?;
    let pr_ref = core
        .pr_ref(&payload.pr)
        .await
        .ok_or_else(|| Outcome::Fail("pr not found".into()))?;
    let result = if payload.draft {
        core.0.deps.host.convert_to_draft(&pr_ref).await
    } else {
        core.0.deps.host.mark_ready(&pr_ref).await
    };
    match result {
        Ok(()) => {
            refresh_pr_detail(core, &payload.pr, &pr_ref).await;
            Ok(())
        }
        Err(e) => Err(classify_host_error(core, item, e).await),
    }
}

async fn do_update_branch(core: &Core, item: &OutboxItem) -> Result<(), Outcome> {
    let payload: UpdateBranchPayload =
        serde_json::from_value(item.payload.clone()).map_err(|e| Outcome::Fail(e.to_string()))?;
    let pr_ref = core
        .pr_ref(&payload.pr)
        .await
        .ok_or_else(|| Outcome::Fail("pr not found".into()))?;
    match core.0.deps.host.update_branch(&pr_ref).await {
        Ok(()) => {
            refresh_pr_detail(core, &payload.pr, &pr_ref).await;
            Ok(())
        }
        Err(e) => Err(classify_host_error(core, item, e).await),
    }
}

async fn do_react(core: &Core, item: &OutboxItem) -> Result<(), Outcome> {
    let payload: ReactPayload =
        serde_json::from_value(item.payload.clone()).map_err(|e| Outcome::Fail(e.to_string()))?;
    let pr_ref = core
        .pr_ref(&payload.pr)
        .await
        .ok_or_else(|| Outcome::Fail("pr not found".into()))?;
    let result = if payload.on {
        core.0
            .deps
            .host
            .add_reaction(&pr_ref, &payload.comment, &payload.emoji)
            .await
    } else {
        core.0
            .deps
            .host
            .remove_reaction(&pr_ref, &payload.comment, &payload.emoji)
            .await
    };
    match result {
        Ok(()) => {
            refresh_pr_detail(core, &payload.pr, &pr_ref).await;
            Ok(())
        }
        Err(e) => Err(classify_host_error(core, item, e).await),
    }
}

async fn do_edit_comment(core: &Core, item: &OutboxItem) -> Result<(), Outcome> {
    let payload: EditCommentPayload =
        serde_json::from_value(item.payload.clone()).map_err(|e| Outcome::Fail(e.to_string()))?;
    let pr_ref = core
        .pr_ref(&payload.pr)
        .await
        .ok_or_else(|| Outcome::Fail("pr not found".into()))?;
    let result = match &payload.body {
        Some(body) => {
            core.0
                .deps
                .host
                .edit_comment(&pr_ref, &payload.comment, body)
                .await
        }
        None => {
            core.0
                .deps
                .host
                .delete_comment(&pr_ref, &payload.comment)
                .await
        }
    };
    match result {
        Ok(()) => {
            refresh_pr_detail(core, &payload.pr, &pr_ref).await;
            Ok(())
        }
        Err(e) => Err(classify_host_error(core, item, e).await),
    }
}

/// Re-fetches `pr_detail` so a thread write (reply, resolve) is reflected
/// without waiting for the next poll tick.
async fn refresh_pr_detail(core: &Core, pr: &PrId, pr_ref: &PrRef) {
    core.refresh_pr(pr, pr_ref).await;
}

async fn do_rerun_check(core: &Core, item: &OutboxItem) -> Result<(), Outcome> {
    let payload: RerunCheckPayload =
        serde_json::from_value(item.payload.clone()).map_err(|e| Outcome::Fail(e.to_string()))?;
    let pr_ref = core
        .pr_ref(&payload.pr)
        .await
        .ok_or_else(|| Outcome::Fail("pr not found".into()))?;
    let check = core
        .pr_check(&payload.pr, &payload.check)
        .await
        .ok_or_else(|| Outcome::Fail(format!("check {} not found", payload.check)))?;
    match core.0.deps.host.rerun_check(&pr_ref, &check).await {
        Ok(()) => {
            refresh_pr_detail(core, &payload.pr, &pr_ref).await;
            Ok(())
        }
        Err(e) => Err(classify_host_error(core, item, e).await),
    }
}

async fn do_triage(core: &Core, item: &OutboxItem) -> Result<(), Outcome> {
    let payload: theodore_contract::TriageRequest =
        serde_json::from_value(item.payload.clone()).map_err(|e| Outcome::Fail(e.to_string()))?;
    let pr_ref = core
        .pr_ref(&payload.pr)
        .await
        .ok_or_else(|| Outcome::Fail("pr not found".into()))?;
    let viewer = match core.0.deps.host.viewer().await {
        Ok(v) => v,
        Err(e) => return Err(classify_host_error(core, item, e).await),
    };

    match payload.action {
        theodore_contract::TriageAction::Take => {
            if let Err(e) = core
                .0
                .deps
                .host
                .request_reviewers(&pr_ref, std::slice::from_ref(&viewer.login), &[])
                .await
            {
                return Err(classify_host_error(core, item, e).await);
            }
            // Best-effort: a team review request that got the viewer here
            // in the first place doesn't block "take" from succeeding.
            for team in core.pr_requested_teams(&payload.pr).await {
                let _ = core.0.deps.host.remove_reviewer(&pr_ref, &team).await;
            }
        }
        theodore_contract::TriageAction::Decline => {
            if let Err(e) = core
                .0
                .deps
                .host
                .remove_reviewer(&pr_ref, &viewer.login)
                .await
            {
                return Err(classify_host_error(core, item, e).await);
            }
            core.mark_inbox_done(&payload.pr).await;
        }
        theodore_contract::TriageAction::Reassign { to } => {
            if let Err(e) = core.0.deps.host.request_reviewers(&pr_ref, &to, &[]).await {
                return Err(classify_host_error(core, item, e).await);
            }
            let _ = core
                .0
                .deps
                .host
                .remove_reviewer(&pr_ref, &viewer.login)
                .await;
        }
    }
    refresh_pr_detail(core, &payload.pr, &pr_ref).await;
    Ok(())
}

/// Opens a space's PR, idempotent by head branch (docs/spaces.md "Open
/// PR"): an open PR for the branch — from an earlier attempt that timed
/// out after the host made it — is linked instead of opening a second.
async fn do_create_space_pr(core: &Core, item: &OutboxItem) -> Result<(), Outcome> {
    let payload: crate::usecases::ship::CreateSpacePrPayload =
        serde_json::from_value(item.payload.clone()).map_err(|e| Outcome::Fail(e.to_string()))?;
    let pr_ref = open_or_find_pr(core, item, &payload.repo, &payload.pr).await?;
    core.space_pr_opened(&payload.space, &pr_ref).await;
    Ok(())
}

/// Opens the PR of a config saved to a host repo (docs/config.md
/// "Creating and saving the config"), idempotent by head branch the same
/// way: a second save pushed to the branch and finds the PR open.
async fn do_create_config_pr(core: &Core, item: &OutboxItem) -> Result<(), Outcome> {
    let payload: crate::usecases::config_editor::CreateConfigPrPayload =
        serde_json::from_value(item.payload.clone()).map_err(|e| Outcome::Fail(e.to_string()))?;
    let pr_ref = open_or_find_pr(core, item, &payload.repo, &payload.pr).await?;
    core.config_pr_opened(&payload.project, &pr_ref).await;
    Ok(())
}

/// The open PR from `pr.head`, opened if there is none.
async fn open_or_find_pr(
    core: &Core,
    item: &OutboxItem,
    repo: &theodore_domain::RepoRef,
    pr: &theodore_ports::NewPr,
) -> Result<PrRef, Outcome> {
    let host = &core.0.deps.host;
    let find = || host.find_open_pr(repo, &pr.head);
    let existing = match find().await {
        Ok(found) => found,
        // A host that can't look PRs up still gets one create attempt.
        Err(HostError::Unsupported(_)) => None,
        Err(e) => return Err(classify_host_error(core, item, e).await),
    };
    Ok(match existing {
        Some(pr_ref) => pr_ref,
        None => match host.create_pr(repo, pr).await {
            Ok(pr_ref) => pr_ref,
            Err(HostError::Conflict(msg)) => match find().await {
                Ok(Some(pr_ref)) => pr_ref,
                Ok(None) | Err(_) => return Err(Outcome::Fail(msg)),
            },
            Err(e) => return Err(classify_host_error(core, item, e).await),
        },
    })
}

async fn apply_submitted_review(
    core: &Core,
    payload: &SubmitReviewPayload,
    submitted: &SubmittedReview,
) {
    // Every draft of the payload is on the PR now, posted by this review or
    // found by its marker: archive them all, not only those whose thread the
    // host listed back (a thread read right after the write can miss it,
    // and a draft left behind went out again with the next submit).
    let mut actions = draft_archive_actions(payload.comments.iter().map(|c| c.draft_id.clone()));
    for (marker, thread_id) in &submitted.threads {
        let Some(c) = payload.comments.iter().find(|c| &c.marker == marker) else {
            continue;
        };
        let row = ThreadRow {
            pr: payload.pr.clone(),
            thread: theodore_domain::Thread {
                id: thread_id.clone(),
                anchor: Some(c.anchor.clone()),
                resolved: false,
                outdated: false,
                comments: vec![Comment {
                    id: CommentId::new(marker.clone()),
                    author: Person {
                        login: "me".into(),
                        name: None,
                        avatar_url: None,
                    },
                    body: c.body.clone(),
                    created_at: core.0.deps.clock.now(),
                    edited: false,
                    reactions: Vec::new(),
                }],
            },
        };
        let data = serde_json::to_value(&row).expect("serializable");
        actions.push(NewAction {
            model: models::THREAD.into(),
            model_id: thread_id.0.clone(),
            action: SyncActionCode::I,
            data: Some(data),
        });
    }
    let _ = core.apply(actions).await;
}

fn draft_archive_actions(ids: impl Iterator<Item = String>) -> Vec<NewAction> {
    ids.map(|id| NewAction {
        model: models::DRAFT.into(),
        model_id: id,
        action: SyncActionCode::A,
        data: None,
    })
    .collect()
}

async fn archive_drafts(core: &Core, ids: impl Iterator<Item = String>) {
    let _ = core.apply(draft_archive_actions(ids)).await;
}
