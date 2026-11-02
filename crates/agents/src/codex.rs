//! Codex runner (`codex` 0.15x): `codex exec --json`.
//!
//! Flags verified against `codex exec --help` / `codex mcp --help` on the
//! installed 0.156.1 binary. HTTP MCP needs no stdio bridge: `codex exec`
//! accepts `-c mcp_servers.<name>.url=...` / `.bearer_token_env_var=...`
//! overrides without touching `~/.codex/config.toml` (confirmed by
//! `codex mcp add --help`, which documents the same keys for the persisted
//! form).
//!
//! The event schema below (`thread.started`, `turn.started/completed/failed`,
//! `item.started/completed` with `agent_message` / `reasoning` /
//! `command_execution` / `mcp_tool_call` / `file_change` / `web_search`
//! items) matches OpenAI's published non-interactive-mode reference. Only the
//! `error`/`turn.failed` path was exercised live here — this sandbox's Codex
//! has no OpenAI auth, so a real `turn.completed` was never observed; see
//! `tests/fixtures/codex_auth_error.jsonl` (real, redacted) vs
//! `tests/fixtures/codex_success.jsonl` (built from the documented schema,
//! marked untested).
//!
//! No text deltas: `AgentRequest::stream_text` is ignored and an answer
//! arrives whole, as `item.completed(agent_message)`. `codex exec --json`
//! (0.164.0, `exec/src/event_processor_with_jsonl_output.rs`) drops the
//! app-server's `AgentMessageDelta` notifications, sends `item.updated`
//! only for `todo_list`, and has no flag or feature to turn deltas on.
//! Only the experimental `codex app-server` protocol streams them
//! (`item/agentMessage/delta`) — a different runner, not a flag.

use serde_json::Value;
use theodore_domain::Usage;
use theodore_ports::agent::{AgentEvent, AgentMode, AgentRequest};

use crate::config::AgentBinaryConfig;
use crate::process::LineParser;

/// `codex exec [flags] <prompt>`, or with `req.resume`
/// `codex exec [flags] resume [-i …] <thread id> <prompt>`: `-C`, `-s`, `-c`
/// and `-m` are `exec`'s own and go before `resume` (0.156.1 resumes a
/// thread with them there; `resume`'s own `-i` takes one value).
pub fn build_args(req: &AgentRequest, cfg: &AgentBinaryConfig) -> Vec<String> {
    let mut args = vec!["exec".to_string(), "--json".to_string()];
    // Images go to the model with the prompt; other attachments are only
    // listed in it (the sandbox reads outside the workspace). Before `-C`:
    // `-i` takes several values, so a flag must follow it, never the prompt.
    // It also splits on `,`, so a path with one is left to the prompt.
    let images: Vec<String> = req
        .attachments
        .iter()
        .map(|a| (a, a.path.display().to_string()))
        .filter(|(a, path)| a.mime.starts_with("image/") && !path.contains(','))
        .map(|(_, path)| path)
        .collect();
    if req.resume.is_none() {
        for path in &images {
            args.push("-i".to_string());
            args.push(path.clone());
        }
    }
    args.push("-C".to_string());
    args.push(req.cwd.display().to_string());
    args.push("-s".to_string());
    args.push(
        match req.mode {
            AgentMode::ReadOnly => "read-only",
            AgentMode::Edit | AgentMode::Run => "workspace-write",
        }
        .to_string(),
    );
    if req.mode == AgentMode::Run {
        // Writes stay inside the workspace; commands may reach the network
        // (installs). Codex's own sandbox, nothing added by Theodore.
        args.push("-c".to_string());
        args.push("sandbox_workspace_write.network_access=true".to_string());
    }
    if let Some(mcp) = &req.mcp {
        args.push("-c".to_string());
        args.push(format!("mcp_servers.{}.url=\"{}\"", mcp.name, mcp.url));
        args.push("-c".to_string());
        args.push(format!(
            "mcp_servers.{}.bearer_token_env_var=\"{MCP_TOKEN_ENV_VAR}\"",
            mcp.name
        ));
    }
    if let Some(model) = req.model.as_ref().or(cfg.model.as_ref()) {
        args.push("-m".to_string());
        args.push(model.clone());
    }
    match req.effort.as_deref() {
        // Levels differ per model (`codex debug models`), so no fixed list;
        // only the shape is checked, as it goes inside a TOML string.
        Some(effort) if is_effort_token(effort) => {
            args.push("-c".to_string());
            args.push(format!("model_reasoning_effort=\"{effort}\""));
        }
        Some(effort) => tracing::warn!(effort, "malformed codex effort level, left out"),
        None => {}
    }
    args.extend(cfg.extra_args.iter().cloned());
    if let Some(thread) = &req.resume {
        // The thread already opens with the system prompt (folded into its
        // first turn, below): only the new turn goes in.
        args.push("resume".to_string());
        for path in images {
            args.push("-i".to_string());
            args.push(path);
        }
        args.push(thread.clone());
        args.push(req.prompt.clone());
        return args;
    }
    // Codex exec has no --append-system-prompt; fold it into the prompt text.
    let prompt = match &req.system {
        Some(system) => format!("{system}\n\n{}", req.prompt),
        None => req.prompt.clone(),
    };
    args.push(prompt);
    args
}

