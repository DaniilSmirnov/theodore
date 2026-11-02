//! The background indexer: order, progress, updates, cancellation.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{grammar_for, language_of, provider, write_tree};
use pretty_assertions::assert_eq;
use theodore_code_intel_syntax::{
    GrammarProvider, Indexer, IndexerConfig, Snapshot, SyntaxIndex, file_entry,
};
use theodore_ports::code_intel::LanguageId;
use tokio::sync::watch;

/// A provider that holds `held` languages' grammars until the gate opens.
fn gated(held: &'static str) -> (GrammarProvider, watch::Sender<bool>) {
    let (tx, rx) = watch::channel(false);
    let p: GrammarProvider = Arc::new(move |lang: LanguageId| {
        let mut rx = rx.clone();
        Box::pin(async move {
            if lang.as_str() == held {
                let _ = rx.wait_for(|open| *open).await;
            }
            grammar_for(&lang)
        })
    });
    (p, tx)
}

fn config(workers: usize) -> IndexerConfig {
    IndexerConfig {
        workers,
        ..IndexerConfig::default()
    }
}

/// Paths in the order their blobs were stored (row ids grow).
fn index_order(db: &std::path::Path, snap: &Snapshot) -> Vec<String> {
    let conn = rusqlite::Connection::open(db).unwrap();
    let shas: Vec<String> = conn
        .prepare("SELECT sha FROM blobs ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    shas.iter()
        .flat_map(|s| snap.paths_for_blob(s).map(|f| f.path.clone()))
        .collect()
}

async fn eventually(mut check: impl FnMut() -> bool) {
    for _ in 0..500 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not reached");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_then_mentions_then_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let mut files: Vec<(String, String)> = (0..8)
        .map(|i| (format!("f{i}.rs"), format!("fn f{i}() {{}}\n")))
        .collect();
    files.push(("c.rs".into(), "fn c() { needle(); }\n".into()));
    files.push(("m.rs".into(), "fn m() {}\n".into()));
    files.push(("x.rs".into(), "fn x() { let needle = 1; }\n".into()));
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let snap = write_tree(dir.path(), &refs);
    let db = dir.path().join("index.sqlite");
    let (p, gate) = gated("rust");
    let indexer = Indexer::new(SyntaxIndex::open(&db).unwrap(), p, config(1));
    let ws = indexer.start(snap.clone());

    ws.prioritize_paths(&["m.rs".to_string()]);
    assert_eq!(ws.prioritize_mentions(&["needle".to_string()]).await, 2);
    assert_eq!(ws.progress().percent(), 0);
    gate.send_replace(true);
    ws.wait_idle().await;

    let order = index_order(&db, &snap);
    assert_eq!(order.len(), 11);
    assert_eq!(order[0], "m.rs");
    let mut mentions = order[1..3].to_vec();
    mentions.sort();
    assert_eq!(mentions, vec!["c.rs", "x.rs"]);
    let rest: Vec<String> = (0..8).map(|i| format!("f{i}.rs")).collect();
    assert_eq!(order[3..].to_vec(), rest);
    assert_eq!(ws.progress().percent(), 100);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn percent_counts_only_languages_with_a_grammar() {
    let dir = tempfile::tempdir().unwrap();
    let snap = write_tree(
        dir.path(),
        &[
            ("a.rs", "fn a() {}\n"),
            ("b.rs", "fn b() {}\n"),
            ("c.py", "def c(): pass\n"),
            ("d.cob", "IDENTIFICATION DIVISION.\n"),
            ("e.txt", "notes\n"),
        ],
    );
    let (p, gate) = gated("rust");
    let indexer = Indexer::new(SyntaxIndex::open_in_memory().unwrap(), p, config(2));
    let ws = indexer.start(snap);

    eventually(|| {
        let pr = ws.progress();
        pr.indexed == 1 && !pr.missing_languages.is_empty()
    })
    .await;
    let pr = ws.progress();
    // a.rs, b.rs (grammar on its way) and c.py; COBOL has none, text has
    // no language.
    assert_eq!((pr.indexed, pr.indexable, pr.percent()), (1, 3, 33));
    assert_eq!(pr.pending_languages, vec![LanguageId::new("rust")]);
    assert_eq!(pr.missing_languages, vec![LanguageId::new("cobol")]);

    gate.send_replace(true);
    ws.wait_idle().await;
    let pr = ws.progress();
    assert_eq!((pr.indexed, pr.indexable, pr.percent()), (3, 3, 100));
    assert!(pr.pending_languages.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edits_reindex_and_a_second_worktree_reuses_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let files = [
        ("src/a.rs", "pub fn alpha() {}\n"),
        ("src/b.rs", "pub fn beta() { alpha(); }\n"),
    ];
    let snap = write_tree(dir.path(), &files);
    let index = SyntaxIndex::open_in_memory().unwrap();
    let indexer = Indexer::new(index.clone(), provider(), config(2));
    let ws = indexer.start(snap);
    ws.wait_idle().await;
    let counts = index.counts().unwrap();

    // Another worktree of the same repo: every blob is already there.
    let other = tempfile::tempdir().unwrap();
    let ws2 = indexer.start(write_tree(other.path(), &files));
    ws2.wait_idle().await;
    assert_eq!(index.counts().unwrap(), counts);
    assert_eq!(ws2.progress().percent(), 100);

    // An agent edits a file: the new blob is indexed, the old one stays
    // for snapshots that still have it.
    std::fs::write(dir.path().join("src/a.rs"), "pub fn gamma() {}\n").unwrap();
    let e = file_entry(dir.path(), "src/a.rs", &language_of)
        .unwrap()
        .unwrap();
    ws.update_file(e);
    ws.wait_idle().await;
    let now = ws.snapshot();
    assert_eq!(
        index.find_definitions(&now, "gamma", None).unwrap().len(),
        1
    );
    assert!(
        index
            .find_definitions(&now, "alpha", None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        index
            .find_definitions(&ws2.snapshot(), "alpha", None)
            .unwrap()
            .len(),
        1
    );

    ws.remove_file("src/b.rs");
    assert!(ws.snapshot().get("src/b.rs").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn index_now_waits_for_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let files: Vec<(String, String)> = (0..200)
        .map(|i| (format!("f{i:03}.rs"), format!("fn f{i}() {{}}\n")))
        .collect();
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let snap = write_tree(dir.path(), &refs);
    let index = SyntaxIndex::open_in_memory().unwrap();
    let indexer = Indexer::new(index.clone(), provider(), config(1));
    let ws = indexer.start(snap);
    ws.index_now(&["f199.rs".to_string()]).await;
    let syms = index.file_symbols(&ws.snapshot(), "f199.rs").unwrap();
    assert_eq!(syms[0].name, "f199");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_stops_waiting_work() {
    let dir = tempfile::tempdir().unwrap();
    let snap = write_tree(dir.path(), &[("a.rs", "fn a() {}\n")]);
    let (p, _gate) = gated("rust");
    let index = SyntaxIndex::open_in_memory().unwrap();
    let indexer = Indexer::new(index.clone(), p, config(1));
    let ws = indexer.start(snap);
    ws.close();
    assert!(ws.is_closed());
    // Returns although the grammar never arrived.
    tokio::time::timeout(Duration::from_secs(5), ws.wait_idle())
        .await
        .unwrap();
    assert_eq!(index.counts().unwrap(), (0, 0));
}
