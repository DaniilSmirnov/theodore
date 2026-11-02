//! The `space-tools` tool set over a real server and client: which tools a
//! token sees and may call (space turn, review on a space's PR, plain
//! review), the result shapes, the trust-gate error and the log tail
//! bounds. The space logic itself is tested in `app`.

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
    AppStatus, BackendError, CheckOutcome, GroupWithHunks, LogTail, McpServer, NewFinding,
    NewPatch, PrContext, REVIEW_TOOL_NAMES, ReviewToolsBackend, RunContext, SPACE_TOOL_NAMES,
    SpaceCommandName, SpaceInfo, SpaceToolError, SpaceToolsBackend, ToolSet,
};

struct EmptyReview;

#[async_trait]
impl ReviewToolsBackend for EmptyReview {
    async fn get_pr_context(&self, _: &RunContext) -> Result<PrContext, BackendError> {
        Ok(PrContext {
            title: "t".into(),
            body: String::new(),
            commits: vec![],
            files: vec![],
            groups: vec![],
        })
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

/// Records every call as `"<tool> <space> [args]"`; `allowed: false`
/// refuses the commands the way the trust gate does.
struct FakeSpaces {
    calls: Mutex<Vec<String>>,
    allowed: bool,
}

impl FakeSpaces {
    fn new(allowed: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            allowed,
        })
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }

    fn gate(&self) -> Result<(), SpaceToolError> {
        if self.allowed {
            Ok(())
        } else {
            Err(SpaceToolError::NotAllowed(
                "`spaces.check` comes from the repo".into(),
            ))
        }
    }

    fn status(running: bool) -> AppStatus {
        AppStatus {
            configured: true,
            allowed: true,
            running,
            ports: vec![4100],
            urls: vec!["http://localhost:4100".into()],
            last_status: None,
            exit_code: None,
            error: None,
        }
    }
}

#[async_trait]
impl SpaceToolsBackend for FakeSpaces {
    async fn run_check(&self, space: &str, run: &RunId) -> Result<CheckOutcome, SpaceToolError> {
        self.record(format!("run_check {space} {run}"));
        self.gate()?;
        Ok(CheckOutcome {
            passed: false,
            exit_code: Some(1),
            timed_out: false,
            summary: "check exited with 1".into(),
            output: vec!["lint: 2 errors".into()],
        })
    }
    async fn start_app(&self, space: &str) -> Result<AppStatus, SpaceToolError> {
        self.record(format!("start_app {space}"));
        self.gate()?;
        Ok(Self::status(true))
    }
    async fn stop_app(&self, space: &str) -> Result<AppStatus, SpaceToolError> {
        self.record(format!("stop_app {space}"));
        Ok(Self::status(false))
    }
    async fn app_status(&self, space: &str) -> Result<AppStatus, SpaceToolError> {
        self.record(format!("app_status {space}"));
        Ok(Self::status(false))
    }
    async fn read_logs(
        &self,
        space: &str,
        kind: SpaceCommandName,
        tail: usize,
    ) -> Result<LogTail, SpaceToolError> {
        self.record(format!("read_logs {space} {} {tail}", kind.as_str()));
        Ok(LogTail {
            kind,
            lines: vec!["ready".into()],
            available: 1,
        })
    }
    async fn space_info(&self, space: &str) -> Result<SpaceInfo, SpaceToolError> {
        self.record(format!("space_info {space}"));
        Ok(SpaceInfo {
            name: "fix-login".into(),
            branch: "space/fix-login".into(),
            base: "main".into(),
            worktree: "/tmp/wt".into(),
            ports: vec![4100],
            urls: vec!["http://localhost:4100".into()],
            env: vec!["THEODORE_PORT".into()],
            commands: vec![],
        })
    }
}

