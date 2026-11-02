//! Routing to a language server (`fake-lsp` from code-intel-lsp) and the
//! `code_intel.lsp` policy: one switch for review runs, one for spaces.

mod common;

use std::path::Path;

use common::{Env, eventually, indexed, repo, server_state, workspace};
use pretty_assertions::assert_eq;
use theodore_code_intel::CodeIntel;
use theodore_ports::code_intel::{
    CodeIntelError, CodeIntelPort, DiagnosticSeverity, LspPolicy, Position, ServerState, Source,
    TrustMode,
};

const LIB: &str = "struct Store {}\nfn helper() {}\nfn main() { helper(); bad(); }\n";

fn at(line: u32, col: u32) -> Position {
    Position { line, col }
}

async fn ready(ci: &CodeIntel, root: &Path) {
    eventually("server ready", || async {
        server_state(ci, root, "rust").await == Some(ServerState::Ready)
    })
    .await;
}

/// The server answers for files of its language, every query kind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trusted_space_routes_to_the_ready_server() {
    let env = Env::new();
    let wt = repo(&[("src/lib.rs", LIB)]);
    let ci = env.fake_lsp_facade(LspPolicy::default());
    ci.open(workspace(wt.path(), TrustMode::Trusted))
        .await
        .unwrap();
    ready(&ci, wt.path()).await;
    let root = wt.path().canonicalize().unwrap();
    assert_eq!(env.launches(), vec![root.clone()]);

    let syms = ci.file_symbols(wt.path(), "src/lib.rs").await.unwrap();
    assert_eq!(syms.source, Source::Lsp);
    let names: Vec<&str> = syms.items.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["Store", "helper", "main"]);

    // The file is open on the server now, so workspace/symbol sees it.
    let found = ci.find_symbol(wt.path(), "helper", None).await.unwrap();
    assert_eq!(found.source, Source::Lsp);
    assert_eq!(found.items.len(), 1);
    assert_eq!(found.items[0].range.start.line, 2);

    // `helper();` on line 3: the fake answers its first occurrence, plus a
    // location outside the worktree, which is dropped.
    let def = ci
        .definition(wt.path(), "src/lib.rs", at(3, 14))
        .await
        .unwrap();
    assert_eq!(def.source, Source::Lsp);
    assert_eq!(def.items.len(), 1);
    assert_eq!(def.items[0].location.path, "src/lib.rs");
    assert_eq!(def.items[0].location.range.start.line, 2);
    assert_eq!(def.items[0].preview.lines().nth(1), Some("fn helper() {}"));

    let refs = ci
        .references(wt.path(), "src/lib.rs", at(2, 5))
        .await
        .unwrap();
    assert_eq!(refs.source, Source::Lsp);
    assert_eq!(refs.items.len(), 2);

    let diags = ci.diagnostics(wt.path(), Some("src/lib.rs")).await.unwrap();
    assert_eq!(diags.source, Source::Lsp);
    assert_eq!(diags.items.len(), 1);
    assert_eq!(diags.items[0].severity, DiagnosticSeverity::Error);
    assert_eq!(diags.items[0].server, "fake-lsp");

    let edits = ci
        .rename(wt.path(), "src/lib.rs", at(2, 5), "aid")
        .await
        .unwrap();
    assert_eq!(edits.len(), 2);
    assert!(edits.iter().all(|e| e.new_text == "aid"));

    // Through the server's symbols when the syntax layer has no such name
    // in the file; here syntax has it, so it answers.
    let (sym, body) = ci
        .symbol_body(wt.path(), "src/lib.rs", "helper")
        .await
        .unwrap();
    assert_eq!(
        (sym.source, body.as_str()),
        (Source::Syntax, "fn helper() {}")
    );

    // Spaces get the trusted options.
    let hover = ci.hover(wt.path(), "src/lib.rs", at(1, 1)).await.unwrap();
    let report: serde_json::Value = serde_json::from_str(&hover.unwrap()).unwrap();
    assert_eq!(report["init_options"]["mode"], "trusted");

    ci.close(wt.path()).await;
    assert!(matches!(
        ci.file_symbols(wt.path(), "src/lib.rs").await,
        Err(CodeIntelError::NotOpen(_))
    ));
}

/// Review runs: no server unless `lsp.review`; then with review options.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_runs_start_no_server_by_default() {
    let env = Env::new();
    let wt = repo(&[("src/lib.rs", LIB)]);
    let ci = env.fake_lsp_facade(LspPolicy::default());
    ci.open(workspace(wt.path(), TrustMode::Review))
        .await
        .unwrap();
    indexed(&ci, wt.path()).await;

    let syms = ci.file_symbols(wt.path(), "src/lib.rs").await.unwrap();
    assert_eq!(syms.source, Source::Syntax);
    assert_eq!(
        syms.note.as_deref(),
        Some("language servers are off for reviews (code_intel.lsp.review)")
    );
    let def = ci
        .definition(wt.path(), "src/lib.rs", at(3, 14))
        .await
        .unwrap();
    assert_eq!(def.source, Source::Syntax);
    assert!(def.note.unwrap().contains("off for reviews"));
    let diags = ci.diagnostics(wt.path(), None).await.unwrap();
    assert!(diags.items.is_empty());
    assert!(matches!(
        ci.rename(wt.path(), "src/lib.rs", at(2, 5), "x").await,
        Err(CodeIntelError::Unsupported(m)) if m.contains("off for reviews")
    ));
    ci.file_changed(wt.path(), "src/lib.rs").await;

    let st = common::status_of(&ci, wt.path(), "rust").await;
    let server = st.server.unwrap();
    assert_eq!(server.name, "fake-lsp");
    // Found on the machine, still never started.
    assert!(matches!(
        server.install,
        theodore_ports::code_intel::InstallState::System { .. }
    ));
    assert_eq!(server.state, ServerState::NotRunning);
    assert!(env.launches().is_empty(), "{:?}", env.launches());
}

