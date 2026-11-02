//! Agent port (plan §8): runs a local agent CLI and streams normalized events.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use theodore_domain::{AgentId, Usage};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub id: AgentId,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub installed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentMode {
    /// read / grep / MCP only
    ReadOnly,
    /// may edit files inside `cwd`
    Edit,
    /// may edit files and run any shell command without asking (a space's
    /// Run mode, docs/spaces.md §5). Theodore adds no sandbox of its own:
    /// the CLI's own configuration is what limits it.
    Run,
}

/// The MCP server name a PR run's tools go under (`mcp__review-tools__*`).
pub const REVIEW_TOOLS: &str = "review-tools";
/// The MCP server name a space turn's tools go under (`mcp__space-tools__*`).
pub const SPACE_TOOLS: &str = "space-tools";

fn review_tools() -> String {
    REVIEW_TOOLS.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpEndpoint {
    pub url: String,
    pub token: String,
    /// The server name the agent CLI registers it under: [`REVIEW_TOOLS`]
    /// or [`SPACE_TOOLS`]. Decides the tool names the agent sees.
    #[serde(default = "review_tools")]
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRequest {
    pub agent: AgentId,
    pub prompt: String,
    pub system: Option<String>,
    pub cwd: PathBuf,
    pub mode: AgentMode,
    pub mcp: Option<McpEndpoint>,
    pub model: Option<String>,
    /// How hard the agent thinks (`low` … `max`). Claude Code takes it as
    /// `--effort`, Codex as `-c model_reasoning_effort=…`; Gemini ignores it.
    #[serde(default)]
    pub effort: Option<String>,
    /// No output for this long → `AgentEvent::Stalled`.
    pub stall_after_ms: u64,
    /// Added to the child's environment after Theodore's own `THEODORE_*`
    /// secrets are stripped from the inherited one: a space's `env` and
    /// variables (docs/config.md).
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// The CLI may also load the user's own MCP servers (Claude Code's
    /// `~/.claude.json`, the repo's `.mcp.json`, claude.ai connectors) next
    /// to `mcp`: the `agents.user_mcp` setting. `false` = only `mcp`.
    #[serde(default)]
    pub user_mcp: bool,
    /// Files sent with this turn (chat attachments). The prompt already
    /// lists their paths; each runner adds what its CLI needs to open them
    /// (Claude Code: `--add-dir`, Codex: `-i` for images).
    #[serde(default)]
    pub attachments: Vec<AgentAttachment>,
    /// Continue this CLI session — an id an earlier run reported as
    /// [`AgentEvent::Session`] — instead of starting a new one, so the agent
    /// keeps what it read, ran and planned. `prompt` is then only the new
    /// turn. Claude Code: `--resume`; Codex: `exec resume`; Gemini has no
    /// resume and ignores it.
    #[serde(default)]
    pub resume: Option<String>,
    /// Also report the answer's text as it is generated
    /// ([`AgentEvent::TextDelta`]), not only per finished message. Claude
    /// Code: `--include-partial-messages`; a CLI that can't ignores it. On
    /// for every run the window shows live (chat, skills, space turns,
    /// fixes) and for grouping runs that stream groups.
    #[serde(default)]
    pub stream_text: bool,
}

/// One file the agent should read, outside `cwd`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAttachment {
    pub path: PathBuf,
    pub mime: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Text {
        text: String,
    },
    /// A piece of the answer's text as it is generated (only with
    /// [`AgentRequest::stream_text`]). The finished message still comes as
    /// `Text`: deltas are extra, never a replacement.
    TextDelta {
        text: String,
    },
    Thinking {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        id: String,
        ok: bool,
        output: String,
    },
    Usage {
        usage: Usage,
    },
    Stalled,
    /// The CLI's session id for this run (Claude Code's `session_id`,
    /// Codex's `thread_id`): what [`AgentRequest::resume`] takes to continue
    /// it. May come more than once; the last one counts.
    Session {
        id: String,
    },
    Done {
        final_text: String,
        usage: Usage,
    },
    Error {
        message: String,
        stderr_tail: String,
    },
}

/// The `error.code` of a run that failed because the agent's own servers
/// couldn't be reached (docs/telemetry.md): an expected state, not a crash.
pub const AGENT_OFFLINE_CODE: &str = "agent.offline";

/// What a CLI says when it can't reach its API: Node's (Claude Code,
/// Gemini) socket errors, Claude Code's own wording, and Codex's (Rust
/// `reqwest`/`hyper`). Matched case-insensitively against the error
/// message and stderr, never against the agent's own text.
const CONNECTION_FAILURE_MARKERS: &[&str] = &[
    "enotfound",
    "getaddrinfo",
    "econnrefused",
    "econnreset",
    "etimedout",
    "enetunreach",
    "ehostunreach",
    "connection error",
    "unable to connect to api",
    "fetch failed",
    "network error",
    "network is unreachable",
    "error sending request",
    "stream disconnected",
    "dns error",
    "failed to lookup address",
    "tcp connect error",
    "connection refused",
];

/// Whether an agent CLI's error output says it couldn't reach its servers.
pub fn is_connection_failure(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    CONNECTION_FAILURE_MARKERS.iter().any(|m| text.contains(m))
}

const OFFLINE_SUFFIX: &str = "'s servers — check your connection";

/// The message a run ends with when the agent couldn't reach its servers.
/// `agent` is the CLI name (`claude`, `codex`, `gemini`).
pub fn offline_message(agent: &str) -> String {
    let mut chars = agent.chars();
    let label: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "the agent".into(),
    };
    format!("Couldn't reach {label}{OFFLINE_SUFFIX}")
}

