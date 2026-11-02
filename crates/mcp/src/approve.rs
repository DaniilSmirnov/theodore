//! `approve_tool`: Claude Code's permission prompts, answered (issue #83).
//!
//! A run with the user's MCP servers on can't pre-approve them by name:
//! claude.ai connectors (`mcp__claude_ai_Linear__…`) are only named once
//! the CLI loads them, and `--allowedTools mcp__*` matches no server. So
//! the CLI asks this tool (`--permission-prompt-tool`) about every call
//! nothing pre-approved: an MCP tool goes through, anything else (Bash,
//! Edit, Write…) is denied, as before, so the run's mode still decides.
//! Theodore's own tools are pre-approved and never asked about.

use rmcp::ErrorData;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::server::ReviewToolsServer;

/// What Claude Code sends a `--permission-prompt-tool` (2.1.312).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApproveToolParams {
    /// The tool the agent wants to call, e.g. `mcp__claude_ai_Linear__get_issue`.
    pub tool_name: String,
    /// Its arguments; passed back unchanged when allowed.
    #[serde(default)]
    pub input: Value,
    pub tool_use_id: Option<String>,
}

/// The answer, as Claude Code reads it: `{"behavior":"allow","updatedInput":…}`
/// or `{"behavior":"deny","message":…}`.
pub fn decide(tool_name: &str, input: &Value) -> Value {
    if tool_name.starts_with("mcp__") {
        serde_json::json!({ "behavior": "allow", "updatedInput": input })
    } else {
        serde_json::json!({
            "behavior": "deny",
            "message": format!("{tool_name} is not allowed in this run's mode."),
        })
    }
}

#[tool_router(router = approve_router, vis = "pub(crate)")]
impl ReviewToolsServer {
    #[tool(
        description = "Internal: Claude Code calls this to ask whether a tool call may run. Never call it yourself."
    )]
    async fn approve_tool(
        &self,
        Parameters(params): Parameters<ApproveToolParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let answer = decide(&params.tool_name, &params.input);
        Ok(CallToolResult::success(vec![ContentBlock::text(
            answer.to_string(),
        )]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_mcp_tool_is_allowed_with_its_input_unchanged() {
        let input = serde_json::json!({ "id": "ENG-1" });
        assert_eq!(
            decide("mcp__claude_ai_Linear__get_issue", &input),
            serde_json::json!({ "behavior": "allow", "updatedInput": { "id": "ENG-1" } })
        );
    }

    #[test]
    fn built_in_tools_stay_denied() {
        for tool in [
            "Bash",
            "Edit",
            "Write",
            "MultiEdit",
            "NotebookEdit",
            "mcp_x",
        ] {
            let answer = decide(tool, &serde_json::json!({}));
            assert_eq!(answer["behavior"], "deny", "{tool}");
            assert!(answer["message"].as_str().unwrap().contains(tool));
        }
    }
}
