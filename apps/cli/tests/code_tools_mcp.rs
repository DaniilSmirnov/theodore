//! `theodore code-tools-mcp` over a pipe: a temp repo, a facade with a
//! static Rust grammar (no downloads, no language server on `$PATH`), the
//! warm-up helper, then an MCP client listing and calling the tools.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Value, json};
use theodore_cli::code_tools_mcp;
use theodore_code_intel::{CodeIntel, CodeIntelOptions, GrammarSource};
use theodore_code_intel_toolchain::{LoadedGrammar, Toolchain};
use theodore_domain::AgentId;
use theodore_mcp::{CODE_EDIT_TOOL_NAMES, CODE_TOOL_NAMES};
use theodore_ports::{CodeIntelError, CodeIntelResult, InstallState, LanguageId, TrustMode};

struct RustOnly;

#[async_trait]
impl GrammarSource for RustOnly {
    async fn grammar(&self, language: &LanguageId) -> CodeIntelResult<LoadedGrammar> {
        if language.as_str() != "rust" {
            return Err(CodeIntelError::Unsupported(format!(
                "{language}: none here"
            )));
        }
        Ok(LoadedGrammar {
            language_id: language.clone(),
            language: tree_sitter_rust::LANGUAGE.into(),
            tags_query: tree_sitter_rust::TAGS_QUERY.to_string(),
        })
    }

    fn state(&self, language: &LanguageId) -> InstallState {
        if language.as_str() == "rust" {
            InstallState::Installed {
                version: "static".into(),
            }
        } else {
            InstallState::Unavailable
        }
    }
}

const LIB: &str = "pub struct Store {}\n\nimpl Store {\n    pub fn save(&self) -> u32 {\n        1\n    }\n}\n\npub fn helper() -> u32 {\n    Store {}.save()\n}\n";
const MAIN: &str =
    "mod lib;\nuse lib::helper;\n\nfn main() {\n    let n = helper();\n    println!(\"{n}\");\n}\n";

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), LIB).unwrap();
    std::fs::write(dir.path().join("src/main.rs"), MAIN).unwrap();
    std::fs::write(dir.path().join("README.md"), "# r\n").unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

/// The facade `code-tools-mcp` builds, with the static grammar and an
/// empty `$PATH` (no rust-analyzer).
fn facade(tmp: &Path) -> CodeIntel {
    let data = tmp.join("code-intel");
    let bin = tmp.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let tc = Toolchain::builder(&data)
        .search_path(bin.into_os_string())
        .build();
    CodeIntel::builder(CodeIntelOptions::new(&data))
        .toolchain(tc)
        .grammar_source(Arc::new(RustOnly))
        .index_workers(2)
        .build()
        .unwrap()
}

fn json_of(r: &CallToolResult) -> Value {
    assert_eq!(r.is_error, Some(false), "{r:?}");
    let text = r.content[0].as_text().expect("text content").text.clone();
    serde_json::from_str(&text).unwrap()
}

