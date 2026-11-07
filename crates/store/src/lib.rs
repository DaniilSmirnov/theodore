//! Store over SQLite (rusqlite): model rows, sync log, outbox, kv.
//!
//! `SqliteStore` wraps a single `rusqlite::Connection` behind a `Mutex`: the
//! port is synchronous by design (plan §9), and one connection serializes
//! writes without needing SQLite's own locking to arbitrate between threads.

mod migrations;

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use theodore_ports::{
    ModelRow, NewAction, OutboxItem, OutboxStatus, Store, StoreError, StoreResult, SyncAction,
    SyncActionCode,
};

pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    pub fn open<P: AsRef<Path>>(path: P) -> StoreResult<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| StoreError::Db(format!("create dir: {e}")))?;
        }
        let mut conn = Connection::open(path).map_err(de)?;
        configure(&conn)?;
        migrations::migrate(&mut conn, Some(path))?;
        reset_inflight(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn open_in_memory() -> StoreResult<Self> {
        let mut conn = Connection::open_in_memory().map_err(de)?;
        configure(&conn)?;
        migrations::migrate(&mut conn, None)?;
        reset_inflight(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> StoreResult<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| StoreError::Db("store mutex poisoned".into()))
    }

    /// Transaction de-dup: has this transaction id already been applied?
    pub fn seen_tx(&self, id: &str) -> StoreResult<bool> {
        let conn = self.lock()?;
        let found: Option<i64> = conn
            .query_row("SELECT 1 FROM tx_seen WHERE id = ?1", params![id], |r| {
                r.get(0)
            })
            .optional()
            .map_err(de)?;
        Ok(found.is_some())
    }

    /// Record a transaction id as applied. Idempotent.
    pub fn mark_tx(&self, id: &str) -> StoreResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR IGNORE INTO tx_seen (id, at) VALUES (?1, ?2)",
            params![id, now_ms()],
        )
        .map_err(de)?;
        Ok(())
    }
}

/// The body of `apply`, run inside a caller-supplied transaction so
/// `apply_with_outbox` can share one with its outbox insert.
fn apply_actions_in_tx(
    tx: &rusqlite::Transaction<'_>,
    actions: Vec<NewAction>,
) -> StoreResult<Vec<SyncAction>> {
    let now = now_ms();
    let mut out = Vec::with_capacity(actions.len());

    for action in actions {
        match action.action {
            SyncActionCode::I => {
                let data = require_data(&action)?;
                let text = serde_json::to_string(data).map_err(se)?;
                tx.execute(
                        "INSERT INTO models (model, id, data, archived, updated_sync_id) VALUES (?1, ?2, ?3, 0, 0)
                         ON CONFLICT (model, id) DO UPDATE SET data = excluded.data, archived = 0",
                        params![action.model, action.model_id, text],
                    )
                    .map_err(de)?;
            }
            SyncActionCode::U => {
                let data = require_data(&action)?;
                let text = serde_json::to_string(data).map_err(se)?;
                tx.execute(
                        "INSERT INTO models (model, id, data, archived, updated_sync_id) VALUES (?1, ?2, ?3, 0, 0)
                         ON CONFLICT (model, id) DO UPDATE SET data = excluded.data",
                        params![action.model, action.model_id, text],
                    )
                    .map_err(de)?;
            }
            SyncActionCode::C => {
                let data = require_data(&action)?;
                let text = serde_json::to_string(data).map_err(se)?;
                let changed = tx
                        .execute(
                            "UPDATE models SET data = json_patch(data, ?3) WHERE model = ?1 AND id = ?2",
                            params![action.model, action.model_id, text],
                        )
                        .map_err(de)?;
                if changed == 0 {
                    return Err(StoreError::Db(format!(
                        "merge-patch: no row for {}:{}",
                        action.model, action.model_id
                    )));
                }
            }
            SyncActionCode::A => {
                let changed = tx
                    .execute(
                        "UPDATE models SET archived = 1 WHERE model = ?1 AND id = ?2",
                        params![action.model, action.model_id],
                    )
                    .map_err(de)?;
                if changed == 0 {
                    return Err(StoreError::Db(format!(
                        "archive: no row for {}:{}",
                        action.model, action.model_id
                    )));
                }
            }
            SyncActionCode::D => {
                tx.execute(
                    "DELETE FROM models WHERE model = ?1 AND id = ?2",
                    params![action.model, action.model_id],
                )
                .map_err(de)?;
            }
        }

        let log_data_text: Option<String> = action
            .data
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(se)?;
        tx.execute(
            "INSERT INTO sync_log (model, model_id, action, data, at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                action.model,
                action.model_id,
                code_str(action.action),
                log_data_text,
                now
            ],
        )
        .map_err(de)?;
        // Monotonic, gap-free across commits: AUTOINCREMENT's counter update is
        // part of this same transaction, so a rollback un-reserves the id too.
        let log_id = tx.last_insert_rowid() as u64;

        if action.action != SyncActionCode::D {
            tx.execute(
                "UPDATE models SET updated_sync_id = ?1 WHERE model = ?2 AND id = ?3",
                params![log_id as i64, action.model, action.model_id],
            )
            .map_err(de)?;
        }

        out.push(SyncAction {
            id: log_id,
            model: action.model,
            model_id: action.model_id,
            action: action.action,
            data: action.data,
        });
    }

    Ok(out)
}

