//! `index.sqlite`: tags by git blob sha. A cache, not app state — on a
//! schema change, or a file that is not a database, it is dropped and
//! rebuilt.
//!
//! ```text
//! blobs  id · sha (unique) · language · grammar fingerprint · status · indexed_at
//!   └─< tags  blob · name · kind · syntax · is_def · range · name range · container
//!             (by name, by blob)
//! ```
//!
//! One row per blob: indexing a blob again under a new grammar fingerprint
//! replaces its tags.

use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use theodore_ports::code_intel::{Position, Range};

use crate::error::Result;
use crate::extract::{Extracted, extract};
use crate::grammar::{CompiledGrammar, Grammar, GrammarCache, kind_str};

/// Bump when the schema or what a row means changes: older files are
/// dropped and rebuilt on open.
const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE blobs (
    id         INTEGER PRIMARY KEY,
    sha        TEXT NOT NULL UNIQUE,
    language   TEXT NOT NULL,
    grammar    TEXT NOT NULL,
    status     INTEGER NOT NULL,
    indexed_at INTEGER NOT NULL
);
CREATE TABLE tags (
    blob            INTEGER NOT NULL REFERENCES blobs(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    kind            TEXT NOT NULL,
    syntax          TEXT NOT NULL,
    is_def          INTEGER NOT NULL,
    start_line      INTEGER NOT NULL,
    start_col       INTEGER NOT NULL,
    end_line        INTEGER NOT NULL,
    end_col         INTEGER NOT NULL,
    name_start_line INTEGER NOT NULL,
    name_start_col  INTEGER NOT NULL,
    name_end_line   INTEGER NOT NULL,
    name_end_col    INTEGER NOT NULL,
    container       TEXT
);
CREATE INDEX tags_by_name ON tags(name);
CREATE INDEX tags_by_blob ON tags(blob);
";

/// How far a blob got. Skipped blobs are recorded too, so they are not
/// read again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlobStatus {
    Indexed = 0,
    TooLarge = 1,
    Binary = 2,
}

impl BlobStatus {
    fn from_i64(v: i64) -> Self {
        match v {
            1 => Self::TooLarge,
            2 => Self::Binary,
            _ => Self::Indexed,
        }
    }
}

/// What [`SyntaxIndex::index_blob`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexOutcome {
    /// Already indexed under this grammar: nothing parsed.
    Cached,
    Indexed {
        tags: usize,
    },
    /// Over the size limit; recorded, retried only under a larger limit.
    TooLarge,
    /// A NUL byte in the first 8 KiB; recorded, never parsed.
    Binary,
}

/// The tags index. Cheap to clone; every clone shares one connection.
/// Methods block (SQLite, parsing): call them from a blocking thread.
#[derive(Clone)]
pub struct SyntaxIndex {
    conn: Arc<Mutex<Connection>>,
    grammars: Arc<GrammarCache>,
}

/// One stored tag with its blob's sha.
#[derive(Debug, Clone)]
pub(crate) struct TagRow {
    pub sha: String,
    pub name: String,
    pub kind: String,
    pub is_def: bool,
    pub range: Range,
    pub name_range: Range,
    pub container: Option<String>,
}

const TAG_COLUMNS: &str = "b.sha, t.name, t.kind, t.is_def, \
     t.start_line, t.start_col, t.end_line, t.end_col, \
     t.name_start_line, t.name_start_col, t.name_end_line, t.name_end_col, t.container";

