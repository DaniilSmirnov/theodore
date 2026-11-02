//! The code tools (docs/code-intel.md §7) over a real server and client:
//! listed only for runs with code intelligence, edits only on a space turn
//! that may write (an Ask-mode turn hears why), result shapes and caps.
//! What the tools do is tested in `app`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use theodore_domain::{AgentId, FindingId, PrId, RunId};
use theodore_mcp::{
    BackendError, CODE_EDIT_TOOL_NAMES, CODE_TOOL_NAMES, CodeEditOutcome, CodeTools,
    CodeToolsBackend, GroupWithHunks, McpServer, NewFinding, NewPatch, NoSpaceTools, PrContext,
    REVIEW_TOOL_NAMES, ReviewToolsBackend, RunContext, SPACE_TOOL_NAMES, ToolSet,
};
use theodore_ports::{
    Answer, CodeHit, CodeSymbol, CodeSymbolKind, Diagnostic, LanguageStatus, Location, Position,
    Range, Source,
};

struct EmptyReview;

#[async_trait]
impl ReviewToolsBackend for EmptyReview {
    async fn get_pr_context(&self, _: &RunContext) -> Result<PrContext, BackendError> {
        Err(BackendError::NotFound("none".into()))
    }
    async fn get_group(&self, _: &RunContext, _: &str) -> Result<GroupWithHunks, BackendError> {
        Err(BackendError::NotFound("none".into()))
    }
    async fn get_base_file(&self, _: &RunContext, _: &str) -> Result<Option<String>, BackendError> {
        Ok(None)
    }
    async fn add_finding(&self, _: &RunContext, _: NewFinding) -> Result<FindingId, BackendError> {
        Ok(FindingId::new("f"))
    }
    async fn propose_patch(&self, _: &RunContext, _: NewPatch) -> Result<(), BackendError> {
        Ok(())
    }
}

/// Records calls; `references` answers with `hits` hits.
struct FakeCode {
    calls: Mutex<Vec<String>>,
    hits: usize,
}

fn at(line: u32, col: u32) -> Position {
    Position { line, col }
}

fn hit(i: usize) -> CodeHit {
    CodeHit {
        location: Location {
            path: format!("src/f{i}.rs"),
            range: Range {
                start: at(i as u32 + 1, 5),
                end: at(i as u32 + 1, 9),
            },
        },
        preview: "x".repeat(1_000),
        is_definition: false,
        source: Source::Lsp,
    }
}

#[async_trait]
impl CodeToolsBackend for FakeCode {
    async fn find_symbol(
        &self,
        run: &RunContext,
        name: &str,
        kind: Option<CodeSymbolKind>,
    ) -> Result<Answer<CodeSymbol>, BackendError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("find_symbol {} {name} {kind:?}", run.run_id));
        Ok(Answer {
            items: vec![CodeSymbol {
                name: name.into(),
                kind: CodeSymbolKind::Function,
                range: Range {
                    start: at(3, 1),
                    end: at(9, 2),
                },
                name_range: Range {
                    start: at(3, 8),
                    end: at(3, 12),
                },
                path: "src/lib.rs".into(),
                container: Some("impl Store".into()),
                source: Source::Syntax,
            }],
            source: Source::Syntax,
            index_percent: 62,
            note: Some("rust-analyzer warming up".into()),
        })
    }
    async fn get_symbol(
        &self,
        _: &RunContext,
        _: &str,
        _: &str,
    ) -> Result<(CodeSymbol, String), BackendError> {
        Err(BackendError::NotFound("no symbol".into()))
    }
    async fn references(
        &self,
        _: &RunContext,
        path: &str,
        at: Position,
    ) -> Result<Answer<CodeHit>, BackendError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("references {path}:{}:{}", at.line, at.col));
        Ok(Answer {
            items: (0..self.hits).map(hit).collect(),
            source: Source::Lsp,
            index_percent: 100,
            note: None,
        })
    }
    async fn definition(
        &self,
        _: &RunContext,
        _: &str,
        _: Position,
    ) -> Result<Answer<CodeHit>, BackendError> {
        Err(BackendError::Invalid("unused".into()))
    }
    async fn diagnostics(
        &self,
        _: &RunContext,
        _: Option<&str>,
    ) -> Result<Answer<Diagnostic>, BackendError> {
        Err(BackendError::Invalid("unused".into()))
    }
    async fn code_status(&self, _: &RunContext) -> Result<Vec<LanguageStatus>, BackendError> {
        Ok(vec![])
    }
    async fn replace_symbol_body(
        &self,
        _: &RunContext,
        path: &str,
        name: &str,
        _: &str,
    ) -> Result<CodeEditOutcome, BackendError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("replace_symbol_body {path} {name}"));
        Ok(CodeEditOutcome {
            files: vec![path.into()],
            edits: 1,
        })
    }
    async fn insert_after_symbol(
        &self,
        _: &RunContext,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<CodeEditOutcome, BackendError> {
        Err(BackendError::Invalid("unused".into()))
    }
    async fn rename_symbol(
        &self,
        _: &RunContext,
        _: &str,
        _: Position,
        _: &str,
    ) -> Result<CodeEditOutcome, BackendError> {
        Err(BackendError::Invalid("unused".into()))
    }
}