impl Store for SqliteStore {
    fn apply(&self, actions: Vec<NewAction>) -> StoreResult<Vec<SyncAction>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(de)?;
        let out = apply_actions_in_tx(&tx, actions)?;
        tx.commit().map_err(de)?;
        Ok(out)
    }

    fn apply_with_outbox(
        &self,
        actions: Vec<NewAction>,
        op: &str,
        payload: serde_json::Value,
    ) -> StoreResult<(Vec<SyncAction>, u64)> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(de)?;
        let out = apply_actions_in_tx(&tx, actions)?;
        let now = now_ms();
        let text = serde_json::to_string(&payload).map_err(se)?;
        tx.execute(
            "INSERT INTO outbox (op, payload, attempts, status, not_before, created_at, updated_at)
             VALUES (?1, ?2, 0, 'pending', 0, ?3, ?3)",
            params![op, text, now],
        )
        .map_err(de)?;
        let outbox_id = tx.last_insert_rowid() as u64;
        tx.commit().map_err(de)?;
        Ok((out, outbox_id))
    }

    fn last_sync_id(&self) -> StoreResult<u64> {
        let conn = self.lock()?;
        let id: i64 = conn
            .query_row("SELECT COALESCE(MAX(id), 0) FROM sync_log", [], |r| {
                r.get(0)
            })
            .map_err(de)?;
        Ok(id as u64)
    }

    fn since(&self, sync_id: u64, limit: u32) -> StoreResult<Vec<SyncAction>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT id, model, model_id, action, data FROM sync_log WHERE id > ?1 ORDER BY id ASC LIMIT ?2")
            .map_err(de)?;
        let rows = stmt
            .query_map(params![sync_id as i64, limit], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(de)?;

        let mut out = Vec::new();
        for row in rows {
            let (id, model, model_id, action, data) = row.map_err(de)?;
            let action = code_from_str(&action)?;
            let data = data
                .map(|s| serde_json::from_str(&s))
                .transpose()
                .map_err(se)?;
            out.push(SyncAction {
                id: id as u64,
                model,
                model_id,
                action,
                data,
            });
        }
        Ok(out)
    }

    fn get(&self, model: &str, id: &str) -> StoreResult<Option<ModelRow>> {
        let conn = self.lock()?;
        let row: Option<(String, String, String, bool)> = conn
            .query_row(
                "SELECT model, id, data, archived FROM models WHERE model = ?1 AND id = ?2",
                params![model, id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(de)?;
        row.map(row_to_model).transpose()
    }

    fn query(&self, model: &str, field: &str, value: &str) -> StoreResult<Vec<ModelRow>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT model, id, data, archived FROM models
                 WHERE model = ?1 AND CAST(json_extract(data, '$.' || ?2) AS TEXT) = ?3",
            )
            .map_err(de)?;
        let rows = stmt
            .query_map(params![model, field, value], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .map_err(de)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row_to_model(row.map_err(de)?)?);
        }
        Ok(out)
    }

    fn all(&self, model: &str) -> StoreResult<Vec<ModelRow>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT model, id, data, archived FROM models WHERE model = ?1")
            .map_err(de)?;
        let rows = stmt
            .query_map(params![model], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .map_err(de)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row_to_model(row.map_err(de)?)?);
        }
        Ok(out)
    }

    fn outbox_push(&self, op: &str, payload: serde_json::Value) -> StoreResult<u64> {
        let conn = self.lock()?;
        let now = now_ms();
        let text = serde_json::to_string(&payload).map_err(se)?;
        conn.execute(
            "INSERT INTO outbox (op, payload, attempts, status, not_before, created_at, updated_at)
             VALUES (?1, ?2, 0, 'pending', 0, ?3, ?3)",
            params![op, text, now],
        )
        .map_err(de)?;
        Ok(conn.last_insert_rowid() as u64)
    }

    fn outbox_take(&self, now: i64) -> StoreResult<Option<OutboxItem>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(de)?;
        let found = tx
            .query_row(
                "SELECT id, op, payload, attempts, not_before, last_error FROM outbox
                 WHERE status = 'pending' AND not_before <= ?1 ORDER BY id ASC LIMIT 1",
                params![now],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(de)?;
        let Some((id, op, payload, attempts, not_before, last_error)) = found else {
            return Ok(None);
        };
        tx.execute(
            "UPDATE outbox SET status = 'in_flight', updated_at = ?2 WHERE id = ?1",
            params![id, now],
        )
        .map_err(de)?;
        tx.commit().map_err(de)?;
        let payload: serde_json::Value = serde_json::from_str(&payload).map_err(se)?;
        Ok(Some(OutboxItem {
            id: id as u64,
            op,
            payload,
            attempts: attempts as u32,
            status: OutboxStatus::InFlight,
            not_before,
            last_error,
        }))
    }

    fn outbox_done(&self, id: u64) -> StoreResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE outbox SET status = 'done', updated_at = ?2 WHERE id = ?1",
            params![id as i64, now_ms()],
        )
        .map_err(de)?;
        Ok(())
    }

    fn outbox_retry(&self, id: u64, not_before: i64, error: &str) -> StoreResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE outbox SET attempts = attempts + 1, status = 'pending', not_before = ?2, last_error = ?4, updated_at = ?3 WHERE id = ?1",
            params![id as i64, not_before, now_ms(), error],
        )
        .map_err(de)?;
        Ok(())
    }

    fn outbox_fail(&self, id: u64, error: &str) -> StoreResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE outbox SET status = 'failed', last_error = ?3, updated_at = ?2 WHERE id = ?1",
            params![id as i64, now_ms(), error],
        )
        .map_err(de)?;
        Ok(())
    }

    fn outbox_has_pending(&self, op: &str, payload: &serde_json::Value) -> StoreResult<bool> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT payload FROM outbox WHERE status = 'pending' AND op = ?1")
            .map_err(de)?;
        let rows = stmt
            .query_map(params![op], |r| r.get::<_, String>(0))
            .map_err(de)?;
        for row in rows {
            let text = row.map_err(de)?;
            if serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .as_ref()
                == Some(payload)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn outbox_pending_count(&self) -> StoreResult<u64> {
        let conn = self.lock()?;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM outbox WHERE status IN ('pending', 'in_flight')",
                [],
                |r| r.get(0),
            )
            .map_err(de)?;
        Ok(n as u64)
    }

    /// The oldest sync log id still present, or `None` if the log is empty.
    fn oldest_sync_id(&self) -> StoreResult<Option<u64>> {
        let conn = self.lock()?;
        let v: Option<i64> = conn
            .query_row("SELECT MIN(id) FROM sync_log", [], |r| r.get(0))
            .map_err(de)?;
        Ok(v.map(|v| v as u64))
    }

    /// Drop sync log entries older than `sync_id` to keep the log bounded.
    /// Returns the number of rows removed.
    ///
    /// After this, `since(id, _)` can no longer serve a client whose
    /// `lastSyncId` is below the oldest id still kept (see
    /// `oldest_sync_id`) — that client must discard its local copy and
    /// bootstrap again from `all`/`get` instead of trying to catch up.
    fn compact_before(&self, sync_id: u64) -> StoreResult<u64> {
        let conn = self.lock()?;
        let changed = conn
            .execute(
                "DELETE FROM sync_log WHERE id < ?1",
                params![sync_id as i64],
            )
            .map_err(de)?;
        Ok(changed as u64)
    }

    /// Keeps whichever of the last `keep_last_n` ids or everything within
    /// `keep_duration_ms` of `now_ms` retains more rows: computes both
    /// cutoffs and compacts before the smaller one (deleting less), then
    /// delegates to `compact_before`.
    fn compact(&self, keep_last_n: u64, keep_duration_ms: i64, now_ms: i64) -> StoreResult<u64> {
        let conn = self.lock()?;
        let last: i64 = conn
            .query_row("SELECT COALESCE(MAX(id), 0) FROM sync_log", [], |r| {
                r.get(0)
            })
            .map_err(de)?;
        // Keep the last `keep_last_n` ids: delete everything below this.
        let by_count_cutoff = (last - keep_last_n as i64 + 1).max(0);
        // Keep everything at or after `min_at`: the smallest id still that
        // recent is the cutoff. No row qualifies (the whole log is older
        // than the window) means this policy alone would drop everything —
        // `last + 1`, so `min()` below defers to the count policy instead.
        let min_at = now_ms.saturating_sub(keep_duration_ms);
        let by_age_cutoff: Option<i64> = conn
            .query_row(
                "SELECT MIN(id) FROM sync_log WHERE at >= ?1",
                params![min_at],
                |r| r.get(0),
            )
            .map_err(de)?;
        let by_age_cutoff = by_age_cutoff.unwrap_or(last + 1);
        let cutoff = by_count_cutoff.min(by_age_cutoff);
        drop(conn);
        if cutoff <= 0 {
            return Ok(0);
        }
        self.compact_before(cutoff as u64)
    }

    fn kv_get(&self, key: &str) -> StoreResult<Option<serde_json::Value>> {
        let conn = self.lock()?;
        let text: Option<String> = conn
            .query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()
            .map_err(de)?;
        text.map(|t| serde_json::from_str(&t))
            .transpose()
            .map_err(se)
    }

    fn kv_set(&self, key: &str, value: &serde_json::Value) -> StoreResult<()> {
        let conn = self.lock()?;
        let text = serde_json::to_string(value).map_err(se)?;
        conn.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, text],
        )
        .map_err(de)?;
        Ok(())
    }
}

