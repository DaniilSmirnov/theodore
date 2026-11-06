//! Core end-to-end tests for outbox retries/failures, interdiff, sync
//! catch-up and restart persistence. Same patterns as `e2e.rs`: the real
//! `app` + `domain` driven via `theodore_runtime::start` on the fixtures.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use theodore_app::{Core, Receivers};
use theodore_contract::{
    BootstrapScope, Ephemeral, FileViewedRow, HunkRead, InterdiffClass, InterdiffRequest,
    MutationOp, OpenPrRequest, RevisionRequest, SubmitReviewRequest, ToastLevel, Transaction,
    TransactionResult, models,
};
use theodore_domain::{
    Anchor, Author, Draft, DraftId, Grouping, HostKind, HunkId, PrId, PrRef, RepoRef, ReviewEvent,
    Sha, Side, Thread, ViewedState,
};
use theodore_fixtures::{FixtureHost, ManualClock, Triggers};
use theodore_ports::host::HostError;
use theodore_runtime::{Runtime, RuntimeConfig};
use tokio::sync::broadcast::error::TryRecvError;

const PR_URL: &str = "https://github.com/octo/webhooks/pull/101";
const DELIVER_HUNK: &str = "1443083b5def4ff9";
const BACKOFF_HUNK_V1: &str = "d54b711f20787c68";
const BACKOFF_HUNK_V2: &str = "299c547d8e70c961";
const TEST_HUNK: &str = "aca3d6e37ea345d7";
const HEAD_1: &str = "head-479-1";
const HEAD_2: &str = "head-479-2";

fn demo_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/data/demo")
}

fn demo_pr_ref() -> PrRef {
    PrRef {
        repo: RepoRef {
            host: HostKind::Github,
            owner: "octo".into(),
            name: "webhooks".into(),
        },
        number: 101,
        host_id: Some("gh_101".into()),
    }
}

async fn start_demo_in(data_dir: &Path) -> Runtime {
    let cfg = RuntimeConfig {
        data_dir: data_dir.to_path_buf(),
        fixtures: Some(demo_dir()),
        sandbox: None,
        build_env: theodore_runtime::BuildEnv::Dev,
    };
    theodore_runtime::start(&cfg).await.expect("runtime starts")
}

struct Env {
    core: Core,
    recv: Receivers,
    triggers: Triggers,
    clock: ManualClock,
    host: std::sync::Arc<FixtureHost>,
    watcher: SyncWatcher,
    dir: tempfile::TempDir,
}

async fn env() -> Env {
    let dir = tempfile::tempdir().expect("tempdir");
    let rt = start_demo_in(dir.path()).await;
    Env {
        core: rt.core,
        recv: rt.receivers,
        triggers: rt.triggers.expect("triggers"),
        clock: rt.fixture_clock.expect("clock"),
        host: rt.fixture_host.expect("host"),
        watcher: SyncWatcher { next_from: 0 },
        dir,
    }
}

struct SyncWatcher {
    next_from: u64,
}

