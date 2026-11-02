//! The client and pool against `fake-lsp` (src/bin/fake-lsp.rs), a scripted
//! server whose answers come from the documents' text.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use theodore_code_intel_lsp::{
    LaunchConfig, LspPool, LspSession, PositionEncoding, SessionOptions,
};
use theodore_ports::{
    CodeIntelError, CodeSymbolKind, DiagnosticSeverity, LanguageId, Location, Position, Range,
    ServerState, TextEdit,
};

/// Line 5 has two crabs (2 UTF-16 units, 4 bytes each) and a Cyrillic
/// letter (1 unit, 2 bytes) before `target` and `bad`.
const LIB: &str = "struct Шар {\n    fn катится() {}\n}\nfn target() {}\nfn main() { let s = \"🦀🦀ё\"; target(); bad(); }\n";
const OTHER: &str = "// ёлка 🎄\nfn other() { target(); }\n";

fn config(env: &[(&str, &str)]) -> LaunchConfig {
    LaunchConfig {
        server: "fake-lsp".into(),
        argv: vec![env!("CARGO_BIN_EXE_fake-lsp").into()],
        env: env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        init_options: json!({"fake": {"a": 1, "nested": {"key": "v"}}, "top": true}),
        workspace_folders: false,
        language_ids: vec![(LanguageId::new("rust"), "rust".into(), vec!["rs".into()])],
    }
}

fn options() -> SessionOptions {
    SessionOptions {
        request_timeout: Duration::from_secs(5),
        init_timeout: Duration::from_secs(5),
        ready_settle: Duration::from_millis(50),
        ready_max_wait: Duration::from_secs(5),
        max_open_docs: 8,
        shutdown_grace: Duration::from_secs(1),
        diagnostics_wait: Duration::from_secs(1),
    }
}

/// A worktree with `src/lib.rs` and `src/other.rs`.
fn worktree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), LIB).unwrap();
    std::fs::write(dir.path().join("src/other.rs"), OTHER).unwrap();
    dir
}

/// 1-based char column of `word` in line `line` (1-based) of `text`.
fn col(text: &str, line: u32, word: &str) -> u32 {
    let l = text.lines().nth(line as usize - 1).unwrap();
    let byte = l.find(word).unwrap();
    l[..byte].chars().count() as u32 + 1
}

fn loc(path: &str, line: u32, start: u32, end: u32) -> Location {
    Location {
        path: path.into(),
        range: range(line, start, end),
    }
}

fn range(line: u32, start: u32, end: u32) -> Range {
    Range {
        start: Position { line, col: start },
        end: Position { line, col: end },
    }
}

async fn start(root: &Path, env: &[(&str, &str)]) -> Arc<LspSession> {
    LspSession::start(root, config(env), options()).unwrap()
}