impl SyntaxIndex {
    /// Opens (or creates) the index at `path`. A file from another schema
    /// version is emptied; one that is not a database is replaced.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = match Self::connect(path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "code-intel index unreadable, rebuilding");
                for suffix in ["", "-wal", "-shm"] {
                    let mut p = path.as_os_str().to_owned();
                    p.push(suffix);
                    let _ = std::fs::remove_file(p);
                }
                Self::connect(path)?
            }
        };
        Ok(Self::from_conn(conn))
    }

    /// An index that lives only as long as the value (tests, one-offs).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::prepare(&conn)?;
        Ok(Self::from_conn(conn))
    }

    fn from_conn(conn: Connection) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
            grammars: Arc::default(),
        }
    }

    fn connect(path: &Path) -> Result<Connection> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::prepare(&conn)?;
        Ok(conn)
    }

    /// Creates the schema, or drops everything and recreates it when the
    /// stored `user_version` is another one.
    fn prepare(conn: &Connection) -> Result<()> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == SCHEMA_VERSION {
            return Ok(());
        }
        let tables: Vec<String> = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        conn.pragma_update(None, "foreign_keys", "OFF")?;
        for t in tables {
            conn.execute_batch(&format!("DROP TABLE IF EXISTS \"{}\";", t.replace('"', "")))?;
        }
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `grammar`'s tags configuration, compiled once per language and
    /// fingerprint.
    pub fn compile(&self, grammar: &Grammar) -> Result<Arc<CompiledGrammar>> {
        self.grammars.get(grammar)
    }

    /// Indexes one blob: skipped when it is already indexed under this
    /// grammar, when `source` is over `max_bytes`, or binary.
    pub fn index_blob(
        &self,
        sha: &str,
        grammar: &Grammar,
        source: &[u8],
        max_bytes: usize,
    ) -> Result<IndexOutcome> {
        let compiled = self.compile(grammar)?;
        self.index_compiled(sha, &compiled, source, max_bytes, None)
    }

    /// [`index_blob`](Self::index_blob) with a compiled grammar and a
    /// cancellation flag (non-zero aborts the parse).
    pub fn index_compiled(
        &self,
        sha: &str,
        grammar: &CompiledGrammar,
        source: &[u8],
        max_bytes: usize,
        cancel: Option<&AtomicUsize>,
    ) -> Result<IndexOutcome> {
        if let Some(status) = self.status(sha, grammar)?
            && (status != BlobStatus::TooLarge || source.len() > max_bytes)
        {
            return Ok(IndexOutcome::Cached);
        }
        if source.len() > max_bytes {
            return self.record_skip(sha, grammar, BlobStatus::TooLarge);
        }
        if is_binary(source) {
            return self.record_skip(sha, grammar, BlobStatus::Binary);
        }
        let tags = extract(grammar, source, cancel)?;
        self.write(sha, grammar, BlobStatus::Indexed, &tags)
    }

    /// Records a blob that is not parsed (too large, binary) so it is not
    /// read again.
    pub(crate) fn record_skip(
        &self,
        sha: &str,
        grammar: &CompiledGrammar,
        status: BlobStatus,
    ) -> Result<IndexOutcome> {
        self.write(sha, grammar, status, &[])
    }

    /// The blob's status under this grammar; `None` when it is not indexed
    /// or was indexed under another fingerprint.
    pub(crate) fn status(
        &self,
        sha: &str,
        grammar: &CompiledGrammar,
    ) -> Result<Option<BlobStatus>> {
        let conn = self.lock();
        Self::status_in(&conn, sha, &grammar.fingerprint)
    }

    fn status_in(conn: &Connection, sha: &str, fingerprint: &str) -> Result<Option<BlobStatus>> {
        let v: Option<i64> = conn
            .prepare_cached("SELECT status FROM blobs WHERE sha = ?1 AND grammar = ?2")?
            .query_row(params![sha, fingerprint], |r| r.get(0))
            .optional()?;
        Ok(v.map(BlobStatus::from_i64))
    }

    /// Whether `sha` is indexed (or recorded as skipped) under any grammar.
    pub fn is_indexed(&self, sha: &str) -> Result<bool> {
        let conn = self.lock();
        let n: i64 = conn
            .prepare_cached("SELECT COUNT(*) FROM blobs WHERE sha = ?1")?
            .query_row([sha], |r| r.get(0))?;
        Ok(n > 0)
    }

    /// Blobs and tags stored, for status and tests.
    pub fn counts(&self) -> Result<(usize, usize)> {
        let conn = self.lock();
        let blobs: i64 = conn.query_row("SELECT COUNT(*) FROM blobs", [], |r| r.get(0))?;
        let tags: i64 = conn.query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0))?;
        Ok((blobs as usize, tags as usize))
    }

    fn write(
        &self,
        sha: &str,
        grammar: &CompiledGrammar,
        status: BlobStatus,
        tags: &[Extracted],
    ) -> Result<IndexOutcome> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        // Another worker may have indexed the same blob (two paths, one
        // content) while this one parsed.
        if Self::status_in(&tx, sha, &grammar.fingerprint)? == Some(status) {
            return Ok(IndexOutcome::Cached);
        }
        tx.execute(
            "DELETE FROM tags WHERE blob IN (SELECT id FROM blobs WHERE sha = ?1)",
            [sha],
        )?;
        tx.execute("DELETE FROM blobs WHERE sha = ?1", [sha])?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        tx.execute(
            "INSERT INTO blobs (sha, language, grammar, status, indexed_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                sha,
                grammar.language_id.as_str(),
                grammar.fingerprint,
                status as i64,
                now
            ],
        )?;
        let blob = tx.last_insert_rowid();
        {
            let mut ins = tx.prepare_cached(
                "INSERT INTO tags (blob, name, kind, syntax, is_def, \
                 start_line, start_col, end_line, end_col, \
                 name_start_line, name_start_col, name_end_line, name_end_col, container) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            )?;
            for t in tags {
                ins.execute(params![
                    blob,
                    t.name,
                    kind_str(t.kind),
                    t.syntax,
                    t.is_def,
                    t.range.start.line,
                    t.range.start.col,
                    t.range.end.line,
                    t.range.end.col,
                    t.name_range.start.line,
                    t.name_range.start.col,
                    t.name_range.end.line,
                    t.name_range.end.col,
                    t.container,
                ])?;
            }
        }
        tx.commit()?;
        Ok(match status {
            BlobStatus::Indexed => IndexOutcome::Indexed { tags: tags.len() },
            BlobStatus::TooLarge => IndexOutcome::TooLarge,
            BlobStatus::Binary => IndexOutcome::Binary,
        })
    }

    /// Every tag named `name` in any blob (callers keep the snapshot's).
    pub(crate) fn tags_named(&self, name: &str, defs_only: bool) -> Result<Vec<TagRow>> {
        let sql = format!(
            "SELECT {TAG_COLUMNS} FROM tags t JOIN blobs b ON b.id = t.blob \
             WHERE t.name = ?1 {}",
            if defs_only { "AND t.is_def = 1" } else { "" }
        );
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map([name], tag_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every tag of one blob, in source order.
    pub(crate) fn tags_of_blob(&self, sha: &str) -> Result<Vec<TagRow>> {
        let sql = format!(
            "SELECT {TAG_COLUMNS} FROM tags t JOIN blobs b ON b.id = t.blob \
             WHERE b.sha = ?1 ORDER BY t.start_line, t.start_col"
        );
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map([sha], tag_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

fn tag_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TagRow> {
    let pos = |l: usize, c: usize| -> rusqlite::Result<Position> {
        Ok(Position {
            line: r.get(l)?,
            col: r.get(c)?,
        })
    };
    Ok(TagRow {
        sha: r.get(0)?,
        name: r.get(1)?,
        kind: r.get(2)?,
        is_def: r.get(3)?,
        range: Range {
            start: pos(4, 5)?,
            end: pos(6, 7)?,
        },
        name_range: Range {
            start: pos(8, 9)?,
            end: pos(10, 11)?,
        },
        container: r.get(12)?,
    })
}

/// Git's own heuristic: a NUL byte in the first 8 KiB.
pub(crate) fn is_binary(source: &[u8]) -> bool {
    memchr::memchr(0, &source[..source.len().min(8192)]).is_some()
}
