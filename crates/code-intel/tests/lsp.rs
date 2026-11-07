//! Routing to a language server (`fake-lsp` from code-intel-lsp) and the
//! `code_intel.lsp` policy: one switch for review runs, one for spaces.

mod common;

use std::path::Path;

use common::{Env, eventually, indexed, repo, server_state, workspace};
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use theodore_code_intel::CodeIntel;
use theodore_ports::code_intel::{
    CodeIntelError, CodeIntelPort, DiagnosticSeverity, InstallState, LanguageId, LspPolicy,
    Position, ServerState, Source, TrustMode,
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
    // location outside the worktree, which stays out of the items (their
    // paths are repo-relative) and is named in the note.
    let def = ci
        .definition(wt.path(), "src/lib.rs", at(3, 14))
        .await
        .unwrap();
    assert_eq!(def.source, Source::Lsp);
    assert_eq!(def.items.len(), 1);
    assert_eq!(
        def.note.as_deref(),
        Some("1 result outside the worktree (std / dependencies): /outside/lib.rs")
    );
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
    let hover = ci
        .hover(wt.path(), "src/lib.rs", at(1, 1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hover.source, Source::Lsp);
    let report: serde_json::Value = serde_json::from_str(&hover.text).unwrap();
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
    assert_eq!(server.state, ServerState::Off);
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
                    server_state(&ci, root, "rust").await == Some(ServerState::Off)
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
            let report: serde_json::Value = serde_json::from_str(&hover.unwrap().text).unwrap();
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
        Some(ServerState::Off)
    );
    assert_eq!(env.launches().len(), 1, "{:?}", env.launches());
}

/// A server stopped for being idle stays stopped until a question, so the
/// settings row read "Not running" for good. `warm` brings it back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warm_brings_back_a_server_stopped_for_being_idle() {
    let env = Env::new();
    let wt = repo(&[("src/lib.rs", LIB)]);
    let root = wt.path().canonicalize().unwrap();
    let ci = env.fake_lsp_facade(LspPolicy::default());
    ci.open(workspace(&root, TrustMode::Trusted)).await.unwrap();
    ready(&ci, &root).await;
    assert_eq!(env.launches().len(), 1);

    let mut o = ci.options();
    o.idle = std::time::Duration::from_millis(20);
    ci.set_options(o).await;
    eventually("idle stop", || async {
        server_state(&ci, &root, "rust").await == Some(ServerState::NotRunning)
    })
    .await;

    let mut o = ci.options();
    o.idle = std::time::Duration::from_secs(600);
    ci.set_options(o).await;
    ci.warm(&root).await;
    ready(&ci, &root).await;

    // Warming a live server starts nothing.
    let launches = env.launches().len();
    ci.warm(&root).await;
    assert_eq!(env.launches().len(), launches);
}

/// Warming never pushes out a running server to make room.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warm_stays_under_the_cap() {
    let env = Env::new();
    let (a, b) = (repo(&[("src/lib.rs", LIB)]), repo(&[("src/lib.rs", LIB)]));
    let (a, b, _keep) = (
        a.path().canonicalize().unwrap(),
        b.path().canonicalize().unwrap(),
        (a, b),
    );
    let ci = env.fake_lsp_facade(LspPolicy::default());
    let mut o = ci.options();
    o.max_servers = 1;
    ci.set_options(o).await;
    ci.open(workspace(&a, TrustMode::Trusted)).await.unwrap();
    ready(&ci, &a).await;

    let off = LspPolicy {
        review: false,
        spaces: false,
    };
    let mut ws = workspace(&b, TrustMode::Trusted);
    ws.repo_key = "other".into();
    ws.lsp = Some(off);
    ci.open(ws).await.unwrap();
    ci.set_lsp(&b, true).await;
    // Turning the switch on is an explicit start: it takes the only slot.
    // Warming the one that lost it must not take it back.
    ready(&ci, &b).await;
    assert_eq!(
        server_state(&ci, &a, "rust").await,
        Some(ServerState::NotRunning)
    );
    let launches = env.launches().len();
    assert_eq!(launches, 2, "{:?}", env.launches());
    ci.warm(&a).await;
    assert_eq!(env.launches().len(), launches, "{:?}", env.launches());
    assert_eq!(
        server_state(&ci, &b, "rust").await,
        Some(ServerState::Ready)
    );
}

/// The project's switch reaches a worktree already open, both ways.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_workspace_switch_applies_live() {
    let env = Env::new();
    let wt = repo(&[("src/lib.rs", LIB)]);
    let root = wt.path().canonicalize().unwrap();
    let ci = env.fake_lsp_facade(LspPolicy::default());
    ci.open(workspace(&root, TrustMode::Review)).await.unwrap();
    indexed(&ci, &root).await;
    assert_eq!(
        server_state(&ci, &root, "rust").await,
        Some(ServerState::Off)
    );
    assert!(env.launches().is_empty());

    ci.set_lsp(&root, true).await;
    ready(&ci, &root).await;
    assert_eq!(env.launches().len(), 1);

    ci.set_lsp(&root, false).await;
    eventually("off", || async {
        server_state(&ci, &root, "rust").await == Some(ServerState::Off)
    })
    .await;
}

