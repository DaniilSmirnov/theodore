//! A space's Plan mode per agent CLI, and the `space-tools` MCP timeout
//! that lets `ask_user` wait for you (docs/spaces.md §5).

use theodore_agents::{claude, codex, gemini};
use theodore_domain::AgentId;
use theodore_ports::agent::{AgentMode, AgentRequest, McpEndpoint};

fn req(agent: AgentId, mode: AgentMode, mcp: Option<&str>) -> AgentRequest {
    AgentRequest {
        agent,
        prompt: "p".into(),
        system: None,
        cwd: "/tmp".into(),
        mode,
        mcp: mcp.map(|name| McpEndpoint {
            url: "http://127.0.0.1:1/mcp".into(),
            token: "t".into(),
            name: name.into(),
        }),
        model: None,
        effort: None,
        stall_after_ms: 1000,
        env: Vec::new(),
        user_mcp: false,
        attachments: vec![],
        resume: None,
        stream_text: false,
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

/// Claude Code's own plan mode; shell and edits denied outright, `Write`
/// left to the mode (its plan file).
#[test]
fn claude_plans_in_its_plan_mode_without_shell_or_edits() {
    let args = claude::build_args(
        &req(AgentId::Claude, AgentMode::Plan, None),
        &Default::default(),
        None,
    );
    assert_eq!(flag(&args, "--permission-mode").as_deref(), Some("plan"));
    let denied = flag(&args, "--disallowedTools").expect("denylist");
    for tool in ["Bash", "Edit", "MultiEdit", "NotebookEdit"] {
        assert!(denied.split(',').any(|t| t == tool), "{tool}: {denied}");
    }
    assert!(!denied.split(',').any(|t| t == "Write"), "{denied}");
    let allowed = flag(&args, "--allowedTools").expect("allowlist");
    assert!(allowed.split(',').any(|t| t == "Read"), "{allowed}");
}

/// Codex and Gemini have no plan mode: they run read-only (the system
/// prompt asks for the plan).
#[test]
fn codex_and_gemini_plan_read_only() {
    let args = codex::build_args(
        &req(AgentId::Codex, AgentMode::Plan, None),
        &Default::default(),
    );
    assert_eq!(flag(&args, "-s").as_deref(), Some("read-only"));

    let r = req(AgentId::Gemini, AgentMode::Plan, None);
    let args = gemini::build_args(&r, &Default::default());
    assert_eq!(flag(&args, "--approval-mode").as_deref(), Some("default"));
    let plan = gemini::settings_json(&r);
    let read_only = gemini::settings_json(&req(AgentId::Gemini, AgentMode::ReadOnly, None));
    assert_eq!(plan["tools"], read_only["tools"]);
}

/// `ask_user` waits up to 30 min: Codex's MCP call timeout (60 s by
/// default) and Gemini's (10 min) go past it for space turns only.
#[test]
fn space_tools_calls_may_wait_for_the_user() {
    let space = codex::build_args(
        &req(
            AgentId::Codex,
            AgentMode::Edit,
            Some(theodore_ports::SPACE_TOOLS),
        ),
        &Default::default(),
    );
    let timeout = space
        .iter()
        .find(|a| a.starts_with("mcp_servers.space-tools.tool_timeout_sec="))
        .expect("codex tool timeout");
    let secs: u64 = timeout.rsplit('=').next().unwrap().parse().unwrap();
    assert!(
        secs * 1000 > theodore_ports::ASK_USER_TIMEOUT_MS,
        "{timeout}"
    );
    let review = codex::build_args(
        &req(
            AgentId::Codex,
            AgentMode::Edit,
            Some(theodore_ports::REVIEW_TOOLS),
        ),
        &Default::default(),
    );
    assert!(!review.iter().any(|a| a.contains("tool_timeout_sec")));

    let settings = gemini::settings_json(&req(
        AgentId::Gemini,
        AgentMode::Edit,
        Some(theodore_ports::SPACE_TOOLS),
    ));
    let ms = settings["mcpServers"]["space-tools"]["timeout"]
        .as_u64()
        .expect("gemini timeout");
    assert!(ms > theodore_ports::ASK_USER_TIMEOUT_MS);
    let settings = gemini::settings_json(&req(
        AgentId::Gemini,
        AgentMode::Edit,
        Some(theodore_ports::REVIEW_TOOLS),
    ));
    assert!(
        settings["mcpServers"]["review-tools"]
            .get("timeout")
            .is_none()
    );
}