/// What the fake server knows, from its hover answer.
async fn report(s: &LspSession, root: &Path) -> Value {
    let text = s
        .hover(root, "src/lib.rs", Position { line: 1, col: 1 })
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn handshake_warms_up_until_indexing_ends() {
    let wt = worktree();
    let t0 = Instant::now();
    let s = start(wt.path(), &[("FAKE_LSP_PROGRESS_MS", "300")]).await;
    assert_eq!(s.state(), ServerState::Warming);
    assert_eq!(
        s.wait_ready(Duration::from_secs(5)).await,
        ServerState::Ready
    );
    assert!(
        t0.elapsed() >= Duration::from_millis(300),
        "{:?}",
        t0.elapsed()
    );
    assert_eq!(s.encoding(), Some(PositionEncoding::Utf16));
    s.shutdown().await;
    assert_eq!(s.state(), ServerState::NotRunning);
    assert!(s.has_exited());
}

#[tokio::test]
async fn ready_after_max_wait_when_indexing_never_ends() {
    let wt = worktree();
    let opts = SessionOptions {
        ready_max_wait: Duration::from_millis(400),
        ..options()
    };
    let t0 = Instant::now();
    let s = LspSession::start(
        wt.path(),
        config(&[("FAKE_LSP_PROGRESS_MS", "never")]),
        opts,
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(s.state(), ServerState::Warming);
    assert_eq!(
        s.wait_ready(Duration::from_secs(5)).await,
        ServerState::Ready
    );
    assert!(t0.elapsed() >= Duration::from_millis(400));
}

#[tokio::test]
async fn positions_convert_through_utf16_and_utf8() {
    for (enc, expect) in [
        ("utf-16", PositionEncoding::Utf16),
        ("utf-8", PositionEncoding::Utf8),
    ] {
        let wt = worktree();
        let root = wt.path();
        let s = start(root, &[("FAKE_LSP_ENCODING", enc)]).await;
        let call = col(LIB, 5, "target");
        assert_eq!(call, 28, "fixture: 27 chars before the call");

        // The server sees the cursor inside `target` only if the column
        // was converted right; it answers with the word's first occurrence.
        let defs = s
            .definition(
                root,
                "src/lib.rs",
                Position {
                    line: 5,
                    col: call + 2,
                },
            )
            .await
            .unwrap();
        assert_eq!(s.encoding(), Some(expect));
        // The server's second location is outside the worktree: dropped.
        assert_eq!(defs, vec![loc("src/lib.rs", 4, 4, 10)], "{enc}");

        // Open the other file so references see it too.
        s.document_symbols(root, "src/other.rs").await.unwrap();
        let refs = s
            .references(root, "src/lib.rs", Position { line: 5, col: call }, true)
            .await
            .unwrap();
        let other = col(OTHER, 2, "target");
        assert_eq!(
            refs,
            vec![
                loc("src/lib.rs", 4, 4, 10),
                loc("src/lib.rs", 5, call, call + 6),
                loc("src/other.rs", 2, other, other + 6),
            ],
            "{enc}"
        );
        let without_decl = s
            .references(root, "src/lib.rs", Position { line: 5, col: call }, false)
            .await
            .unwrap();
        assert_eq!(without_decl.len(), 2);
        s.shutdown().await;
    }
}

#[tokio::test]
async fn configuration_is_answered_from_init_options() {
    let wt = worktree();
    let s = start(wt.path(), &[]).await;
    s.wait_ready(Duration::from_secs(5)).await;
    // The answer may land just after the first hover; ask again until it has.
    let mut seen = Value::Null;
    for _ in 0..100 {
        seen = report(&s, wt.path()).await;
        if !seen["config"].is_null() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Sections: `fake`, `fake.nested.key`, `<server>.top`, unknown.
    assert_eq!(
        seen["config"],
        json!([{"a": 1, "nested": {"key": "v"}}, "v", true, null])
    );
    assert_eq!(seen["init_options"]["top"], json!(true));
}

#[tokio::test]
async fn document_symbols_nested_and_flat() {
    for flat in [false, true] {
        let wt = worktree();
        let root = wt.path();
        let env: &[(&str, &str)] = if flat {
            &[("FAKE_LSP_FLAT_SYMBOLS", "1")]
        } else {
            &[]
        };
        let s = start(root, env).await;
        let syms = s.document_symbols(root, "src/lib.rs").await.unwrap();
        let summary: Vec<(String, CodeSymbolKind, Option<String>, Range)> = syms
            .iter()
            .map(|s| (s.name.clone(), s.kind, s.container.clone(), s.name_range))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("Шар".into(), CodeSymbolKind::Struct, None, range(1, 8, 11)),
                (
                    "катится".into(),
                    CodeSymbolKind::Method,
                    Some("Шар".into()),
                    range(2, 8, 15)
                ),
                (
                    "target".into(),
                    CodeSymbolKind::Function,
                    None,
                    range(4, 4, 10)
                ),
                (
                    "main".into(),
                    CodeSymbolKind::Function,
                    None,
                    range(5, 4, 8)
                ),
            ],
            "flat: {flat}"
        );
        if !flat {
            // The whole struct, `{` to `}` line… as far as the fake knows.
            assert_eq!(syms[0].range.start, Position { line: 1, col: 1 });
            assert_eq!(syms[0].range.end.line, 2);
        }
        let found = s.workspace_symbols(root, "target").await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "src/lib.rs");
        assert_eq!(found[0].name_range, range(4, 4, 10));
        s.shutdown().await;
    }
}

