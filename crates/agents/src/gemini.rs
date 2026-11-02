//! Gemini CLI runner — **untested**: `gemini` is not installed in this
//! environment, so nothing here ran against a real binary. Built from the
//! CLI's public docs (`--output-format stream-json` shipped per
//! google-gemini/gemini-cli#8203; `-p`, `-m`, `--approval-mode`; MCP servers
//! are settings-file only, no `--mcp-config` flag). Flags need
//! re-verification against `gemini --help` and a real run before this runner
//! ships. The event schema (`init` / `message` / `tool_use` / `tool_result` /
//! `error` / `result`, field names included) is read off the v0.66.0 source
//! (`packages/core/src/output/types.ts`, `packages/cli/src/nonInteractiveCli.ts`)
//! and its stream-json snapshot test, not off a run.
//!
//! MCP and the allow/deny tool list are settings-file only; we write a throwaway
//! settings file and point `GEMINI_CLI_SYSTEM_SETTINGS_PATH` at it for just
//! this child process, rather than touching the user's or the repo's real
//! `.gemini/settings.json`.

use std::path::PathBuf;

use serde_json::Value;
use theodore_domain::Usage;
use theodore_ports::agent::{AgentEvent, AgentMode, AgentRequest};

use crate::config::AgentBinaryConfig;
use crate::process::LineParser;

pub const SETTINGS_PATH_ENV_VAR: &str = "GEMINI_CLI_SYSTEM_SETTINGS_PATH";

/// Core tool allowlist per mode (`tools.core` in settings.json). Names beyond
/// `ReadFileTool` / `GlobTool` / `ShellTool` are a best guess pending `--help`.
fn core_tools(mode: AgentMode) -> Vec<&'static str> {
    match mode {
        AgentMode::ReadOnly => vec!["ReadFileTool", "GlobTool", "SearchTextTool"],
        AgentMode::Edit | AgentMode::Run => {
            vec![
                "ReadFileTool",
                "GlobTool",
                "SearchTextTool",
                "WriteFileTool",
                "EditTool",
                "ShellTool",
            ]
        }
    }
}

/// Settings-file JSON for this run: tool allowlist + the `review-tools` MCP
/// server over HTTP, if requested.
pub fn settings_json(req: &AgentRequest) -> Value {
    let mut settings = serde_json::json!({ "tools": { "core": core_tools(req.mode) } });
    if let Some(mcp) = &req.mcp {
        settings["mcpServers"] = serde_json::json!({
            mcp.name.clone(): {
                "httpUrl": mcp.url,
                "headers": { "Authorization": format!("Bearer {}", mcp.token) },
            }
        });
    }
    settings
}

/// Write the settings file; the caller keeps the returned handle alive for
/// the lifetime of the child process (its path goes into `SpawnSpec::env`).
pub fn write_settings_file(req: &AgentRequest) -> std::io::Result<tempfile::NamedTempFile> {
    use std::io::Write;
    let mut file = tempfile::Builder::new()
        .prefix("theodore-gemini-settings-")
        .suffix(".json")
        .tempfile()?;
    file.write_all(settings_json(req).to_string().as_bytes())?;
    file.flush()?;
    Ok(file)
}

pub fn build_args(req: &AgentRequest, cfg: &AgentBinaryConfig) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        req.prompt.clone(),
        "--output-format".to_string(),
        "stream-json".to_string(),
    ];
    args.push("--approval-mode".to_string());
    args.push(
        match req.mode {
            AgentMode::ReadOnly => "default",
            AgentMode::Edit => "auto_edit",
            // Shell commands too, without asking.
            AgentMode::Run => "yolo",
        }
        .to_string(),
    );
    if let Some(model) = req.model.as_ref().or(cfg.model.as_ref()) {
        args.push("-m".to_string());
        args.push(model.clone());
    }
    // No documented --append-system-prompt; fold it into the prompt, as for Codex.
    if let Some(system) = &req.system {
        args[1] = format!("{system}\n\n{}", args[1]);
    }
    args.extend(cfg.extra_args.iter().cloned());
    args
}