const BOTH_ON: LspPolicy = LspPolicy {
    review: true,
    spaces: true,
};

/// `code_intel.servers.<language>`: the project's preference goes first, and
/// a change reopens the worktree on the other server.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_workspace_can_prefer_a_server() {
    let yaml = r#"
servers:
  - name: first
    detect: [["first-ls"]]
    launch: ["{bin}"]
  - name: second
    detect: [["second-ls"]]
    launch: ["{bin}"]
languages:
  - id: rust
    globs: ["*.rs"]
    servers: [first, second]
"#;
    let env = Env::new();
    env.fake_server("first-ls");
    env.fake_server("second-ls");
    let wt = repo(&[("src/lib.rs", LIB)]);
    let ci = env.facade_with(yaml, LspPolicy::default());
    let server_of = |status: Vec<theodore_ports::code_intel::LanguageStatus>| {
        status[0].server.as_ref().unwrap().name.clone()
    };

    ci.open(workspace(wt.path(), TrustMode::Trusted))
        .await
        .unwrap();
    assert_eq!(server_of(ci.status(wt.path()).await.unwrap()), "first");

    let mut ws = workspace(wt.path(), TrustMode::Trusted);
    ws.servers = BTreeMap::from([("rust".to_string(), "second".to_string())]);
    ci.open(ws).await.unwrap();
    assert_eq!(server_of(ci.status(wt.path()).await.unwrap()), "second");
}

/// A runtime missing or too old: not started, status says so, the answer
/// says why, and Install downloads nothing. Installed later, the next open
/// picks it up, and the server gets the runtime that passed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_without_its_runtime_is_not_started_or_downloaded() {
    let yaml = r#"
servers:
  - name: needs-java
    version: "1.0"
    detect: [["java-ls"]]
    download:
      darwin-arm64: { url: "https://example.invalid/j.gz", sha256: "8fb90cb58f5910d8d8b24ac1a93cca49f053365cbf0eb3c682144ad0a0027dcf", format: gz, bin: j }
      linux-x64: { url: "https://example.invalid/j.gz", sha256: "8fb90cb58f5910d8d8b24ac1a93cca49f053365cbf0eb3c682144ad0a0027dcf", format: gz, bin: j }
    launch: ["{bin}"]
    requires: { runtime: java, min: 21 }
languages:
  - id: rust
    globs: ["*.rs"]
    servers: [needs-java]
"#;
    let env = Env::new();
    env.fake_server("java-ls");
    let wt = repo(&[("src/lib.rs", LIB)]);
    let ci = env.facade_with(yaml, LspPolicy::default());
    ci.open(workspace(wt.path(), TrustMode::Trusted))
        .await
        .unwrap();
    indexed(&ci, wt.path()).await;
    let status = ci.status(wt.path()).await.unwrap();
    let server = status[0].server.as_ref().unwrap();
    assert_eq!(
        server.install,
        InstallState::NeedsRuntime {
            runtime: "java".into(),
            min: "21".into(),
            found: None
        }
    );
    assert_eq!(env.launches(), Vec::<std::path::PathBuf>::new());

    let answer = ci.file_symbols(wt.path(), "src/lib.rs").await.unwrap();
    assert_eq!(answer.source, Source::Syntax);
    let note = answer.note.unwrap_or_default();
    assert!(note.contains("needs-java needs java 21"), "{note}");

    // The URL can't resolve: an attempt would fail the install.
    ci.install(&LanguageId::new("rust"), None).await.unwrap();

    // A JDK appears; the next open of the worktree finds it.
    let home = env.tmp.path().canonicalize().unwrap().join("jdk");
    std::fs::create_dir_all(home.join("bin")).unwrap();
    let java = home.join("bin/java");
    std::fs::write(
        &java,
        format!(
            "#!/bin/sh\necho '    java.home = {}' >&2\necho 'openjdk version \"21.0.5\"' >&2\n",
            home.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&java, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(&java, env.bin.join("java")).unwrap();

    ci.close(wt.path()).await;
    ci.open(workspace(wt.path(), TrustMode::Trusted))
        .await
        .unwrap();
    ready(&ci, wt.path()).await;
    assert_eq!(env.launches().len(), 1);
    assert_eq!(
        std::fs::read_to_string(env.java_home_log()).unwrap().trim(),
        home.display().to_string(),
        "the server gets the JDK that passed"
    );
}

/// No off switch for what it runs: a review never starts it, even with the
/// review switch on; a space does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_trusted_only_server_never_serves_a_review() {
    let yaml = r#"
servers:
  - name: builds
    detect: [["build-ls"]]
    launch: ["{bin}"]
    trusted_only: true
languages:
  - id: rust
    globs: ["*.rs"]
    servers: [builds]
"#;
    let env = Env::new();
    env.fake_server("build-ls");
    let wt = repo(&[("src/lib.rs", LIB)]);
    let ci = env.facade_with(yaml, BOTH_ON);

    ci.open(workspace(wt.path(), TrustMode::Review))
        .await
        .unwrap();
    indexed(&ci, wt.path()).await;
    let status = ci.status(wt.path()).await.unwrap();
    let server = status[0].server.as_ref().unwrap();
    assert_eq!(server.install, InstallState::TrustedOnly);
    assert_eq!(server.state, ServerState::NotRunning);
    assert_eq!(env.launches(), Vec::<std::path::PathBuf>::new());
    let answer = ci.file_symbols(wt.path(), "src/lib.rs").await.unwrap();
    assert_eq!(answer.source, Source::Syntax);
    let note = answer.note.unwrap_or_default();
    assert!(note.contains("serves spaces and not reviews"), "{note}");
    assert_eq!(env.launches(), Vec::<std::path::PathBuf>::new());

    ci.close(wt.path()).await;
    ci.open(workspace(wt.path(), TrustMode::Trusted))
        .await
        .unwrap();
    ready(&ci, wt.path()).await;
    assert_eq!(env.launches().len(), 1);
}

