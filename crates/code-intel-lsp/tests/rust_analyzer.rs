//! Against a real rust-analyzer on this crate (and the workspace it sits
//! in). Ignored by default:
//! `cargo test -p theodore-code-intel-lsp --test rust_analyzer -- --ignored --nocapture`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;
use theodore_code_intel_lsp::{LaunchConfig, LspSession, SessionOptions};
use theodore_ports::{LanguageId, Position, ServerState};

fn rust_analyzer() -> Option<PathBuf> {
    let out = std::process::Command::new("rustup")
        .args(["which", "rust-analyzer"])
        .output()
        .ok()?;
    if out.status.success() {
        return Some(PathBuf::from(String::from_utf8(out.stdout).ok()?.trim()));
    }
    // Not a rustup component: anything working on PATH.
    let out = std::process::Command::new("rust-analyzer")
        .arg("--version")
        .output()
        .ok()?;
    out.status.success().then(|| PathBuf::from("rust-analyzer"))
}

/// 1-based line and char column of the first `needle` in `text`.
fn find(text: &str, needle: &str) -> Position {
    let byte = text.find(needle).unwrap();
    let before = &text[..byte];
    let line = before.matches('\n').count() as u32 + 1;
    let col = before.rsplit('\n').next().unwrap().chars().count() as u32 + 1;
    Position { line, col }
}

#[tokio::test]
#[ignore = "needs rust-analyzer; slow"]
async fn rust_analyzer_resolves_a_definition_in_this_crate() {
    let Some(bin) = rust_analyzer() else {
        eprintln!("rust-analyzer not installed; skipping");
        return;
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let config = LaunchConfig {
        server: "rust-analyzer".into(),
        argv: vec![bin.display().to_string()],
        env: vec![],
        // Review-run trust flags (docs/code-intel.md §8), and no
        // `cargo check` racing this test for the target dir.
        init_options: json!({
            "cargo": {"buildScripts": {"enable": false}},
            "procMacro": {"enable": false},
            "checkOnSave": false,
        }),
        workspace_folders: true,
        language_ids: vec![(LanguageId::new("rust"), "rust".into(), vec!["rs".into()])],
    };
    let options = SessionOptions {
        request_timeout: Duration::from_secs(120),
        ready_max_wait: Duration::from_secs(600),
        ..SessionOptions::default()
    };
    let t0 = Instant::now();
    let s = LspSession::start(root, config, options).unwrap();
    let state = s.wait_ready(Duration::from_secs(600)).await;
    let warm = t0.elapsed();
    eprintln!("rust-analyzer: {state:?} after {warm:?}");
    assert_eq!(state, ServerState::Ready);

    let session_rs = std::fs::read_to_string(root.join("src/session.rs")).unwrap();
    let call = find(&session_rs, "text::resolve(&root, path)");
    let at = Position {
        line: call.line,
        col: call.col + "text::".len() as u32 + 1,
    };
    let t1 = Instant::now();
    let defs = s.definition(root, "src/session.rs", at).await.unwrap();
    eprintln!("definition: {defs:?} in {:?}", t1.elapsed());
    let text_rs = std::fs::read_to_string(root.join("src/text.rs")).unwrap();
    let def = find(&text_rs, "fn resolve(");
    assert_eq!(defs.len(), 1, "{defs:?}");
    assert_eq!(defs[0].path, "src/text.rs");
    assert_eq!(defs[0].range.start.line, def.line);

    let syms = s.document_symbols(root, "src/text.rs").await.unwrap();
    assert!(syms.iter().any(|s| s.name == "resolve"), "{syms:?}");
    s.shutdown().await;
}
