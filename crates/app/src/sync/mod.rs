//! The sync engine (plan §3): `bootstrap`, `catch_up`, `transact`. Every
//! other write path in `app` (outbox results, the poller, agent runs) goes
//! through `Core::apply` too, so all of them share one sync-id sequence.

pub(crate) mod compactor;
#[cfg(test)]
mod test_support;
mod validate;

use std::sync::atomic::Ordering;

use theodore_contract::{
    Bootstrap, BootstrapScope, MutationOp, SyncFrame, Transaction, TransactionResult, models,
};
use theodore_ports::{NewAction, StoreError, SyncActionCode};

use crate::core::Core;
use crate::error::{AppError, AppResult};
use crate::wire::{to_wire_action, to_wire_row};

impl Core {
    pub async fn bootstrap(&self, scope: BootstrapScope) -> AppResult<Bootstrap> {
        let store = self.0.deps.store.clone();
        // Read before the rows: a write between them is then replayed by
        // `catch_up` from this id, not lost. Replaying one the rows already
        // hold ends up the same.
        let last_sync_id = {
            let store = store.clone();
            tokio::task::spawn_blocking(move || store.last_sync_id())
                .await
                .map_err(AppError::join)??
        };
        let rows = match scope {
            BootstrapScope::Global => tokio::task::spawn_blocking(move || {
                let mut out = Vec::new();
                for m in [
                    models::PR,
                    models::INBOX_STATE,
                    models::TASK,
                    models::SETTING,
                    models::PROJECT,
                    models::SPACE,
                ] {
                    out.extend(store.all(m)?);
                }
                Ok::<_, StoreError>(out)
            })
            .await
            .map_err(AppError::join)??,
            // A space's transcript (docs/spaces.md §7): chat rows only, and
            // nothing to open — the space is not a PR.
            BootstrapScope::Pr(pr) if pr.0.starts_with("space:") => {
                let key = pr.0.clone();
                tokio::task::spawn_blocking(move || store.query(models::CHAT_MESSAGE, "pr", &key))
                    .await
                    .map_err(AppError::join)??
            }
            BootstrapScope::Pr(pr) => {
                if !self.0.current_head.read().await.contains_key(&pr) {
                    let core = self.clone();
                    let pr = pr.clone();
                    tokio::spawn(async move {
                        if let Err(e) = core.ensure_open(&pr).await {
                            core.emit(theodore_contract::Ephemeral::Toast {
                                level: theodore_contract::ToastLevel::Error,
                                text: format!("Could not open {pr}: {e}"),
                            });
                        }
                    });
                }
                let pr_s = pr.0.clone();
                tokio::task::spawn_blocking(move || {
                    let mut out = Vec::new();
                    if let Some(row) = store.get(models::PR_DETAIL, &pr_s)? {
                        out.push(row);
                    }
                    if let Some(row) = store.get(models::GROUPING, &pr_s)? {
                        out.push(row);
                    }
                    for m in [
                        models::HUNK_READ,
                        models::FILE_VIEWED,
                        models::CHECK_TICK,
                        models::THREAD,
                        models::DRAFT,
                        models::FINDING,
                        models::SKILL_RUN,
                        models::CHAT_MESSAGE,
                    ] {
                        out.extend(store.query(m, "pr", &pr_s)?);
                    }
                    Ok::<_, StoreError>(out)
                })
                .await
                .map_err(AppError::join)??
            }
        };
        Ok(Bootstrap {
            last_sync_id,
            rows: rows.into_iter().map(to_wire_row).collect(),
        })
    }

    /// Replays `(since, last]` over IPC (the UI calls this after a missed
    /// `sync` event, per the event's doc). If `since` has fallen behind the
    /// sync log's compaction horizon (`Store::compact`), there's a gap
    /// `since` can no longer close incrementally — returns
    /// `resync_required` instead of silently skipping the missing ids.
    pub async fn catch_up(&self, since: u64) -> AppResult<SyncFrame> {
        let store = self.0.deps.store.clone();
        let oldest = {
            let store = store.clone();
            tokio::task::spawn_blocking(move || store.oldest_sync_id())
                .await
                .map_err(AppError::join)??
        };
        if let Some(oldest) = oldest
            && since + 1 < oldest
        {
            let last_sync_id = tokio::task::spawn_blocking({
                let store = store.clone();
                move || store.last_sync_id()
            })
            .await
            .map_err(AppError::join)??;
            return Ok(SyncFrame {
                from_sync_id: since,
                last_sync_id,
                actions: Vec::new(),
                resync_required: true,
            });
        }
        let actions = tokio::task::spawn_blocking(move || store.since(since, 5_000))
            .await
            .map_err(AppError::join)??;
        let last = actions.last().map(|a| a.id).unwrap_or(since);
        Ok(SyncFrame {
            from_sync_id: since,
            last_sync_id: last,
            actions: actions.into_iter().map(to_wire_action).collect(),
            resync_required: false,
        })
    }

    pub async fn transact(&self, tx: Transaction) -> TransactionResult {
        match self.transact_inner(tx).await {
            Ok(r) => r,
            Err(e) => TransactionResult::Rejected {
                reason: e.to_string(),
            },
        }
    }

    async fn transact_inner(&self, tx: Transaction) -> AppResult<TransactionResult> {
        let dedup_key = format!("tx:{}", tx.id);
        if let Some(last_sync_id) = self.dedup_lookup(&dedup_key).await? {
            return Ok(TransactionResult::Ok { last_sync_id });
        }

        for op in &tx.ops {
            if let Err(reason) = validate::validate_op(self, op).await {
                return Ok(TransactionResult::Rejected { reason });
            }
        }

        let actions: Vec<NewAction> = tx.ops.iter().map(op_to_new_action).collect();
        let applied = self.apply(actions).await?;
        let last_sync_id = applied
            .last()
            .map(|a| a.id)
            .unwrap_or_else(|| self.0.last_broadcast.load(Ordering::SeqCst));
        self.dedup_remember(&dedup_key, last_sync_id).await;

        crate::reading::handle_reads(self, &tx).await;

        Ok(TransactionResult::Ok { last_sync_id })
    }

