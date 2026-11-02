//! Sync log compaction (rust-review #22): `Store::compact_before` /
//! `oldest_sync_id` exist on the port but nothing ever called them. This
//! runs the retention policy at startup and then hourly, and `catch_up`
//! (in `sync/mod.rs`) consults `oldest_sync_id` to refuse a client that's
//! fallen behind the horizon instead of serving it a silent gap.
//! `Core::start` spawns `run` next to the outbox and the poller.

use std::sync::Arc;
use std::time::Duration;

use theodore_ports::{StoreError, StoreResult};

use crate::core::Inner;

/// Keep the last 10k sync ids or 7 days, whichever keeps more.
const KEEP_LAST_N: u64 = 10_000;
const KEEP_DURATION_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const INTERVAL_MS: i64 = 60 * 60 * 1000;
/// Real-time tick for noticing a `Clock`-measured deadline is due — same
/// pattern as the poller (see its `REAL_TICK_MS` doc comment): short and
/// wall-clock-based so a test's single `advance()` jump is never missed.
const REAL_TICK_MS: u64 = 1_000;

pub(crate) async fn run(inner: Arc<Inner>) {
    let mut next_due = inner.deps.clock.now();
    loop {
        if inner.shutdown.is_cancelled() {
            return;
        }
        if inner.deps.clock.now() >= next_due {
            let _ = compact_once(&inner).await;
            next_due = inner.deps.clock.now() + INTERVAL_MS;
        }
        tokio::select! {
            _ = inner.shutdown.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_millis(REAL_TICK_MS)) => {},
        }
    }
}