/// `low`, `xhigh`, …: a bare word, safe inside `"…"` in a `-c` override.
fn is_effort_token(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Env var Codex reads the MCP bearer token from, per `bearer_token_env_var`
/// above. Named without a `THEODORE_` prefix so the generic env filter in
/// `env.rs` (which strips Theodore's own secrets) never touches it; the
/// caller sets it explicitly alongside the filtered parent env.
pub const MCP_TOKEN_ENV_VAR: &str = "REVIEW_TOOLS_TOKEN";

#[derive(Debug, Default)]
pub struct CodexParser {
    last_agent_message: String,
}

impl CodexParser {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Pure: one `--json` line in, zero or more normalized events out, plus the
/// running `last_agent_message` an eventual `turn.completed` needs for `Done`.
pub fn parse_line(line: &str, last_agent_message: &mut String) -> Vec<AgentEvent> {
    let line = line.trim();
    if line.is_empty() {
        return vec![];
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return vec![];
    };
    match v.get("type").and_then(Value::as_str) {
        Some("item.started") => item_started(v.get("item")),
        Some("item.completed") => item_completed(v.get("item"), last_agent_message),
        Some("turn.completed") => {
            vec![AgentEvent::Done {
                final_text: last_agent_message.clone(),
                usage: v.get("usage").map(codex_usage).unwrap_or_default(),
            }]
        }
        Some("turn.failed") => {
            let message = v
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("codex turn failed")
                .to_string();
            vec![AgentEvent::Error {
                message,
                stderr_tail: String::new(),
            }]
        }
        // A new thread or a resumed one: its id is what `exec resume` takes.
        Some("thread.started") => v
            .get("thread_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(|id| AgentEvent::Session { id: id.to_string() })
            .into_iter()
            .collect(),
        // Top-level `error` events are transport-level retries (seen live:
        // repeated reconnect attempts before the real `turn.failed`); never
        // terminal on their own.
        Some("turn.started") | Some("error") => vec![],
        _ => vec![],
    }
}

fn item_started(item: Option<&Value>) -> Vec<AgentEvent> {
    let Some(item) = item else { return vec![] };
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    match item.get("type").and_then(Value::as_str) {
        Some("command_execution") => {
            let command = item
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default();
            vec![AgentEvent::ToolCall {
                id,
                name: "shell".to_string(),
                input: serde_json::json!({ "command": command }),
            }]
        }
        Some("mcp_tool_call") => {
            let server = item
                .get("server")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let tool = item.get("tool").and_then(Value::as_str).unwrap_or_default();
            vec![AgentEvent::ToolCall {
                id,
                name: format!("{server}__{tool}"),
                input: item.get("arguments").cloned().unwrap_or(Value::Null),
            }]
        }
        _ => vec![],
    }
}

fn item_completed(item: Option<&Value>, last_agent_message: &mut String) -> Vec<AgentEvent> {
    let Some(item) = item else { return vec![] };
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    match item.get("type").and_then(Value::as_str) {
        Some("agent_message") => {
            let text = item
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            last_agent_message.clone_from(&text);
            if text.is_empty() {
                vec![]
            } else {
                vec![AgentEvent::Text { text }]
            }
        }
        Some("reasoning") => {
            let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
            (!text.is_empty())
                .then(|| AgentEvent::Thinking {
                    text: text.to_string(),
                })
                .into_iter()
                .collect()
        }
        Some("command_execution") => {
            let exit_code = item.get("exit_code").and_then(Value::as_i64);
            let output = item
                .get("aggregated_output")
                .and_then(Value::as_str)
                .unwrap_or_default();
            vec![AgentEvent::ToolResult {
                id,
                ok: exit_code == Some(0),
                output: output.to_string(),
            }]
        }
        Some("mcp_tool_call") => {
            let ok = item.get("status").and_then(Value::as_str) == Some("completed")
                && item.get("error").is_none_or(Value::is_null);
            let output = if let Some(err) = item.pointer("/error/message").and_then(Value::as_str) {
                err.to_string()
            } else {
                item.get("result")
                    .map(|r| r.to_string())
                    .unwrap_or_default()
            };
            vec![AgentEvent::ToolResult { id, ok, output }]
        }
        Some("file_change") => {
            let ok = item.get("status").and_then(Value::as_str) == Some("completed");
            let changes = item.get("changes").cloned().unwrap_or(Value::Null);
            vec![AgentEvent::ToolResult {
                id,
                ok,
                output: changes.to_string(),
            }]
        }
        Some("web_search") => {
            let query = item
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or_default();
            vec![
                AgentEvent::ToolCall {
                    id: id.clone(),
                    name: "web_search".to_string(),
                    input: serde_json::json!({ "query": query }),
                },
                AgentEvent::ToolResult {
                    id,
                    ok: true,
                    output: String::new(),
                },
            ]
        }
        // item-level `error`: non-fatal warning, no AgentEvent vocabulary for it.
        _ => vec![],
    }
}

fn codex_usage(u: &Value) -> Usage {
    let field = |name: &str| u.get(name).and_then(Value::as_u64).unwrap_or(0);
    Usage {
        input_tokens: field("input_tokens") + field("cached_input_tokens"),
        output_tokens: field("output_tokens"),
        usd: 0.0, // Codex's `--json` usage carries no cost field
    }
}

impl LineParser for CodexParser {
    fn on_line(&mut self, line: &str) -> Vec<AgentEvent> {
        parse_line(line, &mut self.last_agent_message)
    }

    fn on_exit(&mut self, success: bool, stderr_tail: &str) -> AgentEvent {
        AgentEvent::Error {
            message: if success {
                "codex exited without a turn.completed event".to_string()
            } else {
                "codex exited with a non-zero status".to_string()
            },
            stderr_tail: stderr_tail.to_string(),
        }
    }

    /// Top-level `error` events: Codex's reconnect attempts.
    fn is_transport_notice(&self, line: &str) -> bool {
        line.len() < 16 * 1024
            && serde_json::from_str::<Value>(line)
                .is_ok_and(|v| v.get("type").and_then(Value::as_str) == Some("error"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(s: &str) -> Vec<AgentEvent> {
        let mut last = String::new();
        parse_line(s, &mut last)
    }

    #[test]
    fn thread_started_reports_the_session_and_turn_started_is_ignored() {
        assert_eq!(
            line(r#"{"type":"thread.started","thread_id":"abc"}"#),
            vec![AgentEvent::Session { id: "abc".into() }]
        );
        assert_eq!(line(r#"{"type":"thread.started"}"#), vec![]);
        assert_eq!(line(r#"{"type":"turn.started"}"#), vec![]);
    }

    #[test]
    fn reconnect_errors_are_transport_notices() {
        let p = CodexParser::new();
        assert!(p.is_transport_notice(
            r#"{"type":"error","message":"Reconnecting... 1/5 (stream disconnected before completion: error sending request)"}"#
        ));
        assert!(!p.is_transport_notice(r#"{"type":"turn.started"}"#));
        assert!(!p.is_transport_notice("not json"));
    }

    #[test]
    fn transport_error_is_not_terminal() {
        assert_eq!(
            line(r#"{"type":"error","message":"Reconnecting... 1/5"}"#),
            vec![]
        );
    }

    #[test]
    fn command_execution_started_then_completed() {
        let started = r#"{"type":"item.started","item":{"id":"c1","type":"command_execution","command":"ls","status":"in_progress"}}"#;
        assert_eq!(
            line(started),
            vec![AgentEvent::ToolCall {
                id: "c1".into(),
                name: "shell".into(),
                input: serde_json::json!({"command": "ls"}),
            }]
        );
        let completed = r#"{"type":"item.completed","item":{"id":"c1","type":"command_execution","command":"ls","aggregated_output":"a.rs\n","exit_code":0,"status":"completed"}}"#;
        assert_eq!(
            line(completed),
            vec![AgentEvent::ToolResult {
                id: "c1".into(),
                ok: true,
                output: "a.rs\n".into()
            }]
        );
    }

    #[test]
    fn agent_message_completed_becomes_text_and_tracks_last() {
        let mut last = String::new();
        let events = parse_line(
            r#"{"type":"item.completed","item":{"id":"m1","type":"agent_message","text":"ok"}}"#,
            &mut last,
        );
        assert_eq!(events, vec![AgentEvent::Text { text: "ok".into() }]);
        assert_eq!(last, "ok");
    }

    #[test]
    fn turn_completed_uses_last_agent_message_and_usage() {
        let mut last = "ok".to_string();
        let events = parse_line(
            r#"{"type":"turn.completed","usage":{"input_tokens":10,"cached_input_tokens":5,"output_tokens":3}}"#,
            &mut last,
        );
        assert_eq!(
            events,
            vec![AgentEvent::Done {
                final_text: "ok".into(),
                usage: Usage {
                    input_tokens: 15,
                    output_tokens: 3,
                    usd: 0.0
                },
            }]
        );
    }

    #[test]
    fn turn_failed_becomes_error() {
        assert_eq!(
            line(
                r#"{"type":"turn.failed","error":{"message":"unexpected status 401 Unauthorized"}}"#
            ),
            vec![AgentEvent::Error {
                message: "unexpected status 401 Unauthorized".into(),
                stderr_tail: String::new(),
            }]
        );
    }

    #[test]
    fn real_auth_failure_fixture_ends_in_exactly_one_terminal_event() {
        let fixture = include_str!("../tests/fixtures/codex_auth_error.jsonl");
        let mut last = String::new();
        let mut terminals = 0;
        for l in fixture.lines().filter(|l| !l.trim().is_empty()) {
            for ev in parse_line(l, &mut last) {
                if matches!(ev, AgentEvent::Done { .. } | AgentEvent::Error { .. }) {
                    terminals += 1;
                }
            }
        }
        assert_eq!(terminals, 1);
    }
}

#[cfg(test)]
mod run_mode_tests {
    use super::*;

    #[test]
    fn run_mode_may_edit_and_run_commands() {
        let req = AgentRequest {
            agent: theodore_domain::AgentId::Codex,
            prompt: "p".into(),
            system: None,
            cwd: "/tmp".into(),
            mode: AgentMode::Run,
            mcp: None,
            model: None,
            effort: None,
            stall_after_ms: 1000,
            env: Vec::new(),
            user_mcp: false,
            attachments: vec![],
            resume: None,
            stream_text: false,
        };
        let args = build_args(&req, &Default::default());
        assert!(args.windows(2).any(|w| w == ["-s", "workspace-write"]));
        assert!(
            args.iter()
                .any(|a| a == "sandbox_workspace_write.network_access=true")
        );
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;

    fn req(model: Option<&str>, effort: Option<&str>) -> AgentRequest {
        AgentRequest {
            agent: theodore_domain::AgentId::Codex,
            prompt: "p".into(),
            system: None,
            cwd: "/tmp".into(),
            mode: AgentMode::ReadOnly,
            mcp: None,
            model: model.map(str::to_string),
            effort: effort.map(str::to_string),
            stall_after_ms: 1000,
            env: Vec::new(),
            user_mcp: false,
            attachments: vec![],
            resume: None,
            stream_text: false,
        }
    }

    #[test]
    fn model_goes_to_dash_m_and_effort_to_model_reasoning_effort() {
        let args = build_args(&req(Some("some-model"), Some("high")), &Default::default());
        assert!(
            args.windows(2).any(|w| w == ["-m", "some-model"]),
            "{args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w == ["-c", "model_reasoning_effort=\"high\""]),
            "{args:?}"
        );
        // Flags come before the prompt, which stays last.
        assert_eq!(args.last().map(String::as_str), Some("p"));
    }

    /// Nothing in `exec` streams answer text, so asking for it changes
    /// nothing.
    #[test]
    fn stream_text_changes_nothing() {
        let mut streamed = req(None, None);
        streamed.stream_text = true;
        let cfg = Default::default();
        assert_eq!(
            build_args(&streamed, &cfg),
            build_args(&req(None, None), &cfg)
        );
    }

    #[test]
    fn no_effort_no_override() {
        let args = build_args(&req(None, None), &Default::default());
        assert!(!args.iter().any(|a| a.contains("effort")), "{args:?}");
        assert!(!args.iter().any(|a| a == "-m"), "{args:?}");
    }

    #[test]
    fn a_malformed_effort_is_left_out() {
        for bad in ["", "high\" -c x=\"y", "a b"] {
            let args = build_args(&req(None, Some(bad)), &Default::default());
            assert!(
                !args.iter().any(|a| a.contains("effort")),
                "{bad}: {args:?}"
            );
        }
    }

    /// `exec resume <thread> <prompt>` after `exec`'s own flags; images on
    /// `resume`, and no system prompt: the thread already opens with it.
    #[test]
    fn a_resumed_turn_continues_the_thread_with_only_the_new_prompt() {
        let mut r = req(Some("some-model"), Some("high"));
        r.mode = AgentMode::Run;
        r.system = Some("SYSTEM".into());
        r.resume = Some("thread-1".into());
        r.attachments = vec![theodore_ports::AgentAttachment {
            path: "/data/attachments/a/shot.png".into(),
            mime: "image/png".into(),
        }];
        let args = build_args(&r, &Default::default());
        let at = args.iter().position(|a| a == "resume").expect("resume");
        let (exec, resume) = args.split_at(at);
        for flag in ["-C", "-s", "-m", "-c"] {
            assert!(
                exec.iter().any(|a| a == flag),
                "{flag} before resume: {args:?}"
            );
        }
        assert!(!exec.iter().any(|a| a == "-i"), "{args:?}");
        assert_eq!(
            resume,
            [
                "resume",
                "-i",
                "/data/attachments/a/shot.png",
                "thread-1",
                "p"
            ]
        );
        assert!(!args.iter().any(|a| a.contains("SYSTEM")), "{args:?}");
    }

    #[test]
    fn a_new_turn_folds_the_system_prompt_in() {
        let mut r = req(None, None);
        r.system = Some("SYSTEM".into());
        let args = build_args(&r, &Default::default());
        assert!(!args.iter().any(|a| a == "resume"));
        assert_eq!(args.last().map(String::as_str), Some("SYSTEM\n\np"));
    }

    #[test]
    fn image_attachments_go_to_dash_i_before_the_other_flags() {
        let mut r = req(None, None);
        r.attachments = vec![
            theodore_ports::AgentAttachment {
                path: "/data/attachments/a/shot.png".into(),
                mime: "image/png".into(),
            },
            theodore_ports::AgentAttachment {
                path: "/data/attachments/b/spec.pdf".into(),
                mime: "application/pdf".into(),
            },
            theodore_ports::AgentAttachment {
                path: "/data/attachments/c/a,b.png".into(),
                mime: "image/png".into(),
            },
        ];
        let args = build_args(&r, &Default::default());
        let images: Vec<&str> = args
            .windows(2)
            .filter(|w| w[0] == "-i")
            .map(|w| w[1].as_str())
            .collect();
        assert_eq!(images, ["/data/attachments/a/shot.png"]);
        // `-i` is variadic: what follows it must be a flag, not the prompt.
        let at = args.iter().position(|a| a == "-i").unwrap();
        assert_eq!(args[at + 2], "-C");
        assert_eq!(args.last().map(String::as_str), Some("p"));
    }
}