    async fn dedup_lookup(&self, key: &str) -> AppResult<Option<u64>> {
        let store = self.0.deps.store.clone();
        let key = key.to_string();
        let cached = tokio::task::spawn_blocking(move || store.kv_get(&key))
            .await
            .map_err(AppError::join)??;
        Ok(cached.and_then(|v| serde_json::from_value::<u64>(v).ok()))
    }

    async fn dedup_remember(&self, key: &str, last_sync_id: u64) {
        let store = self.0.deps.store.clone();
        let key = key.to_string();
        let value = serde_json::json!(last_sync_id);
        let _ = tokio::task::spawn_blocking(move || store.kv_set(&key, &value)).await;
    }
}

fn op_to_new_action(op: &MutationOp) -> NewAction {
    match op {
        MutationOp::Create { model, id, data } => NewAction {
            model: model.clone(),
            model_id: id.clone(),
            action: SyncActionCode::I,
            data: Some(data.clone()),
        },
        // `data` holds changed fields only: a partial (`C`) update, not a
        // full replace (`U`, reserved for server-side overwrites).
        MutationOp::Update { model, id, data } => NewAction {
            model: model.clone(),
            model_id: id.clone(),
            action: SyncActionCode::C,
            data: Some(data.clone()),
        },
        MutationOp::Archive { model, id } => NewAction {
            model: model.clone(),
            model_id: id.clone(),
            action: SyncActionCode::A,
            data: None,
        },
        MutationOp::Delete { model, id } => NewAction {
            model: model.clone(),
            model_id: id.clone(),
            action: SyncActionCode::D,
            data: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use theodore_contract::BootstrapScope;
    use theodore_domain::PrId;
    use theodore_ports::{NewAction, Store, SyncActionCode};

    use super::test_support::sqlite_deps;
    use crate::core::Core;
    use crate::testkit::fixtures;

    fn put(store: &dyn Store, id: &str, n: i32) {
        store
            .apply(vec![NewAction {
                model: "setting".into(),
                model_id: id.into(),
                action: SyncActionCode::I,
                data: Some(serde_json::json!({ "n": n })),
            }])
            .unwrap();
    }

    /// rust-review #22: a client whose `since` falls below the sync log's
    /// compaction horizon would otherwise get a frame that silently skips
    /// the deleted ids (`since()`'s `WHERE id > since` just starts later).
    /// `catch_up` must flag this instead.
    #[tokio::test]
    async fn catch_up_flags_resync_once_since_is_behind_the_compaction_horizon() {
        let f = fixtures();
        let (deps, store) = sqlite_deps(&f);
        let (core, _recv) = Core::start(deps);

        for i in 0..5 {
            put(store.as_ref(), &format!("k{i}"), i);
        }
        // Drops ids 1..3, keeping 4 and 5 — oldest_sync_id becomes 4.
        store.compact_before(4).unwrap();

        let frame = core.catch_up(0).await.unwrap();
        assert!(
            frame.resync_required,
            "since=0 is missing ids 1..3, which no longer exist"
        );
        assert!(frame.actions.is_empty());
        assert_eq!(frame.from_sync_id, 0);
        assert_eq!(frame.last_sync_id, 5);
    }

    /// The boundary case: a client standing exactly at the compaction
    /// horizon's lower edge (`since == oldest - 1`) has already received
    /// everything that was dropped — `since()` picks up right where the
    /// log still goes back to, no gap, no resync needed.
    #[tokio::test]
    async fn catch_up_serves_a_normal_frame_right_at_the_compaction_boundary() {
        let f = fixtures();
        let (deps, store) = sqlite_deps(&f);
        let (core, _recv) = Core::start(deps);

        for i in 0..5 {
            put(store.as_ref(), &format!("k{i}"), i);
        }
        store.compact_before(4).unwrap(); // oldest_sync_id == 4

        let frame = core.catch_up(3).await.unwrap();
        assert!(!frame.resync_required);
        assert_eq!(frame.actions.len(), 2); // ids 4 and 5
        assert_eq!(frame.last_sync_id, 5);
    }

    /// A write landing while the rows are read: the bootstrap's id stands
    /// before it, so `catch_up` from that id still delivers it.
    #[tokio::test]
    async fn bootstrap_id_is_read_before_its_rows() {
        let f = fixtures();
        let (core, _recv) = Core::start(f.deps());

        let store = f.store.clone();
        f.store.before_next_read(Box::new(move || {
            put(store.as_ref(), "written-during-read", 1);
        }));
        let scope = BootstrapScope::Pr(PrId("space:s".into()));
        let boot = core.bootstrap(scope).await.unwrap();

        let frame = core.catch_up(boot.last_sync_id).await.unwrap();
        let ids: Vec<&str> = frame.actions.iter().map(|a| a.model_id.as_str()).collect();
        assert!(ids.contains(&"written-during-read"), "{ids:?}");
    }

    /// An implementation that never compacts (`oldest_sync_id` -> `None`
    /// by the `Store` trait's default) never triggers `resync_required`,
    /// however far behind `since` is.
    #[tokio::test]
    async fn catch_up_never_resyncs_against_a_store_that_does_not_compact() {
        let f = fixtures();
        let (core, _recv) = Core::start(f.deps()); // FakeStore: no compaction

        let frame = core.catch_up(0).await.unwrap();
        assert!(!frame.resync_required);
    }
}