fn call(name: &str, args: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_string()).with_arguments(args.as_object().unwrap().clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serves_the_code_tools_over_a_pipe() {
    let wt = repo();
    let tmp = tempfile::tempdir().unwrap();
    let ci = facade(tmp.path());
    let root = code_tools_mcp::open_root(&ci, wt.path(), TrustMode::Trusted)
        .await
        .unwrap();

    let warm = code_tools_mcp::warm(&ci, &root, Duration::from_secs(20), false)
        .await
        .unwrap();
    assert!(warm.ready, "{warm:?}");
    let rust = warm
        .languages
        .iter()
        .find(|l| l.language == "rust")
        .unwrap();
    assert_eq!((rust.files, rust.index_percent), (2, 100));
    assert_eq!(rust.server.as_deref(), Some("rust-analyzer"));

    let (server_side, client_side) = tokio::io::duplex(256 * 1024);
    let server = tokio::spawn(theodore_mcp::serve_code_tools(
        code_tools_mcp::backend(&ci, root.clone()),
        theodore_mcp::code_run(AgentId::Claude, false),
        server_side,
    ));
    let client = ().serve(client_side).await.unwrap();

    // Read-only: the six code tools, nothing of review-tools or space-tools.
    let mut names: Vec<String> = client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort();
    let mut want: Vec<String> = CODE_TOOL_NAMES.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(names, want);

    let found = json_of(
        &client
            .call_tool(call("find_symbol", json!({ "name": "helper" })))
            .await
            .unwrap(),
    );
    assert_eq!(found["source"], "syntax");
    assert_eq!(found["total"], 1);
    assert_eq!(found["items"][0]["path"], "src/lib.rs");
    assert_eq!(found["items"][0]["line"], 9);
    assert!(
        found["note"]
            .as_str()
            .unwrap_or_default()
            .contains("rust-analyzer not installed"),
        "{found}"
    );

    let body = json_of(
        &client
            .call_tool(call(
                "get_symbol",
                json!({ "path": "src/lib.rs", "name": "helper" }),
            ))
            .await
            .unwrap(),
    );
    assert!(
        body["body"]
            .as_str()
            .unwrap()
            .starts_with("pub fn helper() -> u32 {"),
        "{body}"
    );
    assert_eq!(body["truncated"], false);

    let refs = json_of(
        &client
            .call_tool(call(
                "references",
                json!({ "path": "src/lib.rs", "line": 9, "col": 8 }),
            ))
            .await
            .unwrap(),
    );
    let paths: Vec<&str> = refs["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"src/main.rs"), "{refs}");

    let status = json_of(
        &client
            .call_tool(call("code_status", json!({})))
            .await
            .unwrap(),
    );
    assert_eq!(status["languages"][0]["language"], "rust");

    // An agent path never leaves the worktree.
    let outside = client
        .call_tool(call(
            "get_symbol",
            json!({ "path": "../x.rs", "name": "helper" }),
        ))
        .await
        .unwrap();
    assert_eq!(outside.is_error, Some(true));
    // The edits aren't there without --edit.
    assert!(
        client
            .call_tool(call(
                "replace_symbol_body",
                json!({ "path": "src/lib.rs", "name": "helper", "body": "x" }),
            ))
            .await
            .is_err()
    );

    drop(client);
    let _ = server.await;
    ci.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_edit_the_edits_write_the_worktree() {
    let wt = repo();
    let tmp = tempfile::tempdir().unwrap();
    let ci = facade(tmp.path());
    let root = code_tools_mcp::open_root(&ci, wt.path(), TrustMode::Trusted)
        .await
        .unwrap();
    code_tools_mcp::warm(&ci, &root, Duration::from_secs(20), false)
        .await
        .unwrap();

    let (server_side, client_side) = tokio::io::duplex(256 * 1024);
    let server = tokio::spawn(theodore_mcp::serve_code_tools(
        code_tools_mcp::backend(&ci, root.clone()),
        theodore_mcp::code_run(AgentId::Claude, true),
        server_side,
    ));
    let client = ().serve(client_side).await.unwrap();
    let names: Vec<String> = client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    for t in CODE_TOOL_NAMES.iter().chain(CODE_EDIT_TOOL_NAMES) {
        assert!(names.contains(&t.to_string()), "{t} missing: {names:?}");
    }
    assert!(!names.iter().any(|n| n == "approve_tool"), "{names:?}");

    let done = json_of(
        &client
            .call_tool(call(
                "replace_symbol_body",
                json!({
                    "path": "src/lib.rs",
                    "name": "helper",
                    "body": "pub fn helper() -> u32 {\n    2\n}",
                }),
            ))
            .await
            .unwrap(),
    );
    assert_eq!(done, json!({ "files": ["src/lib.rs"], "edits": 1 }));
    let text = std::fs::read_to_string(wt.path().join("src/lib.rs")).unwrap();
    assert!(
        text.ends_with("pub fn helper() -> u32 {\n    2\n}\n"),
        "{text}"
    );

    drop(client);
    let _ = server.await;
    ci.shutdown().await;
}

#[test]
fn edit_needs_trust() {
    let args = code_tools_mcp::CodeToolsMcpArgs {
        root: ".".into(),
        trust: code_tools_mcp::TrustArg::Review,
        edit: true,
        data_dir: None,
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let err = rt.block_on(code_tools_mcp::run(&args)).unwrap_err();
    assert!(err.to_string().contains("--edit needs --trust trusted"));
}
