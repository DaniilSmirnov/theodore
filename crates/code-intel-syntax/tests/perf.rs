//! Speed sanity check on this repository's own Rust code. Ignored by
//! default: `cargo test -p theodore-code-intel-syntax --release --test perf
//! -- --ignored --nocapture`.

mod common;

use std::path::Path;
use std::time::Instant;

use common::provider;
use theodore_code_intel_syntax::{Indexer, IndexerConfig, Snapshot, SyntaxIndex, git_snapshot};
use theodore_ports::code_intel::LanguageId;

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn index_this_repos_crates() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repo = repo.canonicalize().unwrap();
    let t = Instant::now();
    let lang =
        |p: &str| (p.starts_with("crates/") && p.ends_with(".rs")).then(|| LanguageId::new("rust"));
    let snap = git_snapshot(&repo, &lang).unwrap();
    let snapshot_ms = t.elapsed().as_millis();
    let rust_files = snap.files().iter().filter(|f| f.language.is_some()).count();
    let bytes: u64 = snap
        .files()
        .iter()
        .filter(|f| f.language.is_some())
        .filter_map(|f| std::fs::metadata(repo.join(&f.path)).ok())
        .map(|m| m.len())
        .sum();

    let dir = tempfile::tempdir().unwrap();
    let index = SyntaxIndex::open(&dir.path().join("index.sqlite")).unwrap();
    let indexer = Indexer::new(index.clone(), provider(), IndexerConfig::default());

    let t = Instant::now();
    let ws = indexer.start(snap.clone());
    ws.wait_idle().await;
    let cold = t.elapsed();
    let (blobs, tags) = index.counts().unwrap();
    assert_eq!(ws.progress().percent(), 100);

    let t = Instant::now();
    let ws2 = indexer.start(Snapshot::new(snap.root(), snap.files().to_vec()));
    ws2.wait_idle().await;
    let warm = t.elapsed();

    let t = Instant::now();
    let defs = index.find_definitions(&snap, "new", None).unwrap();
    let refs = index
        .references(&snap, "crates/app/src/lib.rs", "new", 100)
        .unwrap();
    let query = t.elapsed();

    println!(
        "snapshot {snapshot_ms} ms; {rust_files} files, {} KiB; cold {} ms ({blobs} blobs, {tags} tags, {} workers); warm {} ms; find_definitions(new) {} + references {} in {} ms",
        bytes / 1024,
        cold.as_millis(),
        IndexerConfig::default().workers,
        warm.as_millis(),
        defs.len(),
        refs.len(),
        query.as_millis()
    );
}