/// The `offline_message` inside `text`, if there is one: how a caller that
/// only gets the run's error text (grouping) tells offline from a crash.
pub fn offline_message_in(text: &str) -> Option<&str> {
    let start = text.find("Couldn't reach ")?;
    let end = start + text[start..].find(OFFLINE_SUFFIX)? + OFFLINE_SUFFIX.len();
    Some(&text[start..end])
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("{0:?} CLI not installed")]
    NotInstalled(AgentId),
    #[error("spawn failed: {0}")]
    Spawn(String),
}

impl AgentError {
    /// The stable error code logged as `error.code` (docs/telemetry.md).
    pub fn code(&self) -> &'static str {
        match self {
            AgentError::NotInstalled(_) => "agent.not_installed",
            AgentError::Spawn(_) => "agent.spawn_failed",
        }
    }
}

/// A running agent. Events end with exactly one `Done` or `Error`.
pub struct AgentRun {
    pub events: mpsc::Receiver<AgentEvent>,
    pub cancel: CancellationToken,
}

/// Sends more of your messages into a run while it works ("Send now",
/// docs/spaces.md §8): the CLI takes each at its next step, in the same
/// turn. Only CLIs that read messages while running give one
/// ([`AgentPort::run_steerable`]); a closed run refuses.
#[derive(Debug, Clone)]
pub struct Steer(mpsc::Sender<SteerMessage>);

/// One message for a running agent, and who to tell once the CLI has it.
#[derive(Debug)]
pub struct SteerMessage {
    pub text: String,
    pub delivered: tokio::sync::oneshot::Sender<()>,
}

impl Steer {
    /// The handle, and the end the runner reads.
    pub fn channel() -> (Steer, mpsc::Receiver<SteerMessage>) {
        let (tx, rx) = mpsc::channel(16);
        (Steer(tx), rx)
    }

    /// `true` once the message is written to the CLI; `false` when the run
    /// already finished (or is finishing) and can't take it.
    pub async fn send(&self, text: String) -> bool {
        let (delivered, ack) = tokio::sync::oneshot::channel();
        if self.0.send(SteerMessage { text, delivered }).await.is_err() {
            return false;
        }
        ack.await.is_ok()
    }
}

/// One model an agent CLI can run, and the effort levels it takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// What the CLI's model flag takes (`opus`, `gpt-5.5`).
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    /// Effort levels the model takes, in the CLI's order; empty = none.
    pub efforts: Vec<String>,
    /// The effort the CLI uses when none is passed, when it says.
    pub default_effort: Option<String>,
    /// An earlier version the CLI still runs: listed under "More models".
    #[serde(default)]
    pub older: bool,
}

#[async_trait]
pub trait AgentPort: Send + Sync {
    async fn detect(&self) -> Vec<AgentInfo>;
    async fn run(&self, req: AgentRequest) -> Result<AgentRun, AgentError>;
    /// [`run`](Self::run), plus a [`Steer`] when this agent's CLI takes
    /// messages while it runs (Claude Code). `None`: the caller queues
    /// them for the next run instead (Codex, Gemini).
    async fn run_steerable(
        &self,
        req: AgentRequest,
    ) -> Result<(AgentRun, Option<Steer>), AgentError> {
        self.run(req).await.map(|run| (run, None))
    }
    /// The models `agent` can run, for the model picker. Never fails: an
    /// agent that can't list them (not installed, no listing) gives `[]`.
    async fn models(&self, _agent: AgentId) -> Vec<ModelInfo> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_failures_are_told_from_other_errors() {
        for offline in [
            "Error: getaddrinfo ENOTFOUND api.anthropic.com",
            "connect ECONNREFUSED 127.0.0.1:443",
            "API Error: Connection error.",
            "Unable to connect to API due to poor internet connection",
            "TypeError: fetch failed",
            "stream disconnected before completion: error sending request for url (https://api.openai.com/v1/responses)",
            "dns error: failed to lookup address information: nodename nor servname provided",
        ] {
            assert!(is_connection_failure(offline), "{offline}");
        }
        for other in [
            "claude exited with a non-zero status",
            "unexpected status 401 Unauthorized",
            "No conversation found with session ID: 0000",
        ] {
            assert!(!is_connection_failure(other), "{other}");
        }
    }

    #[test]
    fn the_offline_message_names_the_agent_and_is_found_again() {
        let m = offline_message("claude");
        assert_eq!(m, "Couldn't reach Claude's servers — check your connection");
        let wrapped = format!("agent run failed: {m}: getaddrinfo ENOTFOUND");
        assert_eq!(offline_message_in(&wrapped), Some(m.as_str()));
        assert_eq!(offline_message_in("agent run failed: boom"), None);
    }
}