/// Folds `stream-json` into normalized events. Gemini only ever streams an
/// answer: every assistant `message` is a `"delta": true` chunk
/// (`nonInteractiveCli.ts`), and nothing repeats it whole. So the chunks of
/// one message are collected here and the whole `Text` goes out at the
/// message's end — the next `tool_use`, or `result`:
///
/// ```text
/// message(delta) ─▶ [TextDelta]   (only with stream_text)
/// message(delta) ─▶ [TextDelta]
/// tool_use       ─▶ Text(whole) , ToolCall
/// message(delta) ─▶ [TextDelta]
/// result         ─▶ Text(whole) , Done(final_text = that last message)
/// ```
///
/// Same contract as Claude's `--include-partial-messages`: deltas first,
/// then the complete message. A `message` without `delta` is taken as
/// whole and goes out as `Text` at once.
#[derive(Debug, Default)]
pub struct GeminiParser {
    stream_text: bool,
    /// The assistant message being written: its chunks so far.
    pending: String,
    /// The last whole assistant message, for `Done` (stream-json's
    /// `result` carries no response text).
    last_text: String,
}

impl GeminiParser {
    pub fn new(stream_text: bool) -> Self {
        Self {
            stream_text,
            ..Self::default()
        }
    }

    /// One `stream-json` line in, zero or more normalized events out.
    pub fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let line = line.trim();
        if line.is_empty() {
            return vec![];
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return vec![];
        };
        match v.get("type").and_then(Value::as_str) {
            Some("message") => self.message(&v),
            Some("tool_use") => {
                let mut out: Vec<AgentEvent> = self.flush().into_iter().collect();
                out.push(AgentEvent::ToolCall {
                    id: str_field(&v, "tool_id"),
                    name: str_field(&v, "tool_name"),
                    input: v.get("parameters").cloned().unwrap_or(Value::Null),
                });
                out
            }
            Some("tool_result") => {
                let ok = v.get("status").and_then(Value::as_str) != Some("error");
                let output = match v.pointer("/error/message").and_then(Value::as_str) {
                    Some(err) if !ok => err.to_string(),
                    _ => str_field(&v, "output"),
                };
                vec![AgentEvent::ToolResult {
                    id: str_field(&v, "tool_id"),
                    ok,
                    output,
                }]
            }
            Some("result") => {
                let mut out: Vec<AgentEvent> = self.flush().into_iter().collect();
                out.push(result_event(&v, &self.last_text));
                out
            }
            // `init`, and `error` (`severity` warning or error: loop
            // detected, max turns, blocked): never terminal on their own —
            // a failed run still ends in `result` with `status: error`, or
            // in the process exiting.
            _ => vec![],
        }
    }

    fn message(&mut self, v: &Value) -> Vec<AgentEvent> {
        if v.get("role").and_then(Value::as_str) != Some("assistant") {
            return vec![];
        }
        let text = v.get("content").and_then(Value::as_str).unwrap_or_default();
        if text.is_empty() {
            return vec![];
        }
        if v.get("thought").and_then(Value::as_bool).unwrap_or(false) {
            return vec![AgentEvent::Thinking {
                text: text.to_string(),
            }];
        }
        if v.get("delta").and_then(Value::as_bool).unwrap_or(false) {
            self.pending.push_str(text);
            return if self.stream_text {
                vec![AgentEvent::TextDelta {
                    text: text.to_string(),
                }]
            } else {
                vec![]
            };
        }
        let mut out: Vec<AgentEvent> = self.flush().into_iter().collect();
        self.last_text = text.to_string();
        out.push(AgentEvent::Text {
            text: text.to_string(),
        });
        out
    }

    /// The message being written is over: it goes out whole.
    fn flush(&mut self) -> Option<AgentEvent> {
        if self.pending.is_empty() {
            return None;
        }
        self.last_text = std::mem::take(&mut self.pending);
        Some(AgentEvent::Text {
            text: self.last_text.clone(),
        })
    }
}