impl SyncWatcher {
    fn drain(&mut self, recv: &mut Receivers) {
        loop {
            match recv.sync.try_recv() {
                Ok(frame) => {
                    assert_eq!(
                        frame.from_sync_id, self.next_from,
                        "sync frame gap: {}..{} does not continue from {}",
                        frame.from_sync_id, frame.last_sync_id, self.next_from
                    );
                    self.next_from = frame.last_sync_id;
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => return,
                Err(TryRecvError::Lagged(n)) => panic!("sync receiver lagged by {n}"),
            }
        }
    }
}

async fn wait_for<T, F, Fut>(recv: &mut Receivers, watcher: &mut SyncWatcher, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    for _ in 0..500 {
        watcher.drain(recv);
        if let Some(v) = probe().await {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not met within the timeout");
}

async fn pr_rows<T: serde::de::DeserializeOwned>(core: &Core, pr: &PrId, model: &str) -> Vec<T> {
    let Ok(bootstrap) = core.bootstrap(BootstrapScope::Pr(pr.clone())).await else {
        return Vec::new();
    };
    bootstrap
        .rows
        .into_iter()
        .filter(|r| r.model == model)
        .filter_map(|r| serde_json::from_value(r.data).ok())
        .collect()
}

async fn draft_archived_flags(core: &Core, pr: &PrId) -> Vec<bool> {
    core.bootstrap(BootstrapScope::Pr(pr.clone()))
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter(|r| r.model == models::DRAFT)
        .map(|r| r.archived)
        .collect()
}

fn calls(host: &FixtureHost, method: &str) -> usize {
    host.calls().iter().filter(|c| c.method == method).count()
}

fn hunk_read_tx(n: &str, pr: &PrId, hunk: &str) -> Transaction {
    Transaction {
        id: n.into(),
        ops: vec![MutationOp::Create {
            model: models::HUNK_READ.into(),
            id: format!("{}:{}", pr.0, hunk),
            data: serde_json::to_value(HunkRead {
                pr: pr.clone(),
                hunk: HunkId::new(hunk),
            })
            .unwrap(),
        }],
    }
}

/// Opens PR #101 and waits for the head-1 grouping.
async fn open_pr(e: &mut Env) -> PrId {
    let resp = e
        .core
        .open_pr(OpenPrRequest { url: PR_URL.into() })
        .await
        .expect("open_pr");
    assert_eq!(resp.head, Sha::new(HEAD_1));
    let pr = resp.pr.clone();
    let _: Grouping = wait_for(&mut e.recv, &mut e.watcher, || {
        let core = e.core.clone();
        let pr = pr.clone();
        async move {
            pr_rows::<Grouping>(&core, &pr, models::GROUPING)
                .await
                .into_iter()
                .next()
        }
    })
    .await;
    pr
}

/// Creates the one comment draft on `deliver.ts`.
async fn create_draft(e: &Env, pr: &PrId) {
    let draft = Draft {
        id: DraftId::new(format!("{}:d1", pr.0)),
        pr: pr.clone(),
        anchor: Anchor {
            rev: Sha::new(HEAD_1),
            path: "src/webhooks/deliver.ts".into(),
            side: Side::New,
            line: 5,
            start_line: None,
            start_side: None,
        },
        body: "Consider logging which branch this retry takes.".into(),
        source: Author::Me,
        outdated: false,
    };
    let result = e
        .core
        .transact(Transaction {
            id: "draft-1".into(),
            ops: vec![MutationOp::Create {
                model: models::DRAFT.into(),
                id: draft.id.0.clone(),
                data: serde_json::to_value(&draft).unwrap(),
            }],
        })
        .await;
    assert!(matches!(result, TransactionResult::Ok { .. }), "{result:?}");
}

async fn submit(e: &Env, pr: &PrId) {
    e.core
        .submit_review(SubmitReviewRequest {
            pr: pr.clone(),
            event: ReviewEvent::Comment,
            body: "LGTM with one note.".into(),
        })
        .await
        .expect("submit_review");
}

/// Reads backoff.ts's hunk and waits for the `set_viewed` call.
async fn read_backoff(e: &mut Env, pr: &PrId) {
    let r = e
        .core
        .transact(hunk_read_tx("read-backoff", pr, BACKOFF_HUNK_V1))
        .await;
    assert!(matches!(r, TransactionResult::Ok { .. }), "{r:?}");
}

async fn wait_calls(e: &mut Env, method: &'static str, n: usize) {
    let host = e.host.clone();
    wait_for(&mut e.recv, &mut e.watcher, || {
        let ok = calls(&host, method) >= n;
        async move { ok.then_some(()) }
    })
    .await;
}

async fn wait_threads(e: &mut Env, pr: &PrId) -> Vec<Thread> {
    wait_for(&mut e.recv, &mut e.watcher, || {
        let core = e.core.clone();
        let pr = pr.clone();
        async move {
            let rows = pr_rows::<Thread>(&core, &pr, models::THREAD).await;
            (!rows.is_empty()).then_some(rows)
        }
    })
    .await
}

async fn open_and_review(e: &mut Env) -> PrId {
    let pr = open_pr(e).await;
    read_backoff(e, &pr).await;
    wait_calls(e, "set_viewed", 1).await;
    create_draft(e, &pr).await;
    submit(e, &pr).await;
    wait_threads(e, &pr).await;
    // the draft is archived once the submission lands
    wait_for(&mut e.recv, &mut e.watcher, || {
        let core = e.core.clone();
        let pr = pr.clone();
        async move {
            let f = draft_archived_flags(&core, &pr).await;
            (f == vec![true]).then_some(())
        }
    })
    .await;
    pr
}

#[tokio::test]
async fn submit_review_network_error_keeps_draft_and_retries_after_backoff() {
    let mut e = env().await;
    let pr = open_pr(&mut e).await;
    create_draft(&e, &pr).await;
    e.triggers
        .fail_host("submit_review", vec![HostError::Network("down".into())]);
    submit(&e, &pr).await;

    wait_calls(&mut e, "submit_review", 1).await;
    // Let the outbox record the failure, then check nothing was lost.
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        e.watcher.drain(&mut e.recv);
    }
    assert_eq!(
        calls(&e.host, "submit_review"),
        1,
        "no retry before backoff"
    );
    assert!(
        pr_rows::<Thread>(&e.core, &pr, models::THREAD)
            .await
            .is_empty()
    );
    assert_eq!(draft_archived_flags(&e.core, &pr).await, vec![false]);

    e.clock.advance(2_100);
    wait_calls(&mut e, "submit_review", 2).await;
    let threads = wait_threads(&mut e, &pr).await;
    assert_eq!(threads.len(), 1);
    wait_for(&mut e.recv, &mut e.watcher, || {
        let core = e.core.clone();
        let pr = pr.clone();
        async move { (draft_archived_flags(&core, &pr).await == vec![true]).then_some(()) }
    })
    .await;
    assert_eq!(calls(&e.host, "submit_review"), 2);
    assert!(
        calls(&e.host, "find_marker") >= 1,
        "retry must look for its marker first: {:?}",
        e.host.calls()
    );
}

#[tokio::test]
async fn submit_review_validation_error_fails_permanently_with_a_toast() {
    let mut e = env().await;
    let pr = open_pr(&mut e).await;
    create_draft(&e, &pr).await;
    e.triggers
        .fail_host("submit_review", vec![HostError::Validation("nope".into())]);
    submit(&e, &pr).await;

    // Wait for the error toast on the ephemeral channel.
    let mut text = None;
    for _ in 0..500 {
        e.watcher.drain(&mut e.recv);
        loop {
            match e.recv.ephemeral.try_recv() {
                Ok(Ephemeral::Toast {
                    level: ToastLevel::Error,
                    text: t,
                }) if t.contains("nope") => {
                    text = Some(t);
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        if text.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(text.is_some(), "expected an error toast containing 'nope'");

    e.clock.advance(600_000);
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        e.watcher.drain(&mut e.recv);
    }
    assert_eq!(
        calls(&e.host, "submit_review"),
        1,
        "validation is not retried"
    );
    assert!(
        pr_rows::<Thread>(&e.core, &pr, models::THREAD)
            .await
            .is_empty()
    );
    assert_eq!(draft_archived_flags(&e.core, &pr).await, vec![false]);
}

#[tokio::test]
async fn set_viewed_network_error_keeps_local_state_and_retries() {
    let mut e = env().await;
    let pr = open_pr(&mut e).await;
    e.triggers
        .fail_host("set_viewed", vec![HostError::Network("down".into())]);
    read_backoff(&mut e, &pr).await;
    wait_calls(&mut e, "set_viewed", 1).await;

    let viewed = |core: Core, pr: PrId| async move {
        pr_rows::<FileViewedRow>(&core, &pr, models::FILE_VIEWED)
            .await
            .into_iter()
            .find(|r| r.path == "src/webhooks/backoff.ts" && r.state == ViewedState::Viewed)
    };
    assert!(viewed(e.core.clone(), pr.clone()).await.is_some());
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        e.watcher.drain(&mut e.recv);
    }
    assert_eq!(calls(&e.host, "set_viewed"), 1);
    assert!(viewed(e.core.clone(), pr.clone()).await.is_some());

    e.clock.advance(2_100);
    wait_calls(&mut e, "set_viewed", 2).await;
    assert!(viewed(e.core.clone(), pr.clone()).await.is_some());
}

#[tokio::test]
async fn interdiff_after_author_push_classifies_hunks() {
    let mut e = env().await;
    let pr = open_and_review(&mut e).await;
    e.triggers
        .author_push(&demo_pr_ref(), Sha::new(HEAD_2))
        .expect("author_push");
    e.clock.advance(15_000);
    wait_for(&mut e.recv, &mut e.watcher, || {
        let core = e.core.clone();
        let pr = pr.clone();
        async move {
            pr_rows::<Grouping>(&core, &pr, models::GROUPING)
                .await
                .into_iter()
                .find(|g| g.rev == Sha::new(HEAD_2))
        }
    })
    .await;

    let entries = e
        .core
        .interdiff(InterdiffRequest {
            pr: pr.clone(),
            base_sha: Sha::new(HEAD_1),
            file: None,
        })
        .await
        .expect("interdiff");
    let find = |h: &str| {
        entries
            .iter()
            .find(|x| x.hunk == HunkId::new(h))
            .unwrap_or_else(|| panic!("no entry for {h}: {entries:?}"))
    };
    assert_eq!(find(DELIVER_HUNK).class, InterdiffClass::Unchanged);
    let changed = find(BACKOFF_HUNK_V2);
    assert_eq!(changed.class, InterdiffClass::Changed);
    assert_eq!(changed.previous, Some(HunkId::new(BACKOFF_HUNK_V1)));
    assert_eq!(find(TEST_HUNK).class, InterdiffClass::Unchanged);
    let _ = e
        .core
        .get_revision(RevisionRequest {
            pr,
            head: Sha::new(HEAD_2),
        })
        .await
        .expect("head 2 revision");
}

#[tokio::test]
async fn catch_up_replays_a_contiguous_frame_with_the_thread() {
    let mut e = env().await;
    let pr = open_and_review(&mut e).await;
    let frame = e.core.catch_up(0).await.expect("catch_up");
    assert!(!frame.resync_required);
    assert_eq!(frame.from_sync_id, 0);
    let ids: Vec<u64> = frame.actions.iter().map(|a| a.id).collect();
    assert!(!ids.is_empty());
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "ids ascend: {ids:?}");
    assert_eq!(frame.last_sync_id, *ids.last().unwrap());
    assert!(frame.actions.iter().any(|a| a.model == models::THREAD));
    // Continuing from the tail yields nothing new.
    let tail = e.core.catch_up(frame.last_sync_id).await.unwrap();
    assert!(tail.actions.is_empty());
    let _ = pr;
}

// Ignored: fixture mode (`fixture_adapters` in src/lib.rs) uses
// `SqliteStore::open_in_memory()`, so nothing is persisted under `data_dir` and a
// second runtime on the same dir starts empty. Enable once fixture mode opens
// `data_dir/theodore.db`.
#[tokio::test]
#[ignore = "fixture runtime uses an in-memory store; no persistence across restarts"]
async fn state_survives_a_restart_on_the_same_data_dir() {
    let mut e = env().await;
    let pr = open_and_review(&mut e).await;
    let Env { core, dir, .. } = e;
    core.shutdown();
    drop(core);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let rt = start_demo_in(dir.path()).await;
    let core = rt.core;
    let threads = pr_rows::<Thread>(&core, &pr, models::THREAD).await;
    assert_eq!(threads.len(), 1);
    assert_eq!(draft_archived_flags(&core, &pr).await, vec![true]);
    let reads = pr_rows::<HunkRead>(&core, &pr, models::HUNK_READ).await;
    assert!(reads.iter().any(|r| r.hunk == HunkId::new(BACKOFF_HUNK_V1)));
    let viewed = pr_rows::<FileViewedRow>(&core, &pr, models::FILE_VIEWED).await;
    assert!(
        viewed
            .iter()
            .any(|r| r.path == "src/webhooks/backoff.ts" && r.state == ViewedState::Viewed)
    );
}
