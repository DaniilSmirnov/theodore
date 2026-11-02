//! Whether tsgo's LSP mode is usable for definition, references and
//! documentSymbol (docs/code-intel.md §11 "tsgo's LSP maturity"): drives a
//! downloaded tsgo through `theodore-code-intel-lsp`'s real client against
//! a tiny two-file TS project, not just `tsgo --help`.
//!
//! Network (downloads tsgo through `Toolchain::install_server`) and slow:
//! ignored by default.
//! `cargo test -p theodore-code-intel-toolchain --test tsgo_lsp -- --ignored --nocapture`.

use std::time::Duration;

use serde_json::json;
use theodore_code_intel_lsp::{LaunchConfig, LspSession, SessionOptions};
use theodore_code_intel_toolchain::Toolchain;
use theodore_ports::{LanguageId, Position, ServerState};

/// 1-based line and char column of the first `needle` in `text`.
fn find(text: &str, needle: &str) -> Position {
    let byte = text.find(needle).unwrap();
    let before = &text[..byte];
    let line = before.matches('\n').count() as u32 + 1;
    let col = before.rsplit('\n').next().unwrap().chars().count() as u32 + 1;
    Position { line, col }
}

#[tokio::test]
#[ignore = "network: downloads tsgo; slow"]
async fn tsgo_resolves_definition_references_and_symbols() {
    let dl_root = tempfile::tempdir().unwrap();
    let tc = Toolchain::new(dl_root.path());
    let spec = tc
        .manifest()
        .server("tsgo")
        .expect("tsgo in the builtin manifest")
        .clone();
    let resolved = tc
        .install_server(&spec)
        .await
        .expect("tsgo install (network)");
    eprintln!("tsgo installed at {}", resolved.bin.display());

    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("a.ts"),
        "export function greet(name: string): string {\n  return `hi ${name}`;\n}\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join("b.ts"),
        "import { greet } from \"./a\";\n\nconsole.log(greet(\"world\"));\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join("tsconfig.json"),
        r#"{"compilerOptions": {"module": "commonjs", "target": "es2020", "moduleResolution": "node"}}"#,
    )
    .unwrap();

    let config = LaunchConfig {
        server: "tsgo".into(),
        argv: resolved
            .argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
        env: vec![],
        init_options: json!({}),
        workspace_folders: true,
        language_ids: vec![(
            LanguageId::new("typescript"),
            "typescript".into(),
            vec!["ts".into()],
        )],
    };
    let options = SessionOptions {
        request_timeout: Duration::from_secs(60),
        init_timeout: Duration::from_secs(60),
        ready_max_wait: Duration::from_secs(60),
        ..SessionOptions::default()
    };
    let s = LspSession::start(project.path(), config, options).unwrap();
    let state = s.wait_ready(Duration::from_secs(60)).await;
    eprintln!("tsgo state: {state:?}");
    assert_eq!(state, ServerState::Ready, "tsgo did not warm up");

    let b_ts = std::fs::read_to_string(project.path().join("b.ts")).unwrap();
    let call_at = find(&b_ts, "greet(\"world\")");
    let defs = s.definition(project.path(), "b.ts", call_at).await;
    eprintln!("definition(b.ts call site): {defs:?}");

    let a_ts = std::fs::read_to_string(project.path().join("a.ts")).unwrap();
    let decl_at = find(&a_ts, "function greet");
    let name_at = Position {
        line: decl_at.line,
        col: decl_at.col + "function ".len() as u32,
    };
    let refs = s.references(project.path(), "a.ts", name_at, true).await;
    eprintln!("references(a.ts decl): {refs:?}");

    let syms = s.document_symbols(project.path(), "a.ts").await;
    eprintln!("document_symbols(a.ts): {syms:?}");

    s.shutdown().await;

    let defs = defs.expect("tsgo: definition request errored");
    assert!(!defs.is_empty(), "tsgo: definition returned no results");
    assert_eq!(defs[0].path, "a.ts");

    let refs = refs.expect("tsgo: references request errored");
    assert!(!refs.is_empty(), "tsgo: references returned no results");

    let syms = syms.expect("tsgo: documentSymbol request errored");
    assert!(
        syms.iter().any(|sym| sym.name == "greet"),
        "tsgo: no `greet` in documentSymbol: {syms:?}"
    );
}