fn row_to_model(
    (model, id, data, archived): (String, String, String, bool),
) -> StoreResult<ModelRow> {
    let data = serde_json::from_str(&data).map_err(se)?;
    Ok(ModelRow {
        model,
        id,
        data,
        archived,
    })
}

fn require_data(action: &NewAction) -> StoreResult<&serde_json::Value> {
    action
        .data
        .as_ref()
        .ok_or_else(|| StoreError::Db(format!("{:?} action requires data", action.action)))
}

fn code_str(code: SyncActionCode) -> &'static str {
    match code {
        SyncActionCode::I => "I",
        SyncActionCode::U => "U",
        SyncActionCode::C => "C",
        SyncActionCode::A => "A",
        SyncActionCode::D => "D",
    }
}

fn code_from_str(s: &str) -> StoreResult<SyncActionCode> {
    match s {
        "I" => Ok(SyncActionCode::I),
        "U" => Ok(SyncActionCode::U),
        "C" => Ok(SyncActionCode::C),
        "A" => Ok(SyncActionCode::A),
        "D" => Ok(SyncActionCode::D),
        other => Err(StoreError::Db(format!("unknown sync action code: {other}"))),
    }
}

fn configure(conn: &Connection) -> StoreResult<()> {
    conn.busy_timeout(std::time::Duration::from_millis(5_000))
        .map_err(de)?;
    // No-op (reports "memory") on an in-memory connection; fine either way.
    let _: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .map_err(de)?;
    conn.pragma_update(None, "foreign_keys", true).map_err(de)?;
    Ok(())
}

/// Crash recovery: anything left `in_flight` from a previous run was never
/// confirmed done, so it goes back to `pending` and will be retried. Bump
/// `attempts` too: the item really was handed to a worker before (it may
/// have landed its side effect right before the crash), so op handlers that
/// key "did I already do this?" off `attempts == 0` must not treat this
/// re-pickup as a fresh first try.
fn reset_inflight(conn: &Connection) -> StoreResult<()> {
    conn.execute(
        "UPDATE outbox SET status = 'pending', attempts = attempts + 1 WHERE status = 'in_flight'",
        [],
    )
    .map_err(de)?;
    Ok(())
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) fn de(e: rusqlite::Error) -> StoreError {
    StoreError::Db(e.to_string())
}

pub(crate) fn se(e: serde_json::Error) -> StoreError {
    StoreError::Serde(e.to_string())
}