/// A hint reaches the status of a server nothing can fetch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hint_reaches_the_status() {
    let yaml = r#"
servers:
  - name: gem-ls
    detect: [["gem-ls"]]
    launch: ["{bin}"]
    hint: gem install gem-ls
languages:
  - id: rust
    globs: ["*.rs"]
    servers: [gem-ls]
"#;
    let env = Env::new();
    let wt = repo(&[("src/lib.rs", LIB)]);
    let ci = env.facade_with(yaml, LspPolicy::default());
    ci.open(workspace(wt.path(), TrustMode::Trusted))
        .await
        .unwrap();
    let status = ci.status(wt.path()).await.unwrap();
    let server = status[0].server.as_ref().unwrap();
    assert_eq!(server.install, InstallState::Unavailable);
    assert_eq!(server.hint.as_deref(), Some("gem install gem-ls"));
}

/// Headings and keys of prose and data files (Markdown, TOML, YAML) are
/// not code definitions: `find_symbol` leaves them out and says so, while
/// the file's own symbols still list them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prose_symbols_are_not_definitions_unless_asked_for_the_file() {
    let yaml = r#"
servers:
  - name: fake-lsp
    detect: [["fake-lsp"]]
    launch: ["{bin}"]
languages:
  - id: rust
    globs: ["*.rs"]
    servers: [fake-lsp]
  - id: markdown
    globs: ["*.md"]
    prose: true
    servers: [fake-lsp]
"#;
    let env = Env::new();
    env.fake_server("fake-lsp");
    let wt = repo(&[("README.md", "fn helper\n"), ("src/lib.rs", LIB)]);
    let ci = env.facade_with(yaml, LspPolicy::default());
    ci.open(workspace(wt.path(), TrustMode::Trusted))
        .await
        .unwrap();
    ready(&ci, wt.path()).await;
    let own = ci.file_symbols(wt.path(), "README.md").await.unwrap();
    assert_eq!(own.items.len(), 1, "asked for the file: listed");

    let found = ci.find_symbol(wt.path(), "helper", None).await.unwrap();
    let paths: Vec<&str> = found.items.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(paths, vec!["src/lib.rs"]);
    // Only prose has it: empty, with a note naming where.
    std::fs::write(wt.path().join("src/lib.rs"), "fn other() {}\n").unwrap();
    ci.file_changed(wt.path(), "src/lib.rs").await;
    let found = ci.find_symbol(wt.path(), "helper", None).await.unwrap();
    assert!(found.items.is_empty(), "{:?}", found.items);
    let note = found.note.unwrap_or_default();
    assert!(
        note.contains("in prose or data files, not code: README.md"),
        "{note}"
    );
}

/// A server without a method (taplo has no `workspace/symbol`) is not
/// asked for it, and its "method not found" is no note: the syntax layer
/// answers as if no server were there for that question.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_method_the_server_lacks_is_neither_asked_nor_noted() {
    let env = Env::new();
    let wt = repo(&[("src/lib.rs", LIB)]);
    let ci = env.fake_lsp_facade(LspPolicy::default());
    let real = env!("CARGO_BIN_EXE_code-intel-fake-lsp");
    env.script(
        "fake-lsp",
        &format!("FAKE_LSP_NO_SYMBOLS=1 exec '{real}' \"$@\""),
    );
    ci.open(workspace(wt.path(), TrustMode::Trusted))
        .await
        .unwrap();
    ready(&ci, wt.path()).await;
    indexed(&ci, wt.path()).await;
    // The file is open on the server: it would answer if it could.
    ci.file_symbols(wt.path(), "src/lib.rs").await.unwrap();

    let found = ci.find_symbol(wt.path(), "helper", None).await.unwrap();
    assert_eq!(found.source, Source::Syntax);
    assert_eq!(found.items.len(), 1);
    assert_eq!(found.note, None);
}