fn space_run() -> RunContext {
    RunContext {
        run_id: RunId::new("space:s1:m1"),
        pr: PrId::new("space:s1"),
        agent: AgentId::Claude,
        space: Some("s1".into()),
        tools: ToolSet::Space,
        code: theodore_mcp::CodeTools::Off,
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

fn text_of(result: &CallToolResult) -> String {
    result.content[0].as_text().expect("text").text.clone()
}

fn sorted(names: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = names.iter().map(|s| s.to_string()).collect();
    v.sort();
    v
}

#[tokio::test]
async fn a_space_turn_gets_only_the_space_tools() {
    let spaces = FakeSpaces::new(true);
    let handle = McpServer::start_with_spaces(Arc::new(EmptyReview), spaces.clone())
        .await
        .unwrap();
    let endpoint = handle.register_run(space_run());
    assert_eq!(endpoint.name, "space-tools");
    let client = connect(&endpoint.url, &endpoint.token).await;

    assert_eq!(names(&client).await, sorted(SPACE_TOOL_NAMES));
    let refused = client
        .call_tool(CallToolRequestParams::new("get_pr_context"))
        .await;
    assert!(
        refused.is_err(),
        "a review tool isn't callable: {refused:?}"
    );

    let check = json_of(&call(&client, "run_check", json!({})).await);
    assert_eq!(
        check,
        json!({
            "passed": false,
            "exit_code": 1,
            "timed_out": false,
            "summary": "check exited with 1",
            "output": ["lint: 2 errors"],
        })
    );
    let started = json_of(&call(&client, "start_app", json!({})).await);
    assert_eq!(started["running"], json!(true));
    assert_eq!(started["urls"], json!(["http://localhost:4100"]));
    let stopped = json_of(&call(&client, "stop_app", json!({})).await);
    assert_eq!(stopped["running"], json!(false));
    let status = json_of(&call(&client, "app_status", json!({})).await);
    assert_eq!(status["ports"], json!([4100]));
    let info = json_of(&call(&client, "space_info", json!({})).await);
    assert_eq!(info["env"], json!(["THEODORE_PORT"]));
    let logs = json_of(&call(&client, "read_logs", json!({ "kind": "run" })).await);
    assert_eq!(logs["kind"], json!("run"));

    // The space is the token's, whatever the agent sends.
    assert_eq!(
        spaces.calls(),
        [
            "run_check s1 space:s1:m1",
            "start_app s1",
            "stop_app s1",
            "app_status s1",
            "space_info s1",
            "read_logs s1 run 100",
        ]
    );
    drop(client);
    handle.shutdown().await;
}

#[tokio::test]
async fn read_logs_tail_is_bounded() {
    let spaces = FakeSpaces::new(true);
    let handle = McpServer::start_with_spaces(Arc::new(EmptyReview), spaces.clone())
        .await
        .unwrap();
    let endpoint = handle.register_run(space_run());
    let client = connect(&endpoint.url, &endpoint.token).await;
    for tail in [json!(100000), json!(0), json!(7)] {
        call(
            &client,
            "read_logs",
            json!({ "kind": "check", "tail": tail }),
        )
        .await;
    }
    let bad = client
        .call_tool(
            CallToolRequestParams::new("read_logs")
                .with_arguments(json!({ "kind": "shell" }).as_object().unwrap().clone()),
        )
        .await;
    let rejected = match bad {
        Err(_) => true,
        Ok(r) => r.is_error == Some(true),
    };
    assert!(rejected, "an unknown kind is refused");
    assert_eq!(
        spaces.calls(),
        [
            "read_logs s1 check 400",
            "read_logs s1 check 1",
            "read_logs s1 check 7"
        ]
    );
    drop(client);
    handle.shutdown().await;
}

#[tokio::test]
async fn the_trust_gate_refusal_reads_clearly() {
    let spaces = FakeSpaces::new(false);
    let handle = McpServer::start_with_spaces(Arc::new(EmptyReview), spaces.clone())
        .await
        .unwrap();
    let endpoint = handle.register_run(space_run());
    let client = connect(&endpoint.url, &endpoint.token).await;
    for tool in ["run_check", "start_app"] {
        let r = call(&client, tool, json!({})).await;
        assert_eq!(r.is_error, Some(true));
        assert!(
            text_of(&r).starts_with("not allowed by the user yet"),
            "{}",
            text_of(&r)
        );
    }
    drop(client);
    handle.shutdown().await;
}

#[tokio::test]
async fn a_review_on_a_space_pr_reads_logs_and_status_only() {
    let spaces = FakeSpaces::new(true);
    let handle = McpServer::start_with_spaces(Arc::new(EmptyReview), spaces.clone())
        .await
        .unwrap();
    let mut run = RunContext::review(RunId::new("r1"), PrId::new("local:p#1"), AgentId::Codex);
    run.space = Some("s1".into());
    let endpoint = handle.register_run(run);
    assert_eq!(endpoint.name, "review-tools");
    let client = connect(&endpoint.url, &endpoint.token).await;

    let mut expected: Vec<&str> = REVIEW_TOOL_NAMES.to_vec();
    expected.extend(["read_logs", "app_status"]);
    assert_eq!(names(&client).await, sorted(&expected));
    for tool in ["run_check", "start_app", "stop_app", "space_info"] {
        let r = client
            .call_tool(CallToolRequestParams::new(tool.to_string()))
            .await;
        assert!(r.is_err(), "{tool} is refused");
    }
    json_of(&call(&client, "app_status", json!({})).await);
    assert_eq!(spaces.calls(), ["app_status s1"]);
    drop(client);
    handle.shutdown().await;
}

#[tokio::test]
async fn a_plain_review_gets_no_space_tools() {
    let spaces = FakeSpaces::new(true);
    let handle = McpServer::start_with_spaces(Arc::new(EmptyReview), spaces.clone())
        .await
        .unwrap();
    let endpoint = handle.register_run(RunContext::review(
        RunId::new("r1"),
        PrId::new("github:o/r#1"),
        AgentId::Claude,
    ));
    let client = connect(&endpoint.url, &endpoint.token).await;
    assert_eq!(names(&client).await, sorted(REVIEW_TOOL_NAMES));
    let r = client
        .call_tool(CallToolRequestParams::new("read_logs"))
        .await;
    assert!(r.is_err());
    assert!(spaces.calls().is_empty());
    drop(client);
    handle.shutdown().await;
}
