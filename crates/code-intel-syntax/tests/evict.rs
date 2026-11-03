//! The size cap: least recently used blobs go first, blobs of open
//! worktrees last; and the size limit for files changing while indexing.

mod common;

use std::collections::HashSet;
use std::time::Duration;

use common::{provider, rust, write_tree};
use pretty_assertions::assert_eq;
use theodore_code_intel_syntax::{
    DEFAULT_MAX_FILE_BYTES, Indexer, IndexerConfig, Snapshot, SyntaxIndex, blob_sha,
};

/// A Rust file with `n` functions: a few hundred bytes of tags each.
fn source(tag: &str, n: usize) -> String {
    (0..n)
        .map(|i| format!("fn {tag}_{i}() {{ {tag}_{}(); }}\n", i + 1))
        .collect()
}

/// Indexes three similar blobs a, b, c in that order, a few ms apart so
/// their `last_used` differ.
async fn three_blobs(idx: &SyntaxIndex) -> [String; 3] {
    let mut shas = Vec::new();
    for tag in ["a", "b", "c"] {
        let src = source(tag, 400);
        let sha = blob_sha(src.as_bytes());
        idx.index_blob(&sha, &rust(), src.as_bytes(), DEFAULT_MAX_FILE_BYTES)
            .unwrap();
        shas.push(sha);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    shas.try_into().unwrap()
}

fn present(idx: &SyntaxIndex, shas: &[String]) -> Vec<bool> {
    shas.iter().map(|s| idx.is_indexed(s).unwrap()).collect()
}

#[tokio::test]
async fn under_the_cap_nothing_goes() {
    let dir = tempfile::tempdir().unwrap();
    let idx = SyntaxIndex::open(&dir.path().join("index.sqlite")).unwrap();
    let shas = three_blobs(&idx).await;
    let used = idx.used_bytes().unwrap();
    let ev = idx.evict(used, &HashSet::new()).unwrap();
    assert!(ev.unused.is_empty() && ev.in_use.is_empty());
    assert_eq!(present(&idx, &shas), [true, true, true]);
}

#[tokio::test]
async fn the_least_recently_used_blob_goes_first() {
    let dir = tempfile::tempdir().unwrap();
    let idx = SyntaxIndex::open(&dir.path().join("index.sqlite")).unwrap();
    let [a, b, c] = three_blobs(&idx).await;
    // a was indexed first but is used now: b is the oldest.
    idx.touch([a.as_str()]).unwrap();

    let used = idx.used_bytes().unwrap();
    let ev = idx.evict(used - 1, &HashSet::new()).unwrap();
    assert_eq!(ev.unused, vec![b.clone()]);
    assert!(ev.in_use.is_empty());
    assert!(ev.bytes_after < ev.bytes_before, "{ev:?}");
    assert_eq!(
        present(&idx, &[a.clone(), b, c.clone()]),
        [true, false, true]
    );
    // Its tags went with it.
    let (blobs, _) = idx.counts().unwrap();
    assert_eq!(blobs, 2);
}

#[tokio::test]
async fn blobs_of_open_worktrees_go_last() {
    let dir = tempfile::tempdir().unwrap();
    let idx = SyntaxIndex::open(&dir.path().join("index.sqlite")).unwrap();
    let [a, b, c] = three_blobs(&idx).await;
    let open: HashSet<String> = [a.clone()].into();

    // Over by a little: the oldest blob nobody has open (b) goes, not a.
    let used = idx.used_bytes().unwrap();
    let ev = idx.evict(used - 1, &open).unwrap();
    assert_eq!(ev.unused, vec![b.clone()]);
    assert_eq!(
        present(&idx, &[a.clone(), b.clone(), c.clone()]),
        [true, false, true]
    );

    // A cap of nothing: the rest goes too, open blobs last and reported.
    let ev = idx.evict(0, &open).unwrap();
    assert_eq!(ev.unused, vec![c.clone()]);
    assert_eq!(ev.in_use, vec![a.clone()]);
    assert_eq!(present(&idx, &[a, b, c]), [false, false, false]);
}

#[tokio::test]
async fn the_file_shrinks_after_an_eviction() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    let idx = SyntaxIndex::open(&db).unwrap();
    three_blobs(&idx).await;
    let conn = rusqlite::Connection::open(&db).unwrap();
    let mode: i64 = conn
        .pragma_query_value(None, "auto_vacuum", |r| r.get(0))
        .unwrap();
    assert_eq!(mode, 2, "incremental");
    let pages = |c: &rusqlite::Connection| -> i64 {
        c.pragma_query_value(None, "page_count", |r| r.get(0))
            .unwrap()
    };
    let before = pages(&conn);
    idx.evict(0, &HashSet::new()).unwrap();
    assert!(pages(&conn) < before, "pages are given back");
}