fn review(code: CodeTools) -> RunContext {
    let mut run = RunContext::review(RunId::new("r1"), PrId::new("github:o/r#1"), AgentId::Claude);
    run.code = code;
    run
}

fn space(code: CodeTools) -> RunContext {
    RunContext {
        run_id: RunId::new("space:s1:m1"),
        pr: PrId::new("space:s1"),
        agent: AgentId::Claude,
        space: Some("s1".into()),
        tools: ToolSet::Space,
        code,
    }
}

async fn connect(url: &str, token: &str) -> RunningService<rmcp::RoleClient, ()> {
    let config = StreamableHttpClientTransportConfig::with_uri(url).auth_header(token);
    ().serve(StreamableHttpClientTransport::from_config(config))
        .await
        .expect("connect")
}

async fn names(client: &RunningService<rmcp::RoleClient, ()>) -> Vec<String> {
    let mut names: Vec<String> = client
        .list_all_tools()
        .await
        .expect("list tools")
        .iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort();
    names
}

async fn call(
    client: &RunningService<rmcp::RoleClient, ()>,
    tool: &str,
    args: Value,
) -> CallToolResult {
    let mut req = CallToolRequestParams::new(tool.to_string());
    if let Value::Object(map) = args {
        req = req.with_arguments(map);
    }
    client.call_tool(req).await.expect("call")
}

fn json_of(result: &CallToolResult) -> Value {
    assert_eq!(result.is_error, Some(false), "{result:?}");
    serde_json::from_str(&result.content[0].as_text().expect("text").text).expect("json")
}

fn sorted(sets: &[&[&str]]) -> Vec<String> {
    let mut v: Vec<String> = sets
        .iter()
        .flat_map(|s| s.iter())
        .map(|s| s.to_string())
        .collect();
    v.sort();
    v
}

async fn server(code: Arc<FakeCode>) -> theodore_mcp::McpHandle {
    McpServer::start_with_code(Arc::new(EmptyReview), Arc::new(NoSpaceTools), code)
        .await
        .unwrap()
}

fn fake(hits: usize) -> Arc<FakeCode> {
    Arc::new(FakeCode {
        calls: Mutex::new(Vec::new()),
        hits,
    })
}