#[tokio::test]
async fn diagnostics_cache_follows_file_changes() {
    let wt = worktree();
    let root = wt.path();
    let s = start(root, &[]).await;
    let bad = col(LIB, 5, "bad");
    let diags = s.diagnostics(root, Some("src/lib.rs")).await.unwrap();
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].location, loc("src/lib.rs", 5, bad, bad + 3));
    assert_eq!(diags[0].severity, DiagnosticSeverity::Error);
    assert_eq!(diags[0].code.as_deref(), Some("E1"));
    assert_eq!(diags[0].server, "fake-lsp");
    assert_eq!(s.diagnostics(root, None).await.unwrap(), diags);

    std::fs::write(root.join("src/lib.rs"), LIB.replace("bad()", "ok()")).unwrap();
    s.file_changed(root, "src/lib.rs").await.unwrap();
    eventually("diagnostics to clear", || async {
        s.diagnostics(root, None).await.unwrap().is_empty()
    })
    .await;
    // The fake registered for watched files, so it heard about the change.
    assert_eq!(report(&s, root).await["watched"], json!(1));
}

#[tokio::test]
async fn pull_diagnostics_when_the_server_offers_them() {
    let wt = worktree();
    let root = wt.path();
    let s = start(root, &[("FAKE_LSP_PULL", "1")]).await;
    let diags = s.diagnostics(root, Some("src/lib.rs")).await.unwrap();
    let bad = col(LIB, 5, "bad");
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].location, loc("src/lib.rs", 5, bad, bad + 3));
}

#[tokio::test]
async fn rename_handles_document_changes_and_changes() {
    for changes in [false, true] {
        let wt = worktree();
        let root = wt.path();
        let env: &[(&str, &str)] = if changes {
            &[("FAKE_LSP_RENAME_CHANGES", "1")]
        } else {
            &[]
        };
        let s = start(root, env).await;
        s.document_symbols(root, "src/other.rs").await.unwrap();
        let call = col(LIB, 5, "target");
        let edits = s
            .rename(root, "src/lib.rs", Position { line: 5, col: call }, "aim")
            .await
            .unwrap();
        let other = col(OTHER, 2, "target");
        let edit = |path: &str, line, start| TextEdit {
            path: path.into(),
            range: range(line, start, start + 6),
            new_text: "aim".into(),
        };
        assert_eq!(
            edits,
            vec![
                // Last-first within a file, so they apply in order.
                edit("src/lib.rs", 5, call),
                edit("src/lib.rs", 4, 4),
                edit("src/other.rs", 2, other),
            ],
            "changes: {changes}"
        );
        s.shutdown().await;
    }
}