fn str_field(v: &Value, name: &str) -> String {
    v.get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `status: error` fails the run, with or without an `error` object (an
/// invalid model stream ends in a bare `status: error`).
fn result_event(v: &Value, last_text: &str) -> AgentEvent {
    let error = v.get("error").filter(|e| !e.is_null());
    if error.is_some() || v.get("status").and_then(Value::as_str) == Some("error") {
        let message = error
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("gemini reported an error");
        return AgentEvent::Error {
            message: message.to_string(),
            stderr_tail: String::new(),
        };
    }
    let final_text = v
        .get("response")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| last_text.to_string());
    let usage = v.get("stats").map(gemini_usage).unwrap_or_default();
    AgentEvent::Done { final_text, usage }
}

fn gemini_usage(stats: &Value) -> Usage {
    let field = |name: &str| stats.get(name).and_then(Value::as_u64).unwrap_or(0);
    Usage {
        input_tokens: field("input_tokens"),
        output_tokens: field("output_tokens"),
        usd: 0.0,
    }
}

impl LineParser for GeminiParser {
    fn on_line(&mut self, line: &str) -> Vec<AgentEvent> {
        self.parse_line(line)
    }

    fn on_exit(&mut self, success: bool, stderr_tail: &str) -> AgentEvent {
        AgentEvent::Error {
            message: if success {
                "gemini exited without a final result event".to_string()
            } else {
                "gemini exited with a non-zero status".to_string()
            },
            stderr_tail: stderr_tail.to_string(),
        }
    }
}

/// `(path, extra env for `GEMINI_CLI_SYSTEM_SETTINGS_PATH`, guard to keep the
/// temp file alive)`, or `None` if nothing needs injecting (no MCP, default
/// tool set — still written, since `tools.core` always applies).
pub fn settings_env(
    req: &AgentRequest,
) -> std::io::Result<(Vec<(String, String)>, tempfile::NamedTempFile)> {
    let file = write_settings_file(req)?;
    let path: PathBuf = file.path().to_path_buf();
    Ok((
        vec![(
            SETTINGS_PATH_ENV_VAR.to_string(),
            path.display().to_string(),
        )],
        file,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(s: &str) -> Vec<AgentEvent> {
        GeminiParser::new(false).parse_line(s)
    }

    fn run(stream_text: bool, lines: &str) -> Vec<AgentEvent> {
        let mut p = GeminiParser::new(stream_text);
        lines.lines().flat_map(|l| p.parse_line(l)).collect()
    }

    /// v0.66.0's stream-json snapshot (`nonInteractiveCli.test.ts.snap`,
    /// "should emit appropriate events for streaming JSON output"), the
    /// final answer split in two chunks the way a model streams it.
    const STREAM: &str = r#"{"type":"init","timestamp":"2026-10-31T00:00:00.000Z","session_id":"test-session-id","model":"test-model"}
{"type":"message","timestamp":"2026-10-31T00:00:00.000Z","role":"user","content":"Stream test"}
{"type":"message","timestamp":"2026-10-31T00:00:00.000Z","role":"assistant","content":"Thinking...","delta":true}
{"type":"tool_use","timestamp":"2026-10-31T00:00:00.000Z","tool_name":"testTool","tool_id":"tool-1","parameters":{"arg1":"value1"}}
{"type":"tool_result","timestamp":"2026-10-31T00:00:00.000Z","tool_id":"tool-1","status":"success","output":"Tool executed successfully"}
{"type":"message","timestamp":"2026-10-31T00:00:00.000Z","role":"assistant","content":"Final ","delta":true}
{"type":"message","timestamp":"2026-10-31T00:00:00.000Z","role":"assistant","content":"answer","delta":true}
{"type":"result","timestamp":"2026-10-31T00:00:00.000Z","status":"success","stats":{"total_tokens":12,"input_tokens":10,"output_tokens":2,"cached":0,"input":10,"duration_ms":5,"tool_calls":1,"models":{}}}"#;

    fn tool_call() -> AgentEvent {
        AgentEvent::ToolCall {
            id: "tool-1".into(),
            name: "testTool".into(),
            input: serde_json::json!({"arg1": "value1"}),
        }
    }

    fn tool_result() -> AgentEvent {
        AgentEvent::ToolResult {
            id: "tool-1".into(),
            ok: true,
            output: "Tool executed successfully".into(),
        }
    }

    fn done() -> AgentEvent {
        AgentEvent::Done {
            final_text: "Final answer".into(),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 2,
                usd: 0.0,
            },
        }
    }

    fn text(s: &str) -> AgentEvent {
        AgentEvent::Text { text: s.into() }
    }

    fn delta(s: &str) -> AgentEvent {
        AgentEvent::TextDelta { text: s.into() }
    }

    #[test]
    fn streamed_chunks_go_out_as_deltas_then_the_whole_message() {
        assert_eq!(
            run(true, STREAM),
            vec![
                delta("Thinking..."),
                text("Thinking..."),
                tool_call(),
                tool_result(),
                delta("Final "),
                delta("answer"),
                text("Final answer"),
                done(),
            ]
        );
    }

    #[test]
    fn without_stream_text_each_message_is_one_text() {
        assert_eq!(
            run(false, STREAM),
            vec![
                text("Thinking..."),
                tool_call(),
                tool_result(),
                text("Final answer"),
                done(),
            ]
        );
    }

    #[test]
    fn a_message_without_delta_is_whole() {
        let lines = r#"{"type":"message","role":"assistant","content":"a","delta":true}
{"type":"message","role":"assistant","content":"whole"}"#;
        assert_eq!(run(true, lines), vec![delta("a"), text("a"), text("whole")]);
    }

    #[test]
    fn init_is_ignored() {
        assert_eq!(
            line(r#"{"type":"init","session_id":"x","model":"gemini-pro"}"#),
            vec![]
        );
    }

    #[test]
    fn assistant_message_becomes_text() {
        assert_eq!(
            line(r#"{"type":"message","role":"assistant","content":"ok"}"#),
            vec![AgentEvent::Text { text: "ok".into() }]
        );
    }

    #[test]
    fn user_message_is_ignored() {
        assert_eq!(
            line(r#"{"type":"message","role":"user","content":"hi"}"#),
            vec![]
        );
    }

    #[test]
    fn a_failed_tool_reports_its_error() {
        assert_eq!(
            line(
                r#"{"type":"tool_result","tool_id":"t1","status":"error","error":{"type":"x","message":"no such file"}}"#
            ),
            vec![AgentEvent::ToolResult {
                id: "t1".into(),
                ok: false,
                output: "no such file".into()
            }]
        );
    }

    #[test]
    fn result_becomes_done() {
        let events = line(
            r#"{"type":"result","response":"ok","stats":{"input_tokens":10,"output_tokens":2}}"#,
        );
        assert_eq!(
            events,
            vec![AgentEvent::Done {
                final_text: "ok".into(),
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 2,
                    usd: 0.0
                }
            }]
        );
    }

    #[test]
    fn a_bare_error_status_is_an_error() {
        assert_eq!(
            line(r#"{"type":"result","status":"error","stats":{}}"#),
            vec![AgentEvent::Error {
                message: "gemini reported an error".into(),
                stderr_tail: String::new()
            }]
        );
    }

    #[test]
    fn result_with_error_becomes_error() {
        assert_eq!(
            line(r#"{"type":"result","error":{"message":"boom"}}"#),
            vec![AgentEvent::Error {
                message: "boom".into(),
                stderr_tail: String::new()
            }]
        );
    }

    #[test]
    fn settings_json_has_mcp_server_when_requested() {
        let req = sample_request_with_mcp();
        let settings = settings_json(&req);
        assert_eq!(
            settings["mcpServers"]["review-tools"]["httpUrl"],
            "https://example.invalid/mcp"
        );
    }

    fn sample_request_with_mcp() -> AgentRequest {
        use theodore_domain::AgentId;
        use theodore_ports::agent::McpEndpoint;
        AgentRequest {
            agent: AgentId::Gemini,
            prompt: "hi".into(),
            system: None,
            cwd: PathBuf::from("/tmp"),
            mode: AgentMode::ReadOnly,
            mcp: Some(McpEndpoint {
                url: "https://example.invalid/mcp".into(),
                token: "t".into(),
                name: theodore_ports::REVIEW_TOOLS.into(),
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
}

#[cfg(test)]
mod run_mode_tests {
    use super::*;

    #[test]
    fn run_mode_may_edit_and_run_commands() {
        let req = AgentRequest {
            agent: theodore_domain::AgentId::Gemini,
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
        assert!(args.windows(2).any(|w| w == ["--approval-mode", "yolo"]));
        assert!(core_tools(AgentMode::Run).contains(&"ShellTool"));
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;

    #[test]
    fn model_goes_to_dash_m_and_effort_is_not_passed() {
        let req = AgentRequest {
            agent: theodore_domain::AgentId::Gemini,
            prompt: "p".into(),
            system: None,
            cwd: "/tmp".into(),
            mode: AgentMode::ReadOnly,
            mcp: None,
            model: Some("some-model".into()),
            effort: Some("high".into()),
            stall_after_ms: 1000,
            env: Vec::new(),
            user_mcp: false,
            attachments: vec![],
            resume: None,
            stream_text: false,
        };
        let args = build_args(&req, &Default::default());
        assert!(
            args.windows(2).any(|w| w == ["-m", "some-model"]),
            "{args:?}"
        );
        assert!(!args.iter().any(|a| a.contains("effort") || a == "high"));
    }
}
