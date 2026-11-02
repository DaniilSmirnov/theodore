//! The change tools of `space-tools` (`ship`, `get_space_diff`,
//! `get_review_comments`, `add_review_comment`, `set_base`,
//! `update_progress`) over a real server and client: each reaches the
//! backend with the token's space, run and agent, the result shapes, and
//! a review run on a space's PR never sees them.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use theodore_domain::{
    AgentId, DiffLine, FileDiff, FileStatus, FindingId, Hunk, HunkId, LineKind, PrId, RunId, Sha,
};
use theodore_mcp::space_agent::MAX_DIFF_CHARS;
use theodore_mcp::{
    AppStatus, BackendError, BaseChange, CheckOutcome, GroupWithHunks, LogTail, McpServer,
    NewFinding, NewPatch, NewReviewComment, PrContext, ProgressStep, ReviewItem,
    ReviewToolsBackend, RunContext, ShipOutcome, ShipToolAction, SpaceCommandName, SpaceDiff,
    SpaceInfo, SpaceToolError, SpaceToolsBackend, ToolSet,
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

/// Records every call as `"<tool> <space> [args]"`.
#[derive(Default)]
struct FakeChange {
    calls: Mutex<Vec<String>>,
    /// Lines of the diff `space_diff` returns.
    diff_lines: usize,
}

impl FakeChange {
    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

fn no(_: &str) -> SpaceToolError {
    SpaceToolError::Unavailable("not in this fake".into())
}

#[async_trait]
impl SpaceToolsBackend for FakeChange {
    async fn run_check(&self, s: &str, _: &RunId) -> Result<CheckOutcome, SpaceToolError> {
        Err(no(s))
    }
    async fn start_app(&self, s: &str) -> Result<AppStatus, SpaceToolError> {
        Err(no(s))
    }
    async fn stop_app(&self, s: &str) -> Result<AppStatus, SpaceToolError> {
        Err(no(s))
    }
    async fn app_status(&self, s: &str) -> Result<AppStatus, SpaceToolError> {
        Err(no(s))
    }
    async fn read_logs(
        &self,
        s: &str,
        _: SpaceCommandName,
        _: usize,
    ) -> Result<LogTail, SpaceToolError> {
        Err(no(s))
    }
    async fn space_info(&self, s: &str) -> Result<SpaceInfo, SpaceToolError> {
        Err(no(s))
    }

    async fn ship(
        &self,
        space: &str,
        run: &RunId,
        action: ShipToolAction,
        title: Option<String>,
    ) -> Result<ShipOutcome, SpaceToolError> {
        self.record(format!("ship {space} {run} {action:?} {title:?}"));
        if action == ShipToolAction::Push {
            return Err(SpaceToolError::Unavailable(
                "open a PR before pushing".into(),
            ));
        }
        Ok(ShipOutcome {
            action,
            branch: "ada/fix-login".into(),
            sha: "squash1".into(),
            pr_number: Some(12),
            pr_url: Some("https://github.com/acme/widget/pull/12".into()),
            summary: "Opened PR #12 from ada/fix-login".into(),
        })
    }
    async fn space_diff(
        &self,
        space: &str,
        file: Option<&str>,
    ) -> Result<SpaceDiff, SpaceToolError> {
        self.record(format!("space_diff {space} {file:?}"));
        let lines = (0..self.diff_lines)
            .map(|i| DiffLine {
                kind: LineKind::Add,
                old_no: None,
                new_no: Some(i as u32 + 1),
                text: "x".repeat(40),
            })
            .collect();
        let hunk = Hunk {
            id: HunkId::new("h1"),
            path: "src/a.rs".into(),
            old_start: 0,
            old_lines: 0,
            new_start: 1,
            new_lines: self.diff_lines as u32,
            header: String::new(),
            lines,
        };
        Ok(SpaceDiff {
            base: "main".into(),
            base_sha: Sha::new("base000"),
            head: Sha::new("snap1"),
            uncommitted: true,
            files: vec![FileDiff {
                path: "src/a.rs".into(),
                old_path: None,
                status: FileStatus::Added,
                similarity: None,
                old_mode: None,
                new_mode: None,
                flags: vec![],
                additions: self.diff_lines as u32,
                deletions: 0,
                hunks: vec![hunk],
            }],
        })
    }
    async fn review_comments(
        &self,
        space: &str,
        file: Option<&str>,
    ) -> Result<Vec<ReviewItem>, SpaceToolError> {
        self.record(format!("review_comments {space} {file:?}"));
        Ok(vec![])
    }
    async fn add_review_comment(
        &self,
        space: &str,
        agent: AgentId,
        c: NewReviewComment,
    ) -> Result<FindingId, SpaceToolError> {
        self.record(format!(
            "add_review_comment {space} {} {}:{} {:?}",
            agent.as_str(),
            c.file,
            c.line,
            c.severity
        ));
        Ok(FindingId::new("f1"))
    }
    async fn set_base(
        &self,
        space: &str,
        run: &RunId,
        branch: &str,
    ) -> Result<BaseChange, SpaceToolError> {
        self.record(format!("set_base {space} {run} {branch}"));
        Ok(BaseChange {
            base: branch.into(),
            base_sha: "rel000".into(),
            previous: "main".into(),
            note: None,
        })
    }
    async fn update_progress(
        &self,
        space: &str,
        run: &RunId,
        steps: Vec<ProgressStep>,
        note: Option<String>,
    ) -> Result<(), SpaceToolError> {
        self.record(format!(
            "update_progress {space} {run} {} {note:?}",
            steps.len()
        ));
        Ok(())
    }
}

fn space_run() -> RunContext {
    RunContext {
        run_id: RunId::new("space:s1:m1"),
        pr: PrId::new("space:s1"),
        agent: AgentId::Codex,
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

#[tokio::test]
async fn the_change_tools_reach_the_backend_with_the_tokens_space() {
    let spaces = Arc::new(FakeChange {
        diff_lines: 2,
        ..Default::default()
    });
    let handle = McpServer::start_with_spaces(Arc::new(EmptyReview), spaces.clone())
        .await
        .unwrap();
    let endpoint = handle.register_run(space_run());
    let client = connect(&endpoint.url, &endpoint.token).await;

    let shipped = json_of(
        &call(
            &client,
            "ship",
            json!({ "action": "open_draft_pr", "title": "Fix login" }),
        )
        .await,
    );
    assert_eq!(shipped["pr_number"], json!(12));
    assert_eq!(shipped["action"], json!("open_draft_pr"));
    let refused = call(&client, "ship", json!({ "action": "push" })).await;
    assert_eq!(refused.is_error, Some(true));
    assert_eq!(
        refused.content[0].as_text().unwrap().text,
        "open a PR before pushing"
    );
    let bad = client
        .call_tool(
            CallToolRequestParams::new("ship").with_arguments(
                json!({ "action": "force_push" })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await;
    assert!(
        bad.is_err() || bad.unwrap().is_error == Some(true),
        "an unknown action is refused"
    );

    let diff = json_of(&call(&client, "get_space_diff", json!({ "file": "src/a.rs" })).await);
    assert_eq!(diff["uncommitted"], json!(true));
    assert_eq!(diff["files"][0]["status"], json!("added"));
    assert!(diff["diff"].as_str().unwrap().contains("+++ b/src/a.rs"));
    let stat = json_of(&call(&client, "get_space_diff", json!({ "stat": true })).await);
    assert_eq!(stat["diff"], Value::Null);
    assert_eq!(stat["additions"], json!(2));

    let items = json_of(&call(&client, "get_review_comments", json!({})).await);
    assert_eq!(items, json!({ "items": [] }));
    let added = json_of(
        &call(
            &client,
            "add_review_comment",
            json!({ "file": "src/a.rs", "line": 3, "body": "why?", "severity": "high" }),
        )
        .await,
    );
    assert_eq!(added["finding_id"], json!("f1"));
    let base = json_of(&call(&client, "set_base", json!({ "branch": "release" })).await);
    assert_eq!(base["base"], json!("release"));
    let progress = json_of(
        &call(
            &client,
            "update_progress",
            json!({ "steps": [
                { "label": "Read", "state": "done" },
                { "label": "Fix", "state": "active" }
            ], "note": "half" }),
        )
        .await,
    );
    assert_eq!(progress, json!({ "ok": true }));

    assert_eq!(
        spaces.calls(),
        [
            "ship s1 space:s1:m1 OpenDraftPr Some(\"Fix login\")",
            "ship s1 space:s1:m1 Push None",
            "space_diff s1 Some(\"src/a.rs\")",
            "space_diff s1 None",
            "review_comments s1 None",
            "add_review_comment s1 codex src/a.rs:3 High",
            "set_base s1 space:s1:m1 release",
            "update_progress s1 space:s1:m1 2 Some(\"half\")",
        ]
    );
    drop(client);
    handle.shutdown().await;
}

#[tokio::test]
async fn a_big_diff_is_cut_and_says_so() {
    let spaces = Arc::new(FakeChange {
        diff_lines: MAX_DIFF_CHARS / 40 + 100,
        ..Default::default()
    });
    let handle = McpServer::start_with_spaces(Arc::new(EmptyReview), spaces)
        .await
        .unwrap();
    let endpoint = handle.register_run(space_run());
    let client = connect(&endpoint.url, &endpoint.token).await;
    let diff = json_of(&call(&client, "get_space_diff", json!({})).await);
    assert_eq!(diff["truncated"], json!(true));
    assert_eq!(
        diff["diff"].as_str().unwrap().chars().count(),
        MAX_DIFF_CHARS
    );
    drop(client);
    handle.shutdown().await;
}

#[tokio::test]
async fn a_review_on_a_space_pr_never_gets_the_change_tools() {
    let spaces = Arc::new(FakeChange::default());
    let handle = McpServer::start_with_spaces(Arc::new(EmptyReview), spaces.clone())
        .await
        .unwrap();
    let mut run = RunContext::review(RunId::new("r1"), PrId::new("local:p#1"), AgentId::Claude);
    run.space = Some("s1".into());
    let endpoint = handle.register_run(run);
    let client = connect(&endpoint.url, &endpoint.token).await;
    let names: Vec<String> = client
        .list_all_tools()
        .await
        .unwrap()
        .iter()
        .map(|t| t.name.to_string())
        .collect();
    for tool in [
        "ship",
        "get_space_diff",
        "get_review_comments",
        "add_review_comment",
        "set_base",
        "update_progress",
    ] {
        assert!(!names.iter().any(|n| n == tool), "{tool} listed");
        let r = client
            .call_tool(
                CallToolRequestParams::new(tool.to_string())
                    .with_arguments(json!({ "action": "open_pr" }).as_object().unwrap().clone()),
            )
            .await;
        assert!(r.is_err(), "{tool} is refused");
    }
    assert!(spaces.calls().is_empty());
    drop(client);
    handle.shutdown().await;
}