#[tokio::test]
async fn a_hung_request_times_out_and_the_session_survives() {
    let wt = worktree();
    let root = wt.path();
    let opts = SessionOptions {
        request_timeout: Duration::from_millis(300),
        ..options()
    };
    let s = LspSession::start(
        root,
        config(&[("FAKE_LSP_HANG_ON", "textDocument/hover")]),
        opts,
    )
    .unwrap();
    let err = s
        .hover(root, "src/lib.rs", Position { line: 1, col: 1 })
        .await
        .unwrap_err();
    match err {
        CodeIntelError::Server { server, message } => {
            assert_eq!(server, "fake-lsp");
            assert!(message.contains("timed out"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        !s.definition(root, "src/lib.rs", Position { line: 4, col: 5 })
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn paths_outside_the_root_are_refused() {
    let wt = worktree();
    let root = wt.path();
    let s = start(root, &[]).await;
    let at = Position { line: 1, col: 1 };
    assert!(matches!(
        s.definition(root, "../etc/passwd", at).await,
        Err(CodeIntelError::OutsideWorkspace(_))
    ));
    assert!(matches!(
        s.definition(Path::new("/nowhere"), "src/lib.rs", at).await,
        Err(CodeIntelError::NotOpen(_))
    ));
    assert!(matches!(
        s.definition(root, "src/missing.rs", at).await,
        Err(CodeIntelError::FileNotFound(_))
    ));
    assert!(matches!(
        s.definition(root, "README.md", at).await,
        Err(CodeIntelError::Unsupported(_))
    ));
}

#[tokio::test]
async fn least_recently_used_documents_are_closed() {
    let wt = worktree();
    let root = wt.path();
    let opts = SessionOptions {
        max_open_docs: 1,
        ..options()
    };
    let s = LspSession::start(root, config(&[]), opts).unwrap();
    s.document_symbols(root, "src/other.rs").await.unwrap();
    // The hover opens lib.rs, which closes other.rs.
    let open = report(&s, root).await["open"].clone();
    let open: Vec<String> = serde_json::from_value(open).unwrap();
    assert_eq!(open.len(), 1);
    assert!(open[0].ends_with("/src/lib.rs"), "{open:?}");
}

#[tokio::test]
async fn shutdown_kills_a_server_that_ignores_exit() {
    let wt = worktree();
    let opts = SessionOptions {
        shutdown_grace: Duration::from_millis(300),
        ..options()
    };
    let s = LspSession::start(wt.path(), config(&[("FAKE_LSP_IGNORE_EXIT", "1")]), opts).unwrap();
    s.wait_ready(Duration::from_secs(5)).await;
    let t0 = Instant::now();
    s.shutdown().await;
    assert!(s.has_exited());
    assert_eq!(s.state(), ServerState::NotRunning);
    assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
}

#[tokio::test]
async fn a_crash_fails_pending_requests_and_restarts_once() {
    let wt = worktree();
    let root = wt.path();
    let pool = LspPool::with_options(4, Duration::from_secs(60), options());
    let cfg = config(&[("FAKE_LSP_CRASH_ON", "textDocument/definition")]);
    let at = Position { line: 4, col: 5 };

    let first = pool.get_or_spawn("repo", root, &cfg).await.unwrap();
    let err = first.definition(root, "src/lib.rs", at).await.unwrap_err();
    assert!(matches!(err, CodeIntelError::Server { .. }), "{err:?}");
    let mut rx = first.subscribe();
    tokio::time::timeout(
        Duration::from_secs(5),
        rx.wait_for(|s| *s == ServerState::Failed),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(pool.get("repo", root, "fake-lsp").await.is_none());
    // Restartable: not a failure yet.
    assert_eq!(
        pool.status(root).await,
        vec![("fake-lsp".to_string(), ServerState::NotRunning)]
    );

    let second = pool.get_or_spawn("repo", root, &cfg).await.unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    assert!(second.definition(root, "src/lib.rs", at).await.is_err());
    let mut rx = second.subscribe();
    tokio::time::timeout(
        Duration::from_secs(5),
        rx.wait_for(|s| *s == ServerState::Failed),
    )
    .await
    .unwrap()
    .unwrap();

    let third = pool.get_or_spawn("repo", root, &cfg).await;
    assert!(
        matches!(&third, Err(CodeIntelError::Server { message, .. }) if message.contains("crashed")),
        "{:?}",
        third.err()
    );
    assert_eq!(
        pool.status(root).await,
        vec![("fake-lsp".to_string(), ServerState::Failed)]
    );
    // Closing the worktree forgets it.
    pool.release_root(root).await;
    assert!(pool.status(root).await.is_empty());
}

#[tokio::test]
async fn pool_evicts_the_least_recently_used_over_max_servers() {
    let (a, b, c) = (worktree(), worktree(), worktree());
    let pool = LspPool::with_options(2, Duration::from_secs(60), options());
    let cfg = config(&[]);
    let sa = pool.get_or_spawn("repo", a.path(), &cfg).await.unwrap();
    let sb = pool.get_or_spawn("repo", b.path(), &cfg).await.unwrap();
    assert!(!Arc::ptr_eq(&sa, &sb), "no workspace folders: one per root");
    assert!(Arc::ptr_eq(
        &sa,
        &pool.get_or_spawn("repo", a.path(), &cfg).await.unwrap()
    ));
    // `a` is used after `b`, so `b` goes first.
    report(&sb, b.path()).await;
    report(&sa, a.path()).await;
    let sc = pool.get_or_spawn("repo", c.path(), &cfg).await.unwrap();
    assert_eq!(pool.len().await, 2);
    assert!(pool.get("repo", b.path(), "fake-lsp").await.is_none());
    assert!(pool.get("repo", a.path(), "fake-lsp").await.is_some());
    let mut rx = sb.subscribe();
    tokio::time::timeout(
        Duration::from_secs(5),
        rx.wait_for(|s| *s == ServerState::NotRunning),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        sc.wait_ready(Duration::from_secs(5)).await,
        ServerState::Ready
    );
    pool.shutdown_all().await;
    assert!(pool.is_empty().await);
    assert_eq!(sa.state(), ServerState::NotRunning);
}

#[tokio::test]
async fn idle_sessions_are_reaped() {
    let wt = worktree();
    let pool = LspPool::with_options(4, Duration::from_millis(300), options());
    let s = pool
        .get_or_spawn("repo", wt.path(), &config(&[]))
        .await
        .unwrap();
    let mut rx = s.subscribe();
    tokio::time::timeout(
        Duration::from_secs(5),
        rx.wait_for(|s| *s == ServerState::NotRunning),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(pool.is_empty().await);
    assert!(pool.status(wt.path()).await.is_empty());
}

#[tokio::test]
async fn worktrees_join_one_server_as_workspace_folders() {
    let (a, b) = (worktree(), worktree());
    let pool = LspPool::with_options(4, Duration::from_secs(60), options());
    let cfg = LaunchConfig {
        workspace_folders: true,
        ..config(&[("FAKE_LSP_FOLDERS", "1")])
    };
    let sa = pool.get_or_spawn("repo", a.path(), &cfg).await.unwrap();
    let sb = pool.get_or_spawn("repo", b.path(), &cfg).await.unwrap();
    assert!(Arc::ptr_eq(&sa, &sb));
    let other_repo = pool.get_or_spawn("other", b.path(), &cfg).await.unwrap();
    assert!(!Arc::ptr_eq(&sa, &other_repo));

    let uri = |p: &Path| -> String {
        let canon = std::fs::canonicalize(p).unwrap();
        url::Url::from_file_path(canon).unwrap().to_string()
    };
    let folders = report(&sa, b.path()).await["folders"].clone();
    assert_eq!(folders, json!([uri(a.path()), uri(b.path())]));
    // Answers are relative to the root asked about.
    let defs = sb
        .definition(b.path(), "src/lib.rs", Position { line: 4, col: 5 })
        .await
        .unwrap();
    assert_eq!(defs, vec![loc("src/lib.rs", 4, 4, 10)]);

    pool.release_root(b.path()).await;
    assert!(matches!(
        sa.state(),
        ServerState::Warming | ServerState::Ready
    ));
    let folders = report(&sa, a.path()).await["folders"].clone();
    assert_eq!(folders, json!([uri(a.path())]));
    assert!(matches!(
        sa.hover(b.path(), "src/lib.rs", Position { line: 1, col: 1 })
            .await,
        Err(CodeIntelError::NotOpen(_))
    ));
    pool.release_root(a.path()).await;
    assert_eq!(sa.state(), ServerState::NotRunning);
}

#[tokio::test]
async fn single_root_servers_get_one_session_per_worktree() {
    let (a, b) = (worktree(), worktree());
    let pool = LspPool::with_options(4, Duration::from_secs(60), options());
    // The config says folders, the server's capabilities don't.
    let cfg = LaunchConfig {
        workspace_folders: true,
        ..config(&[])
    };
    let sa = pool.get_or_spawn("repo", a.path(), &cfg).await.unwrap();
    let sb = pool.get_or_spawn("repo", b.path(), &cfg).await.unwrap();
    assert!(!Arc::ptr_eq(&sa, &sb));
    assert_eq!(sa.roots(), vec![canonical(a.path())]);
    assert_eq!(sb.roots(), vec![canonical(b.path())]);
    assert_eq!(pool.len().await, 2);
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap()
}
