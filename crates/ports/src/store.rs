//! Store port (plan §3, §9): models as JSON rows + sync log + outbox.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SyncActionCode {
    /// insert
    I,
    /// full update
    U,
    /// partial update (changed fields only)
    C,
    /// archive
    A,
    /// delete
    D,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncAction {
    pub id: u64,
    pub model: String,
    pub model_id: String,
    pub action: SyncActionCode,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewAction {
    pub model: String,
    pub model_id: String,
    pub action: SyncActionCode,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRow {
    pub model: String,
    pub id: String,
    pub data: serde_json::Value,
    pub archived: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutboxStatus {
    Pending,
    InFlight,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboxItem {
    pub id: u64,
    pub op: String,
    pub payload: serde_json::Value,
    pub attempts: u32,
    pub status: OutboxStatus,
    pub not_before: i64,
    /// Why the last attempt failed (the host's own message where it gave
    /// one); `None` until an attempt fails.
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Db(String),
    #[error("serde: {0}")]
    Serde(String),
    #[error("migration: {0}")]
    Migration(String),
}

pub type StoreResult<T> = Result<T, StoreError>;

impl StoreError {
    /// The stable error code logged as `error.code` (docs/telemetry.md).
    pub fn code(&self) -> &'static str {
        match self {
            StoreError::Db(_) => "store.db",
            StoreError::Serde(_) => "store.serde",
            StoreError::Migration(_) => "store.migration",
        }
    }
}

/// Synchronous on purpose (rusqlite); `app` calls it via `spawn_blocking`.
pub trait Store: Send + Sync {
    /// Apply atomically: write model rows, append to the sync log, return the
    /// actions with their assigned ids.
    fn apply(&self, actions: Vec<NewAction>) -> StoreResult<Vec<SyncAction>>;
    fn last_sync_id(&self) -> StoreResult<u64>;
    fn since(&self, sync_id: u64, limit: u32) -> StoreResult<Vec<SyncAction>>;
    fn get(&self, model: &str, id: &str) -> StoreResult<Option<ModelRow>>;
    /// All rows of `model` whose JSON `data.<field>` equals `value` (e.g. pr id).
    fn query(&self, model: &str, field: &str, value: &str) -> StoreResult<Vec<ModelRow>>;
    fn all(&self, model: &str) -> StoreResult<Vec<ModelRow>>;

    fn outbox_push(&self, op: &str, payload: serde_json::Value) -> StoreResult<u64>;
    /// Next pending item with `not_before <= now`, marked in-flight.
    fn outbox_take(&self, now: i64) -> StoreResult<Option<OutboxItem>>;
    fn outbox_done(&self, id: u64) -> StoreResult<()>;
    /// Back to pending, due at `not_before`, one more attempt counted;
    /// `error` is why this attempt failed (kept as `last_error`).
    fn outbox_retry(&self, id: u64, not_before: i64, error: &str) -> StoreResult<()>;
    /// Given up for good: never taken again, not pending for `is_idle`.
    fn outbox_fail(&self, id: u64, error: &str) -> StoreResult<()>;

    /// `apply` plus one `outbox_push`, as a single atomic unit: a caller
    /// that both changes a model row and enqueues a host write for it (e.g.
    /// accepting a task: mark it `Done`, enqueue `resolve_thread`) must not
    /// land one half on a crash between the two calls. Default: the two
    /// calls in sequence, for implementations with no real transaction to
    /// share (e.g. an in-memory test fake, where there's nothing to
    /// interleave with and nothing to crash mid-way). `SqliteStore` runs
    /// both in one DB transaction.
    fn apply_with_outbox(
        &self,
        actions: Vec<NewAction>,
        op: &str,
        payload: serde_json::Value,
    ) -> StoreResult<(Vec<SyncAction>, u64)> {
        let applied = self.apply(actions)?;
        let outbox_id = self.outbox_push(op, payload)?;
        Ok((applied, outbox_id))
    }

    fn kv_get(&self, key: &str) -> StoreResult<Option<serde_json::Value>>;
    fn kv_set(&self, key: &str, value: &serde_json::Value) -> StoreResult<()>;

    /// Outbox items not yet `Done`/`Failed` (plan §3.4 `is_idle`: an update
    /// must not restart while a host write is still in flight). `0` by
    /// default; an implementation may override it when it can answer cheaply.
    fn outbox_pending_count(&self) -> StoreResult<u64> {
        Ok(0)
    }

    /// Whether an item `op` with exactly this `payload` waits, not yet
    /// taken (`pending`, a backoff included): a write that reads its state
    /// when it runs coalesces with one already queued. `false` by default:
    /// no coalescing, every push runs.
    fn outbox_has_pending(&self, op: &str, payload: &serde_json::Value) -> StoreResult<bool> {
        let _ = (op, payload);
        Ok(false)
    }

    /// The oldest sync log id still present, or `None` if the log is empty
    /// or this implementation never compacts it. `catch_up` compares its
    /// caller's `since` against this: a client standing further behind than
    /// the log now goes back can't be caught up incrementally and must
    /// re-bootstrap instead (see `compact`). `Ok(None)` by default, so a
    /// store that never compacts never triggers that path.
    fn oldest_sync_id(&self) -> StoreResult<Option<u64>> {
        Ok(None)
    }

    /// Drops sync log entries older than `sync_id`, returning the number of
    /// rows removed. A lower-level primitive `compact` is built on; `Ok(0)`
    /// by default (nothing to drop).
    fn compact_before(&self, sync_id: u64) -> StoreResult<u64> {
        let _ = sync_id;
        Ok(0)
    }

    /// Compaction policy: keeps whichever of the last `keep_last_n` sync ids
    /// or everything within `keep_duration_ms` of `now_ms` retains more
    /// rows, then removes everything older via `compact_before`. Returns
    /// the number of rows removed. `Ok(0)` by default — compaction is
    /// optional; an implementation with no sync log to bound (e.g. a test
    /// fake) can ignore it safely.
    fn compact(&self, keep_last_n: u64, keep_duration_ms: i64, now_ms: i64) -> StoreResult<u64> {
        let _ = (keep_last_n, keep_duration_ms, now_ms);
        Ok(0)
    }
}