#[tokio::test]
async fn code_tools_are_listed_only_when_the_run_has_them() {
    let handle = server(fake(0)).await;
    for (run, want) in [
        (review(CodeTools::Off), sorted(&[REVIEW_TOOL_NAMES])),
        (
            review(CodeTools::Read),
            sorted(&[REVIEW_TOOL_NAMES, CODE_TOOL_NAMES]),
        ),
        // A PR run never edits, whatever it was registered with.
        (
            review(CodeTools::Edit),
            sorted(&[REVIEW_TOOL_NAMES, CODE_TOOL_NAMES]),
        ),
        (space(CodeTools::Off), sorted(&[SPACE_TOOL_NAMES])),
        (
            space(CodeTools::Read),
            sorted(&[SPACE_TOOL_NAMES, CODE_TOOL_NAMES]),
        ),
        (
            space(CodeTools::Edit),
            sorted(&[SPACE_TOOL_NAMES, CODE_TOOL_NAMES, CODE_EDIT_TOOL_NAMES]),
        ),
    ] {
        let endpoint = handle.register_run(run.clone());
        let client = connect(&endpoint.url, &endpoint.token).await;
        assert_eq!(names(&client).await, want, "{run:?}");
    }
    // Off: not callable either.
    let endpoint = handle.register_run(review(CodeTools::Off));
    let client = connect(&endpoint.url, &endpoint.token).await;
    let refused = client
        .call_tool(
            CallToolRequestParams::new("find_symbol")
                .with_arguments(json!({"name": "x"}).as_object().unwrap().clone()),
        )
        .await;
    assert!(refused.is_err(), "{refused:?}");
}

#[tokio::test]
async fn an_ask_mode_turn_hears_why_it_cannot_edit() {
    let code = fake(0);
    let handle = server(code.clone()).await;
    let endpoint = handle.register_run(space(CodeTools::Read));
    let client = connect(&endpoint.url, &endpoint.token).await;
    let refused = call(
        &client,
        "replace_symbol_body",
        json!({"path": "src/lib.rs", "name": "save", "body": "fn save() {}"}),
    )
    .await;
    assert_eq!(refused.is_error, Some(true));
    let text = &refused.content[0].as_text().unwrap().text;
    assert!(text.contains("Ask mode"), "{text}");
    assert!(
        code.calls.lock().unwrap().is_empty(),
        "never reached the backend"
    );

    let endpoint = handle.register_run(space(CodeTools::Edit));
    let client = connect(&endpoint.url, &endpoint.token).await;
    let done = json_of(
        &call(
            &client,
            "replace_symbol_body",
            json!({"path": "src/lib.rs", "name": "save", "body": "fn save() {}"}),
        )
        .await,
    );
    assert_eq!(done, json!({"files": ["src/lib.rs"], "edits": 1}));
}

#[tokio::test]
async fn answers_carry_their_source_and_are_capped() {
    let code = fake(60);
    let handle = server(code.clone()).await;
    let endpoint = handle.register_run(review(CodeTools::Read));
    let client = connect(&endpoint.url, &endpoint.token).await;

    let found = json_of(
        &call(
            &client,
            "find_symbol",
            json!({"name": "save", "kind": "method"}),
        )
        .await,
    );
    assert_eq!(
        found,
        json!({
            "items": [{
                "name": "save", "kind": "function", "path": "src/lib.rs",
                "line": 3, "col": 8, "end_line": 9, "container": "impl Store",
                "source": "syntax",
            }],
            "total": 1,
            "truncated": false,
            "source": "syntax",
            "index_percent": 62,
            "note": "rust-analyzer warming up",
        })
    );

    let refs = json_of(
        &call(
            &client,
            "references",
            json!({"path": "src/lib.rs", "line": 3, "col": 8}),
        )
        .await,
    );
    assert_eq!(refs["items"].as_array().unwrap().len(), 50);
    assert_eq!(refs["total"], json!(60));
    assert_eq!(refs["truncated"], json!(true));
    assert_eq!(refs["source"], json!("lsp"));
    assert!(refs.get("note").is_none());
    let preview = refs["items"][0]["preview"].as_str().unwrap();
    assert_eq!(preview.chars().count(), 401, "400 chars and an ellipsis");
    assert_eq!(
        *code.calls.lock().unwrap(),
        [
            "find_symbol r1 save Some(Method)",
            "references src/lib.rs:3:8"
        ]
    );

    let missing = call(&client, "get_symbol", json!({"path": "a.rs", "name": "x"})).await;
    assert_eq!(missing.is_error, Some(true));
}