#[tokio::test]
async fn an_older_schema_is_rebuilt_with_the_new_columns() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("index.sqlite");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE blobs (id INTEGER PRIMARY KEY, sha TEXT NOT NULL UNIQUE, \
             language TEXT NOT NULL, grammar TEXT NOT NULL, status INTEGER NOT NULL, \
             indexed_at INTEGER NOT NULL); \
             INSERT INTO blobs VALUES (1, 'x', 'rust', 'g', 0, 0); \
             PRAGMA user_version = 1;",
        )
        .unwrap();
    }
    let idx = SyntaxIndex::open(&db).unwrap();
    assert!(!idx.is_indexed("x").unwrap(), "dropped");
    let src = source("z", 3);
    idx.index_blob(
        &blob_sha(src.as_bytes()),
        &rust(),
        src.as_bytes(),
        DEFAULT_MAX_FILE_BYTES,
    )
    .unwrap();
    idx.touch([blob_sha(src.as_bytes()).as_str()]).unwrap();
}

async fn idle(ws: &theodore_code_intel_syntax::WorkspaceIndex) {
    tokio::time::timeout(Duration::from_secs(10), ws.wait_idle())
        .await
        .expect("idle");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_larger_size_limit_retries_skipped_files() {
    let dir = tempfile::tempdir().unwrap();
    let big = source("big", 50);
    let snap: Snapshot = write_tree(dir.path(), &[("big.rs", &big), ("small.rs", "fn s() {}\n")]);
    let idx = SyntaxIndex::open(&dir.path().join("index.sqlite")).unwrap();
    let indexer = Indexer::new(
        idx.clone(),
        provider(),
        IndexerConfig {
            max_file_bytes: 100,
            workers: 1,
        },
    );
    let ws = indexer.start(snap.clone());
    idle(&ws).await;
    assert!(idx.file_symbols(&snap, "big.rs").unwrap().is_empty());
    assert_eq!(idx.file_symbols(&snap, "small.rs").unwrap().len(), 1);

    indexer.set_max_file_bytes(1 << 20);
    assert_eq!(indexer.max_file_bytes(), 1 << 20);
    assert_eq!(ws.retry_too_large(), 1);
    idle(&ws).await;
    assert_eq!(idx.file_symbols(&snap, "big.rs").unwrap().len(), 50);
    assert_eq!(ws.retry_too_large(), 0, "nothing left to retry");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forgotten_blob_counts_as_not_indexed_until_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let snap = write_tree(
        dir.path(),
        &[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")],
    );
    let idx = SyntaxIndex::open(&dir.path().join("index.sqlite")).unwrap();
    let indexer = Indexer::new(idx.clone(), provider(), IndexerConfig::default());
    let ws = indexer.start(snap.clone());
    idle(&ws).await;
    assert_eq!(ws.progress().percent(), 100);

    let open: HashSet<String> = snap.files().iter().map(|f| f.blob_sha.clone()).collect();
    let ev = idx.evict(0, &open).unwrap();
    assert_eq!(ev.in_use.len(), 2);
    ws.forget(&ev.in_use);
    assert_eq!(ws.progress().indexed, 0);

    ws.index_now(&["a.rs".to_string()]).await;
    assert_eq!(idx.file_symbols(&snap, "a.rs").unwrap().len(), 1);
    assert_eq!(ws.progress().indexed, 1);
}
