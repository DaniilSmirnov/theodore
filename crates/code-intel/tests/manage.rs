//! Managing what code intelligence keeps: limits applied live, the index's
//! size cap, removing a language, install progress, and the syntax hover.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Env, StaticGrammars, eventually, indexed, repo, status_of, workspace};
use pretty_assertions::assert_eq;
use sha2::{Digest, Sha256};
use theodore_code_intel::{CodeIntel, CodeIntelOptions};
use theodore_code_intel_toolchain::{Manifest, Toolchain};
use theodore_ports::code_intel::{
    CodeIntelLimits, CodeIntelPort, InstallItem, InstallProgress, InstallProgressState,
    InstallState, LanguageId, LspPolicy, Position, Source, TrustMode,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LIB: &str =
    "pub struct Store {}\n\npub fn helper(a: u32,\n          b: u32) -> u32 {\n    a + b\n}\n";
const MAIN: &str = "fn main() {\n    let n = helper(1, 2);\n}\n";

fn at(line: u32, col: u32) -> Position {
    Position { line, col }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hover_without_a_server_shows_the_definitions_signature() {
    let env = Env::new();
    let wt = repo(&[("src/lib.rs", LIB), ("src/main.rs", MAIN)]);
    let ci = env.facade(StaticGrammars::all());
    ci.open(workspace(wt.path(), TrustMode::Review))
        .await
        .unwrap();
    indexed(&ci, wt.path()).await;

    // `helper(1, 2)` in main.rs: its two-line signature, no body.
    let hover = ci
        .hover(wt.path(), "src/main.rs", at(2, 14))
        .await
        .unwrap()
        .expect("a hover");
    assert_eq!(hover.source, Source::Syntax);
    assert_eq!(
        hover.text,
        "```rust\npub fn helper(a: u32,\n          b: u32) -> u32\n```\n\n\
         _Matched by name, no language server: src/lib.rs:3_"
    );
    // Nothing defined by that name: no hover.
    assert_eq!(
        ci.hover(wt.path(), "src/main.rs", at(1, 1)).await.unwrap(),
        None
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn limits_apply_live_and_a_larger_file_limit_reindexes() {
    let env = Env::new();
    let big: String = (0..40)
        .map(|i| format!("pub fn f{i}() {{ let x = {i}; }}\n"))
        .collect();
    assert!(big.len() > 1024);
    let wt = repo(&[("src/big.rs", &big), ("src/lib.rs", LIB)]);
    let ci = env.facade(StaticGrammars::all());
    ci.set_limits(CodeIntelLimits {
        max_file_kb: 1,
        max_servers: 2,
        idle_minutes: 3,
        cache_mb: 64,
    })
    .await;
    let o = ci.options();
    assert_eq!(
        (o.max_servers, o.idle, o.max_file_kb, o.cache_bytes),
        (2, Duration::from_secs(180), 1, 64 << 20)
    );

    ci.open(workspace(wt.path(), TrustMode::Review))
        .await
        .unwrap();
    indexed(&ci, wt.path()).await;
    let syms = ci.file_symbols(wt.path(), "src/big.rs").await.unwrap();
    assert!(syms.items.is_empty(), "over the limit: not indexed");

    ci.set_limits(CodeIntelLimits {
        max_file_kb: 64,
        ..CodeIntelLimits::default()
    })
    .await;
    eventually("big.rs indexed", || async {
        let a = ci.file_symbols(wt.path(), "src/big.rs").await.unwrap();
        a.items.len() == 40
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cap_evicts_closed_worktrees_first_and_answers_still_come() {
    let env = Env::new();
    let gone = repo(&[("src/old.rs", "pub fn old_one() {}\n")]);
    let open = repo(&[("src/lib.rs", LIB), ("src/main.rs", MAIN)]);
    let ci = env.facade(StaticGrammars::all());
    ci.open(workspace(gone.path(), TrustMode::Review))
        .await
        .unwrap();
    indexed(&ci, gone.path()).await;
    ci.close(gone.path()).await;
    ci.open(workspace(open.path(), TrustMode::Review))
        .await
        .unwrap();
    indexed(&ci, open.path()).await;
    let (_, blobs) = ci.cache_usage().await.unwrap();
    assert_eq!(blobs, 3);

    // A cap of a byte: everything goes, the closed worktree's blob first.
    ci.set_options(CodeIntelOptions {
        cache_bytes: 1,
        ..ci.options()
    })
    .await;
    // (An eviction the set_options spawned may be the one that runs.)
    let seen: Arc<Mutex<(usize, usize)>> = Arc::default();
    eventually("evicted", || {
        let (ci, seen) = (ci.clone(), seen.clone());
        async move {
            let ev = ci.evict_cache().await.unwrap();
            {
                let mut s = seen.lock().unwrap();
                s.0 += ev.unused.len();
                s.1 += ev.in_use.len();
            }
            ci.cache_usage().await.unwrap().1 == 0
        }
    })
    .await;
    // Counted only by the calls made here; the spawned one may have done
    // part. Either way nothing is left, and the open worktree forgot them.
    let (unused, in_use) = *seen.lock().unwrap();
    assert!(unused <= 1 && in_use <= 2, "{unused} {in_use}");

    // The open worktree forgot them: not 100% indexed, the text search
    // fills in, and a query about a file indexes it again.
    let found = ci.find_symbol(open.path(), "helper", None).await.unwrap();
    assert!(!found.items.is_empty());
    let syms = ci.file_symbols(open.path(), "src/lib.rs").await.unwrap();
    assert_eq!(syms.source, Source::Syntax);
    assert!(syms.items.iter().any(|s| s.name == "helper"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_is_refused_while_a_worktree_uses_it_and_sticks_until_install() {
    let env = Env::new();
    let wt = repo(&[("src/lib.rs", LIB)]);
    let ci = env.facade(StaticGrammars::all());
    let rust = LanguageId::new("rust");
    ci.open(workspace(wt.path(), TrustMode::Review))
        .await
        .unwrap();
    let err = ci.remove(&rust).await.unwrap_err().to_string();
    assert!(err.contains("in use by an open worktree"), "{err}");

    ci.close(wt.path()).await;
    ci.remove(&rust).await.unwrap();
    let saved = std::fs::read_to_string(env.data.join("removed.json")).unwrap();
    assert!(saved.contains("\"rust\""), "{saved}");

    // Reopened: no grammar is fetched on its own; text search answers.
    ci.open(workspace(wt.path(), TrustMode::Review))
        .await
        .unwrap();
    eventually("grammar missing", || async {
        let a = ci.find_symbol(wt.path(), "helper", None).await.unwrap();
        a.note.is_some_and(|n| n.contains("rust: no grammar yet"))
    })
    .await;

    // Install takes it back: the grammar loads and the files index.
    ci.install(&rust, None).await.unwrap();
    indexed(&ci, wt.path()).await;
    let syms = ci.file_symbols(wt.path(), "src/lib.rs").await.unwrap();
    assert!(syms.items.iter().any(|s| s.name == "helper"));
    let saved = std::fs::read_to_string(env.data.join("removed.json")).unwrap();
    assert!(!saved.contains("\"rust\""), "{saved}");
}

const PLATFORM: &str = "test-plat";

/// Language `x` served by `srv`, downloaded from `uri`.
fn server_facade(env: &Env, uri: &str, body: &[u8]) -> CodeIntel {
    let sha = hex::encode(Sha256::digest(body));
    let yaml = format!(
        r#"
servers:
  - name: srv
    version: "1.0"
    detect: [["srv-not-on-path", "--version"]]
    download:
      {PLATFORM}: {{ url: "{uri}/srv", sha256: "{sha}", format: binary, bin: srv }}
    launch: ["{{bin}}", "--stdio"]
languages:
  - id: x
    globs: ["*.x"]
    servers: [srv]
"#
    );
    let manifest = Manifest::parse(&yaml, r#"{"schema":1,"grammars":[]}"#)
        .unwrap()
        .0;
    let tc = Toolchain::builder(&env.data)
        .manifest(manifest)
        .platform(PLATFORM)
        .search_path(env.bin.clone().into_os_string())
        .build();
    CodeIntel::builder(env.options(LspPolicy::default()))
        .toolchain(tc)
        .grammar_source(StaticGrammars::all())
        .build()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_reports_progress_and_remove_takes_the_server_back_to_missing() {
    let http = MockServer::start().await;
    let body = vec![b'#'; 2_000_000];
    Mock::given(method("GET"))
        .and(path("/srv"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&http)
        .await;
    let env = Env::new();
    let ci = server_facade(&env, &http.uri(), &body);
    let events: Arc<Mutex<Vec<InstallProgress>>> = Arc::default();
    let sink = events.clone();
    let x = LanguageId::new("x");
    ci.install(
        &x,
        Some(Arc::new(move |p: InstallProgress| {
            sink.lock().unwrap().push(p)
        })),
    )
    .await
    .unwrap();

    let events = events.lock().unwrap().clone();
    assert!(
        events
            .iter()
            .all(|e| e.language == x && e.what == InstallItem::Server && e.name == "srv")
    );
    let downloading: Vec<(u64, Option<u64>)> = events
        .iter()
        .filter_map(|e| match e.state {
            InstallProgressState::Downloading { received, total } => Some((received, total)),
            _ => None,
        })
        .collect();
    assert_eq!(downloading.first(), Some(&(0, Some(2_000_000))));
    assert_eq!(downloading.last(), Some(&(2_000_000, Some(2_000_000))));
    // A local download takes milliseconds: throttled to a handful, not
    // one per chunk.
    assert!(downloading.len() <= 4, "{downloading:?}");
    assert_eq!(
        events.last().map(|e| &e.state),
        Some(&InstallProgressState::Installed)
    );
    assert!(env.data.join("servers/srv/1.0/srv").is_file());

    // Remove: the download is gone and the state is Missing again.
    let wt = repo(&[("a.x", "x")]);
    ci.remove(&x).await.unwrap();
    assert!(!env.data.join("servers/srv").exists());
    ci.open(workspace(wt.path(), TrustMode::Trusted))
        .await
        .unwrap();
    let s = status_of(&ci, wt.path(), "x").await;
    assert_eq!(s.server.unwrap().install, InstallState::Missing);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_download_ends_in_failed() {
    let http = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/srv"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&http)
        .await;
    let env = Env::new();
    let ci = server_facade(&env, &http.uri(), b"never served");
    let events: Arc<Mutex<Vec<InstallProgress>>> = Arc::default();
    let sink = events.clone();
    let result = ci
        .install(
            &LanguageId::new("x"),
            Some(Arc::new(move |p: InstallProgress| {
                sink.lock().unwrap().push(p)
            })),
        )
        .await;
    assert!(result.is_err());
    let events = events.lock().unwrap().clone();
    assert!(
        matches!(&events[..], [InstallProgress { state: InstallProgressState::Failed { error }, .. }] if error.contains("404")),
        "{events:?}"
    );
}