/// One compaction pass, split out from `run` so a test can call it
/// directly instead of waiting on the real-time tick loop.
pub(crate) async fn compact_once(inner: &Arc<Inner>) -> StoreResult<u64> {
    let store = inner.deps.store.clone();
    let now = inner.deps.clock.now();
    tokio::task::spawn_blocking(move || store.compact(KEEP_LAST_N, KEEP_DURATION_MS, now))
        .await
        .map_err(|e| StoreError::Db(e.to_string()))?
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use theodore_ports::{Clock, ModelRow, NewAction, OutboxItem, Store, StoreResult, SyncAction};

    use super::*;
    use crate::core::Core;
    use crate::testkit::fixtures;

    /// Delegates everything to `inner`, except `compact`: records every
    /// call's arguments instead of running them, so a test can assert
    /// *when* and *how often* the scheduler calls it without needing the
    /// policy's numbers to actually trigger a deletion.
    struct CountingStore {
        inner: Arc<dyn Store>,
        calls: StdMutex<Vec<(u64, i64, i64)>>,
    }

    impl Store for CountingStore {
        fn apply(&self, actions: Vec<NewAction>) -> StoreResult<Vec<SyncAction>> {
            self.inner.apply(actions)
        }
        fn last_sync_id(&self) -> StoreResult<u64> {
            self.inner.last_sync_id()
        }
        fn since(&self, sync_id: u64, limit: u32) -> StoreResult<Vec<SyncAction>> {
            self.inner.since(sync_id, limit)
        }
        fn get(&self, model: &str, id: &str) -> StoreResult<Option<ModelRow>> {
            self.inner.get(model, id)
        }
        fn query(&self, model: &str, field: &str, value: &str) -> StoreResult<Vec<ModelRow>> {
            self.inner.query(model, field, value)
        }
        fn all(&self, model: &str) -> StoreResult<Vec<ModelRow>> {
            self.inner.all(model)
        }
        fn outbox_push(&self, op: &str, payload: serde_json::Value) -> StoreResult<u64> {
            self.inner.outbox_push(op, payload)
        }
        fn outbox_take(&self, now: i64) -> StoreResult<Option<OutboxItem>> {
            self.inner.outbox_take(now)
        }
        fn outbox_done(&self, id: u64) -> StoreResult<()> {
            self.inner.outbox_done(id)
        }
        fn outbox_retry(&self, id: u64, not_before: i64, error: &str) -> StoreResult<()> {
            self.inner.outbox_retry(id, not_before, error)
        }
        fn outbox_fail(&self, id: u64, error: &str) -> StoreResult<()> {
            self.inner.outbox_fail(id, error)
        }
        fn kv_get(&self, key: &str) -> StoreResult<Option<serde_json::Value>> {
            self.inner.kv_get(key)
        }
        fn kv_set(&self, key: &str, value: &serde_json::Value) -> StoreResult<()> {
            self.inner.kv_set(key, value)
        }
        fn compact(
            &self,
            keep_last_n: u64,
            keep_duration_ms: i64,
            now_ms: i64,
        ) -> StoreResult<u64> {
            self.calls
                .lock()
                .unwrap()
                .push((keep_last_n, keep_duration_ms, now_ms));
            Ok(0)
        }
    }

    #[tokio::test]
    async fn compact_once_calls_store_compact_with_the_policy_constants() {
        let f = fixtures();
        let spy = Arc::new(CountingStore {
            inner: f.store.clone(),
            calls: StdMutex::new(Vec::new()),
        });
        let mut deps = f.deps();
        deps.store = spy.clone();
        let (core, _recv) = Core::start(deps);
        let inner = core.0.clone();

        compact_once(&inner).await.unwrap();

        // `Core::start` also runs a startup pass; every pass uses the policy.
        let seen = spy.calls.lock().unwrap().clone();
        assert!(!seen.is_empty());
        assert!(
            seen.iter()
                .all(|c| *c == (KEEP_LAST_N, KEEP_DURATION_MS, f.clock.now()))
        );
    }

    /// Paused Tokio time: `run`'s real-time tick (`REAL_TICK_MS`) would
    /// otherwise make this test wait out real hours to see the scheduled
    /// passes. `time::advance` moves the paused clock (and fires any timer
    /// due by then) without sleeping; `FakeClock::advance` moves the
    /// `Clock` port `run` compares `next_due` against — the two are
    /// independent and both need to move forward.
    #[tokio::test(start_paused = true)]
    async fn run_compacts_at_startup_then_waits_for_the_next_hour() {
        let f = fixtures();
        let spy = Arc::new(CountingStore {
            inner: f.store.clone(),
            calls: StdMutex::new(Vec::new()),
        });
        let mut deps = f.deps();
        deps.store = spy.clone();
        // `Core::start` spawns `run` itself.
        let (core, _recv) = Core::start(deps);
        let inner = core.0.clone();
        wait_for_call_count(&spy, 1).await;

        // Advancing by less than an hour must not trigger a second pass.
        f.clock.advance(INTERVAL_MS - 1);
        tokio::time::advance(Duration::from_millis(REAL_TICK_MS * 2)).await;
        assert_eq!(
            spy.calls.lock().unwrap().len(),
            1,
            "too soon for the next pass"
        );

        // Crossing the hour mark triggers the next pass.
        f.clock.advance(2);
        wait_for_call_count(&spy, 2).await;

        inner.shutdown.cancel();
    }

    /// `tokio::spawn` only schedules `run`; the executor needs to actually
    /// poll it (at least once, to reach its first `select!`) before a
    /// `time::advance` that happens to land before that poll would see no
    /// effect. Retries in small steps rather than a single large advance
    /// so this can't itself overshoot past the next scheduled pass.
    ///
    /// `compact` itself runs on the blocking pool, a real thread that paused
    /// time doesn't wait for: each step also gives it a little wall-clock
    /// time, bounded by a real deadline rather than a step count, so a
    /// loaded CI runner can't exhaust the steps before the thread records
    /// its call.
    async fn wait_for_call_count(spy: &CountingStore, expected: usize) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut steps = 0;
        while std::time::Instant::now() < deadline {
            tokio::task::yield_now().await;
            if spy.calls.lock().unwrap().len() >= expected {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
            // Paused time only needs to cover one tick of `run`'s loop.
            if steps < 50 {
                steps += 1;
                tokio::time::advance(Duration::from_millis(REAL_TICK_MS / 10 + 1)).await;
            }
        }
        panic!(
            "compact was not called {expected} time(s) in time; saw {:?}",
            spy.calls.lock().unwrap()
        );
    }
}