/// Every combination of the two switches, on a review run and a space
/// side by side, then switched live.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_switch_gates_its_own_worktrees_live() {
    let env = Env::new();
    let review = repo(&[("src/lib.rs", LIB)]);
    let space = repo(&[("src/lib.rs", LIB)]);
    let (r, s) = (
        review.path().canonicalize().unwrap(),
        space.path().canonicalize().unwrap(),
    );
    let ci = env.fake_lsp_facade(LspPolicy {
        review: false,
        spaces: false,
    });
    ci.open(workspace(&r, TrustMode::Review)).await.unwrap();
    ci.open(workspace(&s, TrustMode::Trusted)).await.unwrap();
    indexed(&ci, &r).await;
    indexed(&ci, &s).await;

    // (review, spaces)
    for (rv, sp) in [
        (false, false),
        (true, false),
        (true, true),
        (false, true),
        (false, false),
    ] {
        let mut o = ci.options();
        o.lsp = LspPolicy {
            review: rv,
            spaces: sp,
        };
        ci.set_options(o).await;
        for (root, on, note) in [
            (
                &r,
                rv,
                "language servers are off for reviews (code_intel.lsp.review)",
            ),
            (
                &s,
                sp,
                "language servers are off for spaces (code_intel.lsp.spaces)",
            ),
        ] {
            let what = format!("review={rv} spaces={sp} {}", root.display());
            if on {
                ready(&ci, root).await;
                let a = ci.file_symbols(root, "src/lib.rs").await.unwrap();
                assert_eq!(a.source, Source::Lsp, "{what}");
            } else {
                eventually(&what, || async {
                    server_state(&ci, root, "rust").await == Some(ServerState::NotRunning)
                })
                .await;
                let a = ci.file_symbols(root, "src/lib.rs").await.unwrap();
                assert_eq!(a.source, Source::Syntax, "{what}");
                assert_eq!(a.note.as_deref(), Some(note), "{what}");
            }
        }
    }
    // Each worktree's server started only while its switch was on: review
    // once (true,false → true,true), space once (true,true → false,true).
    let launches = env.launches();
    assert_eq!(
        launches.iter().filter(|p| **p == r).count(),
        1,
        "{launches:?}"
    );
    assert_eq!(
        launches.iter().filter(|p| **p == s).count(),
        1,
        "{launches:?}"
    );
}

/// A review run with the switch on gets the server's review options.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_sessions_get_review_options_and_their_own_process() {
    let env = Env::new();
    let wt = repo(&[("src/lib.rs", LIB)]);
    let ci = env.fake_lsp_facade(LspPolicy {
        review: true,
        spaces: true,
    });
    // Same repo key for both: they still do not share a process.
    let mut rv = workspace(wt.path(), TrustMode::Review);
    let sp_dir = repo(&[("src/lib.rs", LIB)]);
    let mut sp = workspace(sp_dir.path(), TrustMode::Trusted);
    rv.repo_key = "github:o/r".into();
    sp.repo_key = "github:o/r".into();
    ci.open(rv).await.unwrap();
    ci.open(sp).await.unwrap();
    ready(&ci, wt.path()).await;
    ready(&ci, sp_dir.path()).await;
    assert_eq!(env.launches().len(), 2);

    // The fake's hover reports the initializationOptions it got.
    let mode = |root: &Path| {
        let (ci, root) = (ci.clone(), root.to_path_buf());
        async move {
            let hover = ci.hover(&root, "src/lib.rs", at(1, 1)).await.unwrap();
            let report: serde_json::Value = serde_json::from_str(&hover.unwrap()).unwrap();
            report["init_options"]["mode"].as_str().unwrap().to_string()
        }
    };
    assert_eq!(mode(wt.path()).await, "review");
    assert_eq!(mode(sp_dir.path()).await, "trusted");
    let a = ci.file_symbols(wt.path(), "src/lib.rs").await.unwrap();
    assert_eq!(a.source, Source::Lsp);
}

/// A workspace's own policy (`Workspace::lsp`, the project's config) wins
/// over the facade's default, both ways.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_workspace_policy_overrides_the_default() {
    let env = Env::new();
    let review = repo(&[("src/lib.rs", LIB)]);
    let space = repo(&[("src/lib.rs", LIB)]);
    let ci = env.fake_lsp_facade(LspPolicy::default());
    let on = LspPolicy {
        review: true,
        spaces: true,
    };
    let off = LspPolicy {
        review: false,
        spaces: false,
    };
    let mut ws = workspace(review.path(), TrustMode::Review);
    ws.lsp = Some(on);
    ci.open(ws).await.unwrap();
    let mut ws = workspace(space.path(), TrustMode::Trusted);
    ws.lsp = Some(off);
    ci.open(ws).await.unwrap();

    ready(&ci, review.path()).await;
    indexed(&ci, space.path()).await;
    let syms = ci.file_symbols(space.path(), "src/lib.rs").await.unwrap();
    assert_eq!(syms.source, Source::Syntax);
    assert_eq!(
        server_state(&ci, space.path(), "rust").await,
        Some(ServerState::NotRunning)
    );
    assert_eq!(env.launches().len(), 1, "{:?}", env.launches());
}
