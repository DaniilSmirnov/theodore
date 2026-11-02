//! The facade without language servers: syntax index, text fallback,
//! updates, status, path checks.

mod common;

use common::{Env, StaticGrammars, eventually, indexed, repo, status_of, workspace};
use pretty_assertions::assert_eq;
use theodore_ports::code_intel::{
    CodeIntelError, CodeIntelPort, CodeSymbolKind, InstallState, Position, ServerState, Source,
    TrustMode,
};

const LIB: &str = "pub struct Store {}\n\nimpl Store {\n    pub fn save(&self) {}\n}\n\npub fn helper() -> u32 {\n    1\n}\n";
const MAIN: &str = "mod lib;\n\nfn main() {\n    let n = helper();\n    lib::Store {}.save();\n}\n";

fn at(line: u32, col: u32) -> Position {
    Position { line, col }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn syntax_answers_symbols_definitions_references() {
    let env = Env::new();
    let wt = repo(&[("src/lib.rs", LIB), ("src/main.rs", MAIN)]);
    let ci = env.facade(StaticGrammars::all());
    let mut ws = workspace(wt.path(), TrustMode::Trusted);
    ws.focus_paths = vec!["src/main.rs".into()];
    ci.open(ws.clone()).await.unwrap();
    ci.open(ws).await.unwrap(); // idempotent
    indexed(&ci, wt.path()).await;

    let found = ci.find_symbol(wt.path(), "helper", None).await.unwrap();
    assert_eq!(found.source, Source::Syntax);
    assert_eq!(found.index_percent, 100);
    assert_eq!(found.items.len(), 1);
    assert_eq!(found.items[0].path, "src/lib.rs");
    assert_eq!(found.items[0].kind, CodeSymbolKind::Function);
    // rust-analyzer is in the manifest, not on this machine's (empty) PATH.
    let note = found.note.unwrap_or_default();
    assert!(note.contains("rust-analyzer not installed"), "{note}");

    let structs = ci
        .find_symbol(wt.path(), "Store", Some(CodeSymbolKind::Struct))
        .await
        .unwrap();
    assert_eq!(structs.items.len(), 1);
    assert!(
        ci.find_symbol(wt.path(), "Store", Some(CodeSymbolKind::Function))
            .await
            .unwrap()
            .items
            .is_empty()
    );

    // `helper()` in main.rs line 4.
    let def = ci
        .definition(wt.path(), "src/main.rs", at(4, 14))
        .await
        .unwrap();
    assert_eq!(def.source, Source::Syntax);
    assert_eq!(def.items[0].location.path, "src/lib.rs");
    assert_eq!(def.items[0].location.range.start.line, 7);
    assert!(def.items[0].is_definition);

    let refs = ci
        .references(wt.path(), "src/lib.rs", at(7, 9))
        .await
        .unwrap();
    assert_eq!(refs.source, Source::Syntax);
    let mut paths: Vec<(&str, u32)> = refs
        .items
        .iter()
        .map(|h| (h.location.path.as_str(), h.location.range.start.line))
        .collect();
    paths.sort();
    assert_eq!(paths, vec![("src/lib.rs", 7), ("src/main.rs", 4)]);

    let syms = ci.file_symbols(wt.path(), "src/lib.rs").await.unwrap();
    let names: Vec<&str> = syms.items.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"Store") && names.contains(&"save") && names.contains(&"helper"));

    let (sym, body) = ci
        .symbol_body(wt.path(), "./src/lib.rs", "helper")
        .await
        .unwrap();
    assert_eq!(sym.name, "helper");
    assert_eq!(body, "pub fn helper() -> u32 {\n    1\n}");
    assert!(matches!(
        ci.symbol_body(wt.path(), "src/lib.rs", "nope").await,
        Err(CodeIntelError::NoSymbol { .. })
    ));

    // Without a server: no diagnostics, no rename, and the answer says so.
    let diags = ci.diagnostics(wt.path(), None).await.unwrap();
    assert!(diags.items.is_empty());
    assert!(diags.note.unwrap().contains("language server"));
    assert!(matches!(
        ci.rename(wt.path(), "src/lib.rs", at(7, 9), "aid").await,
        Err(CodeIntelError::Unsupported(_))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_search_covers_a_language_without_grammar() {
    let env = Env::new();
    let wt = repo(&[
        (
            "hello.go",
            "package main\n\nfunc Hello() string {\n\treturn \"hi\"\n}\n",
        ),
        ("main.go", "package main\n\nfunc main() {\n\tHello()\n}\n"),
        ("README.md", "func Hello is documented here\n"),
        ("src/a.rs", "fn rusty() {}\n"),
    ]);
    // `go` is in the manifest; this source has no grammar for it.
    let ci = env.facade(StaticGrammars::without(&["go"]));
    ci.open(workspace(wt.path(), TrustMode::Trusted))
        .await
        .unwrap();
    indexed(&ci, wt.path()).await;

    let found = ci.find_symbol(wt.path(), "Hello", None).await.unwrap();
    assert_eq!(found.source, Source::Text);
    // Markdown is not code: only the Go definition.
    let hits: Vec<(&str, u32)> = found
        .items
        .iter()
        .map(|s| (s.path.as_str(), s.range.start.line))
        .collect();
    assert_eq!(hits, vec![("hello.go", 3)]);
    let note = found.note.unwrap();
    assert!(note.contains("go: no grammar yet"), "{note}");

    let refs = ci.references(wt.path(), "main.go", at(4, 3)).await.unwrap();
    assert_eq!(refs.source, Source::Text);
    let mut paths: Vec<&str> = refs
        .items
        .iter()
        .map(|h| h.location.path.as_str())
        .collect();
    paths.sort();
    assert_eq!(paths, vec!["hello.go", "main.go"]);

    let def = ci.definition(wt.path(), "main.go", at(4, 3)).await.unwrap();
    assert_eq!(def.source, Source::Text);
    assert_eq!(def.items.len(), 1);
    assert_eq!(def.items[0].location.path, "hello.go");

    // Rust still answers from its grammar.
    let rust = ci.find_symbol(wt.path(), "rusty", None).await.unwrap();
    assert_eq!(rust.source, Source::Syntax);
}

/// Upstream TypeScript tags.scm has no class or function patterns of its
/// own; the facade prepends JavaScript's (manifest `tags_extends`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typescript_definitions_come_from_the_joined_javascript_tags() {
    let ts = "export class Widget {\n  render(): void {}\n}\n\nexport function makeWidget(): Widget {\n  return new Widget();\n}\n\ninterface Props { id: number }\n";
    let tsx = "export function Button() {\n  return <div />;\n}\n";
    let files = [("ui/widget.ts", ts), ("ui/button.tsx", tsx)];

    let env = Env::new();
    let wt = repo(&files);
    let ci = env.facade(StaticGrammars::all());
    ci.open(workspace(wt.path(), TrustMode::Review))
        .await
        .unwrap();
    indexed(&ci, wt.path()).await;
    for (name, kind, path) in [
        ("Widget", CodeSymbolKind::Class, "ui/widget.ts"),
        ("makeWidget", CodeSymbolKind::Function, "ui/widget.ts"),
        ("render", CodeSymbolKind::Method, "ui/widget.ts"),
        ("Props", CodeSymbolKind::Interface, "ui/widget.ts"),
        ("Button", CodeSymbolKind::Function, "ui/button.tsx"),
    ] {
        let a = ci.find_symbol(wt.path(), name, Some(kind)).await.unwrap();
        assert_eq!(a.source, Source::Syntax, "{name}");
        assert_eq!(a.items.len(), 1, "{name}: {:?}", a.items);
        assert_eq!(a.items[0].path, path);
    }

    // Control: without the JavaScript query, TypeScript's own finds the
    // interface but not the class.
    let env = Env::new();
    let wt = repo(&files);
    let ci = env.facade(StaticGrammars::without(&["javascript"]));
    ci.open(workspace(wt.path(), TrustMode::Review))
        .await
        .unwrap();
    indexed(&ci, wt.path()).await;
    let class = ci
        .find_symbol(wt.path(), "Widget", Some(CodeSymbolKind::Class))
        .await
        .unwrap();
    assert!(class.items.iter().all(|s| s.source != Source::Syntax));
    let iface = ci.find_symbol(wt.path(), "Props", None).await.unwrap();
    assert_eq!(iface.source, Source::Syntax);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_changed_reindexes_and_removes() {
    let env = Env::new();
    let wt = repo(&[("src/lib.rs", LIB)]);
    let ci = env.facade(StaticGrammars::all());
    ci.open(workspace(wt.path(), TrustMode::Trusted))
        .await
        .unwrap();
    indexed(&ci, wt.path()).await;
    assert!(
        ci.find_symbol(wt.path(), "added", None)
            .await
            .unwrap()
            .items
            .is_empty()
    );

    std::fs::write(
        wt.path().join("src/lib.rs"),
        format!("{LIB}\npub fn added() {{}}\n"),
    )
    .unwrap();
    std::fs::write(wt.path().join("src/new.rs"), "pub fn fresh() {}\n").unwrap();
    ci.file_changed(wt.path(), "src/lib.rs").await;
    ci.file_changed(wt.path(), "src/new.rs").await;
    for name in ["added", "fresh"] {
        eventually(name, || async {
            let a = ci.find_symbol(wt.path(), name, None).await.unwrap();
            a.source == Source::Syntax && a.items.len() == 1
        })
        .await;
    }

    std::fs::remove_file(wt.path().join("src/new.rs")).unwrap();
    ci.file_changed(wt.path(), "src/new.rs").await;
    let gone = ci.find_symbol(wt.path(), "fresh", None).await.unwrap();
    assert!(gone.items.is_empty(), "{:?}", gone.items);
    assert_eq!(status_of(&ci, wt.path(), "rust").await.files, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_per_language() {
    let env = Env::new();
    let wt = repo(&[
        ("src/a.rs", "fn a() {}\n"),
        ("src/b.rs", "fn b() {}\n"),
        ("main.go", "package main\n"),
        ("notes.txt", "plain\n"),
    ]);
    let ci = env.facade(StaticGrammars::without(&["go"]));
    ci.open(workspace(wt.path(), TrustMode::Trusted))
        .await
        .unwrap();
    indexed(&ci, wt.path()).await;
    let status = ci.status(wt.path()).await.unwrap();
    let langs: Vec<(&str, usize)> = status
        .iter()
        .map(|s| (s.language.as_str(), s.files))
        .collect();
    assert_eq!(langs, vec![("rust", 2), ("go", 1)]);

    let rust = &status[0];
    assert_eq!(
        rust.grammar,
        InstallState::Installed {
            version: "static".into()
        }
    );
    assert_eq!(rust.index_percent, 100);
    let server = rust.server.as_ref().unwrap();
    assert_eq!(server.name, "rust-analyzer");
    assert_eq!(server.state, ServerState::NotRunning);
    assert!(matches!(
        server.install,
        InstallState::Missing | InstallState::Unavailable
    ));

    let go = &status[1];
    assert_eq!(go.grammar, InstallState::Unavailable);
    assert_eq!(go.index_percent, 0);
    assert_eq!(go.server.as_ref().unwrap().name, "gopls");

    ci.close(wt.path()).await;
    assert!(matches!(
        ci.status(wt.path()).await,
        Err(CodeIntelError::NotOpen(_))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paths_outside_the_worktree_are_rejected() {
    let env = Env::new();
    let wt = repo(&[("src/lib.rs", LIB)]);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.rs"), "fn secret() {}\n").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.rs"),
        wt.path().join("src/link.rs"),
    )
    .unwrap();
    let ci = env.facade(StaticGrammars::all());
    ci.open(workspace(wt.path(), TrustMode::Review))
        .await
        .unwrap();

    for bad in ["../x.rs", "/etc/passwd", "src/../../x.rs", "", "a\\b.rs"] {
        let outside = |r: Result<(), CodeIntelError>| {
            assert!(
                matches!(r, Err(CodeIntelError::OutsideWorkspace(_))),
                "{bad:?}: {r:?}"
            )
        };
        outside(ci.file_symbols(wt.path(), bad).await.map(|_| ()));
        outside(ci.definition(wt.path(), bad, at(1, 1)).await.map(|_| ()));
        outside(ci.references(wt.path(), bad, at(1, 1)).await.map(|_| ()));
        outside(ci.symbol_body(wt.path(), bad, "x").await.map(|_| ()));
        outside(ci.diagnostics(wt.path(), Some(bad)).await.map(|_| ()));
        ci.file_changed(wt.path(), bad).await; // ignored, no panic
    }
    // A symlink out of the worktree is not a file of it.
    ci.file_changed(wt.path(), "src/link.rs").await;
    indexed(&ci, wt.path()).await;
    assert!(
        ci.find_symbol(wt.path(), "secret", None)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        ci.symbol_body(wt.path(), "src/link.rs", "secret")
            .await
            .is_err()
    );

    let other = tempfile::tempdir().unwrap();
    assert!(matches!(
        ci.find_symbol(other.path(), "x", None).await,
        Err(CodeIntelError::NotOpen(_))
    ));
}
