//! Claude Code runner (`claude` 2.1.x): `-p --output-format stream-json --verbose`.
//!
//! Flags verified against `claude --help` on the installed 2.1.280 binary and a
//! couple of tiny real `-p "reply with the word ok"` runs (see
//! `tests/fixtures/claude_result.jsonl`, redacted). `tool_use`/`tool_result`
//! shapes are long-documented Claude Code stream-json content blocks but were
//! not exercised live here (sandboxed shell denied a tool-using prompt); see
//! `tests/fixtures/claude_tool_call.jsonl`, built from that documented shape.
//!
//! A steerable run (`build_steerable_args`, docs/spaces.md §8 "Send now")
//! reads its messages from stdin instead (`--input-format stream-json
//! --replay-user-messages`, 2.1.312): one written while it works is taken at
//! its next step, in the same turn, and echoed back (`isReplay`) when taken.
//! One that lands as the turn ends starts another turn in the same process
//! and session; the run is done once every message was taken and answered.

use std::path::Path;

use serde_json::Value;
use theodore_domain::Usage;
use theodore_ports::agent::{AgentEvent, AgentMode, AgentRequest};

use crate::config::AgentBinaryConfig;
use crate::edit_guard;
use crate::process::LineParser;

/// The `--mcp-config` JSON, including the bearer token in its `headers`.
/// Written to a 0600 temp file instead of argv (see `write_mcp_config`):
/// `ps`/`/proc/<pid>/cmdline` on this host, or any other process running as
/// the same user, can read another process's argv but not an 0600 file
/// owned by that process's own user.
///
/// `alwaysLoad`: Claude Code otherwise defers MCP tools behind its
/// ToolSearch, and an agent that already has Grep and Read never goes
/// looking — in the code-tools eval, 0 of 34 runs touched a deferred
/// symbol tool. Only Theodore's server is loaded up front; the user's own
/// servers keep the default.
fn mcp_config_json(mcp: &theodore_ports::agent::McpEndpoint) -> Value {
    serde_json::json!({
        "mcpServers": {
            mcp.name.clone(): {
                "type": "http",
                "url": mcp.url,
                "headers": { "Authorization": format!("Bearer {}", mcp.token) },
                "alwaysLoad": true,
            }
        }
    })
}

/// Writes `req.mcp`'s config (bearer token included) to a 0600 temp file,
/// `None` if there's no MCP endpoint for this run. The caller keeps the
/// returned handle alive for the run (`SpawnSpec::guards`) and passes its
/// path to `build_args`.
pub fn write_mcp_config(req: &AgentRequest) -> std::io::Result<Option<tempfile::NamedTempFile>> {
    let Some(mcp) = &req.mcp else { return Ok(None) };
    use std::io::Write;
    let mut file = tempfile::Builder::new()
        .prefix("theodore-claude-mcp-")
        .suffix(".json")
        .tempfile()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(mcp_config_json(mcp).to_string().as_bytes())?;
    file.flush()?;
    Ok(Some(file))
}

/// `AgentRequest` + binary config → argv (binary path is handled by the
/// caller). `mcp_config_path` must be `Some` whenever `req.mcp` is —
/// `write_mcp_config`'s output path, never the config itself.
pub fn build_args(
    req: &AgentRequest,
    cfg: &AgentBinaryConfig,
    mcp_config_path: Option<&Path>,
) -> Vec<String> {
    args_with(
        req,
        cfg,
        mcp_config_path,
        edit_guard::installed_exe(),
        false,
    )
}

/// `build_args` with `guard` as the edit hook's binary (`edit_guard`).
#[cfg(test)]
fn build_args_guarded(
    req: &AgentRequest,
    cfg: &AgentBinaryConfig,
    mcp_config_path: Option<&Path>,
    guard: Option<&Path>,
) -> Vec<String> {
    args_with(req, cfg, mcp_config_path, guard, false)
}

/// [`build_args`] for a run that reads its messages from stdin: the prompt
/// goes in as the first one ([`user_message_line`]), not in argv.
pub fn build_steerable_args(
    req: &AgentRequest,
    cfg: &AgentBinaryConfig,
    mcp_config_path: Option<&Path>,
) -> Vec<String> {
    args_with(req, cfg, mcp_config_path, edit_guard::installed_exe(), true)
}

/// One message of yours in `--input-format stream-json`.
pub fn user_message_line(text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": text },
    })
    .to_string()
}

fn args_with(
    req: &AgentRequest,
    cfg: &AgentBinaryConfig,
    mcp_config_path: Option<&Path>,
    guard: Option<&Path>,
    stdin_input: bool,
) -> Vec<String> {
    let mut args = vec!["-p".to_string()];
    if stdin_input {
        args.extend(["--input-format", "stream-json", "--replay-user-messages"].map(String::from));
    } else {
        args.push(req.prompt.clone());
    }
    args.extend([
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--permission-prompts".to_string(),
        "none".to_string(),
    ]);
    // Text deltas as the answer is generated (`stream_event` lines), on
    // top of the per-message events (2.1.312: `-p` + stream-json only).
    if req.stream_text {
        args.push("--include-partial-messages".to_string());
    }
    // The space's session, continued: everything it read, ran and planned
    // is still there, and `-p` is only the new turn. The flags below are
    // per process, so they still go on every turn.
    if let Some(session) = &req.resume {
        args.push("--resume".to_string());
        args.push(session.clone());
    }
    if let Some(system) = &req.system {
        args.push("--append-system-prompt".to_string());
        args.push(system.clone());
    }
    if let Some(path) = mcp_config_path {
        args.push("--mcp-config".to_string());
        args.push(path.display().to_string());
    }
    // `agents.user_mcp` off: only Theodore's own tools, never the user's
    // MCP servers (`~/.claude.json`, `.mcp.json`, claude.ai connectors).
    // On: those load next to `--mcp-config`, so a run can read the Linear
    // issue or the GitHub thread the user already connected Claude Code to.
    if !req.user_mcp {
        args.push("--strict-mcp-config".to_string());
    }
    // Chat attachments live outside `cwd`; Claude Code's Read only opens
    // them (images and PDFs included) in a directory it was given.
    for dir in attachment_dirs(req) {
        args.push("--add-dir".to_string());
        args.push(dir.display().to_string());
    }
    // The run's own MCP server, whatever it is named. `--permission-prompts
    // none` denies whatever isn't pre-approved.
    let mcp_tools = format!(
        "mcp__{}__*",
        req.mcp
            .as_ref()
            .map_or(theodore_ports::REVIEW_TOOLS, |m| m.name.as_str())
    );
    // The user's servers can't be pre-approved: claude.ai connectors are
    // named only once loaded, and `mcp__*` matches no server (2.1.312).
    // Their calls go to Theodore's `approve_tool` instead, which lets MCP
    // tools through and denies the rest, as `none` would.
    if let Some(tool) = approve_tool(req) {
        if let Some(i) = args.iter().position(|a| a == "--permission-prompts") {
            args[i + 1] = "host".to_string();
        }
        args.push("--permission-prompt-tool".to_string());
        args.push(tool);
    }
    match req.mode {
        AgentMode::ReadOnly => {
            args.push("--allowedTools".to_string());
            args.push(format!("{READ_TOOLS},{mcp_tools}"));
            // `--allowedTools` only pre-approves; anything that writes or runs
            // commands must be denied outright for a review run.
            args.push("--disallowedTools".to_string());
            args.push("Bash,Edit,Write,MultiEdit,NotebookEdit".to_string());
        }
        AgentMode::Edit => {
            // No bare `Edit`/`Write` here: allowed outright they reach any
            // path. `acceptEdits` takes edits in the working directories
            // only, and `edit_settings` names those.
            args.push("--allowedTools".to_string());
            args.push(format!("{READ_TOOLS},Bash,{mcp_tools}"));
            args.push("--permission-mode".to_string());
            args.push("acceptEdits".to_string());
        }
        AgentMode::Plan => {
            // Claude Code's plan mode: it explores and writes its plan to
            // its own plan file (`~/.claude/plans/`), nothing else. Shell
            // and edits are denied outright, as for a review run; `Write`
            // stays for the plan file, which the mode confines it to.
            args.push("--allowedTools".to_string());
            args.push(format!("{READ_TOOLS},{mcp_tools}"));
            args.push("--disallowedTools".to_string());
            args.push("Bash,Edit,MultiEdit,NotebookEdit".to_string());
            args.push("--permission-mode".to_string());
            args.push("plan".to_string());
        }
        AgentMode::Run => {
            // Every tool, no prompts: the CLI's own settings are the limit.
            args.push("--permission-mode".to_string());
            args.push("bypassPermissions".to_string());
        }
    }
    if let Some(settings) = edit_settings(req, guard) {
        args.push("--settings".to_string());
        args.push(settings);
    }
    if let Some(model) = req.model.as_ref().or(cfg.model.as_ref()) {
        args.push("--model".to_string());
        args.push(model.clone());
    }
    match req.effort.as_deref() {
        Some(effort) if EFFORT_LEVELS.contains(&effort) => {
            args.push("--effort".to_string());
            args.push(effort.to_string());
        }
        // Theodore's own level: a space turn also opens its prompt with the
        // keyword (multi-agent); every run goes at the top effort.
        Some(ULTRACODE) => {
            args.push("--effort".to_string());
            args.push("max".to_string());
        }
        // Anything else would fail the whole run on an unknown value.
        Some(effort) => tracing::warn!(effort, "unknown claude effort level, left out"),
        None => {}
    }
    args.extend(cfg.extra_args.iter().cloned());
    args
}

/// `mcp__<server>__approve_tool` when the user's MCP servers load next to
/// Theodore's own and something would otherwise prompt (Run prompts for
/// nothing).
fn approve_tool(req: &AgentRequest) -> Option<String> {
    let mcp = req.mcp.as_ref()?;
    (req.user_mcp && req.mode != AgentMode::Run)
        .then(|| format!("mcp__{}__{}", mcp.name, theodore_ports::APPROVE_TOOL))
}

/// The tools that write files; one `PreToolUse` matcher for all of them.
const EDIT_TOOLS: &str = "Edit|Write|MultiEdit|NotebookEdit";

/// Edit and Run: the `--settings` that keep the file-edit tools inside
/// the worktree and the attachment dirs (docs/spaces.md §5). Edit names
/// them as the only places edits are allowed; both modes get the
/// `edit_guard` hook, which also holds under `bypassPermissions`. Shell
/// commands are not limited.
fn edit_settings(req: &AgentRequest, guard: Option<&Path>) -> Option<String> {
    if req.mode == AgentMode::ReadOnly {
        return None;
    }
    let mut roots = vec![req.cwd.clone()];
    roots.extend(attachment_dirs(req).into_iter().map(Path::to_path_buf));
    if req.mode == AgentMode::Plan {
        // Plan writes only its plan file (`plans`); the worktree stays
        // untouched. No config dir known: every write is denied, and the
        // plan comes from the final message instead.
        roots = crate::models::claude_config_dir()
            .map(|d| vec![d.join("plans")])
            .unwrap_or_default();
    }
    let mut settings = serde_json::json!({});
    if req.mode == AgentMode::Edit {
        let mut allow: Vec<String> = Vec::new();
        for root in &roots {
            // As given and with symlinks resolved (`/tmp` → `/private/tmp`).
            let real = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
            for form in [root, &real] {
                // `//` = an absolute path in Claude Code's rule syntax.
                let rule = format!("Edit(/{}/**)", form.display());
                if !allow.contains(&rule) {
                    allow.push(rule);
                }
            }
        }
        settings["permissions"] = serde_json::json!({ "allow": allow });
    }
    if let Some(exe) = guard {
        settings["hooks"] = serde_json::json!({
            "PreToolUse": [{
                "matcher": EDIT_TOOLS,
                "hooks": [{
                    "type": "command",
                    "command": crate::edit_guard::hook_command(exe, &roots),
                }],
            }],
        });
    }
    (settings != serde_json::json!({})).then(|| settings.to_string())
}

/// Each attachment's directory, once, in order.
fn attachment_dirs(req: &AgentRequest) -> Vec<&Path> {
    let mut dirs: Vec<&Path> = Vec::new();
    for a in &req.attachments {
        if let Some(dir) = a.path.parent()
            && !dirs.contains(&dir)
        {
            dirs.push(dir);
        }
    }
    dirs
}

/// What every run may use without asking: reading, the user's and the
/// repo's skills, subagents, its own todo list and the web. Like Claude
/// Code on its own, minus writing and commands where the mode forbids them.
const READ_TOOLS: &str = "Read,Grep,Glob,Skill,Task,TodoWrite,WebFetch,WebSearch";

/// `--effort <level>` values, as `claude --help` lists them (2.1.310).
pub const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The `agents.claude.effort` value past `max`: Claude Code's multi-agent
/// mode. Runs as `--effort max`; a space turn's prompt opens with it.
pub const ULTRACODE: &str = "ultracode";

#[derive(Debug, Default)]
pub struct ClaudeParser {
    /// The run reads its messages from stdin (a steerable run).
    stdin_input: bool,
    /// Messages written to stdin the CLI hasn't echoed back as taken yet.
    pending: u32,
    /// What the run's turns answered so far, while messages are pending.
    answers: Vec<String>,
    usage: Usage,
    /// Turns finished in this run and not reported yet.
    results: u32,
}

impl ClaudeParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// For [`build_steerable_args`]: `Done` only once every written message
    /// was taken and answered, with every answer in it.
    pub fn with_stdin_input() -> Self {
        Self {
            stdin_input: true,
            ..Self::default()
        }
    }

    fn on_stdin_line(&mut self, line: &str) -> Vec<AgentEvent> {
        if let Ok(v) = serde_json::from_str::<Value>(line.trim())
            && v.get("type").and_then(Value::as_str) == Some("user")
            && v.get("isReplay").and_then(Value::as_bool) == Some(true)
        {
            // One of ours, taken into the conversation.
            self.pending = self.pending.saturating_sub(1);
            return vec![];
        }
        let mut out = Vec::new();
        for ev in parse_line(line) {
            match ev {
                AgentEvent::Done { final_text, usage } => {
                    if !final_text.is_empty() {
                        self.answers.push(final_text);
                    }
                    self.usage += usage;
                    self.results += 1;
                    if self.pending == 0 {
                        out.push(self.done());
                    }
                }
                other => out.push(other),
            }
        }
        out
    }

    fn done(&mut self) -> AgentEvent {
        self.results = 0;
        AgentEvent::Done {
            final_text: std::mem::take(&mut self.answers).join("\n\n"),
            usage: std::mem::take(&mut self.usage),
        }
    }
}

/// The plan a plan-mode run hands in, from its tool calls. Recorded from
/// `claude -p … --permission-mode plan` (2.1.312, `tests/fixtures/
/// claude_plan_mode.jsonl`): `-p` has no `ExitPlanMode`, so the plan is
/// the file it `Write`s under `~/.claude/plans/` (Plan mode denies `Edit`,
/// so a revision writes it whole again). An `ExitPlanMode` call, where a
/// version has one, carries it as `input.plan`.
fn plans(events: &[AgentEvent]) -> Vec<AgentEvent> {
    events
        .iter()
        .filter_map(|ev| {
            let AgentEvent::ToolCall { name, input, .. } = ev else {
                return None;
            };
            let text = |key: &str| {
                input
                    .get(key)
                    .and_then(Value::as_str)
                    .filter(|t| !t.trim().is_empty())
                    .map(str::to_string)
            };
            match name.as_str() {
                "ExitPlanMode" => text("plan"),
                "Write" if text("file_path").is_some_and(|p| is_plan_file(&p)) => text("content"),
                _ => None,
            }
        })
        .map(|text| AgentEvent::Plan { text })
        .collect()
}

/// A file under Claude Code's plan directory (`~/.claude/plans/*.md`).
fn is_plan_file(path: &str) -> bool {
    path.contains("/.claude/plans/") && path.ends_with(".md")
}

/// Pure: one stream-json line in, zero or more normalized events out.
pub fn parse_line(line: &str) -> Vec<AgentEvent> {
    let line = line.trim();
    if line.is_empty() {
        return vec![];
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return vec![];
    };
    match v.get("type").and_then(Value::as_str) {
        Some("assistant") => content_blocks(&v),
        Some("user") => tool_results(&v),
        Some("result") => result_events(&v),
        // Only with `--include-partial-messages`: the raw API stream.
        Some("stream_event") => text_delta(&v).into_iter().collect(),
        // `init` opens every run with the session id `--resume` takes.
        Some("system") if v.get("subtype").and_then(Value::as_str) == Some("init") => {
            session_event(&v).into_iter().collect()
        }
        _ => vec![], // system/rate_limit_event/etc: not user-visible stream content
    }
}

/// A `content_block_delta` carrying answer text (`text_delta`); thinking
/// and tool-input deltas are left to the whole-message events.
fn text_delta(v: &Value) -> Option<AgentEvent> {
    let event = v.get("event")?;
    if event.get("type").and_then(Value::as_str) != Some("content_block_delta") {
        return None;
    }
    let delta = event.get("delta")?;
    if delta.get("type").and_then(Value::as_str) != Some("text_delta") {
        return None;
    }
    let text = delta.get("text").and_then(Value::as_str)?;
    (!text.is_empty()).then(|| AgentEvent::TextDelta {
        text: text.to_string(),
    })
}

fn session_event(v: &Value) -> Option<AgentEvent> {
    let id = v.get("session_id").and_then(Value::as_str)?;
    (!id.is_empty()).then(|| AgentEvent::Session { id: id.to_string() })
}

/// A successful result repeats the session id (in case `init` was missed)
/// before `Done`. A failed one doesn't: a `--resume` of a session that is
/// gone fails carrying the id it was given.
fn result_events(v: &Value) -> Vec<AgentEvent> {
    let done = result_event(v);
    match (&done, session_event(v)) {
        (AgentEvent::Done { .. }, Some(session)) => vec![session, done],
        _ => vec![done],
    }
}

fn content_blocks(v: &Value) -> Vec<AgentEvent> {
    let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) else {
        return vec![];
    };
    blocks
        .iter()
        .filter_map(|b| match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = b.get("text").and_then(Value::as_str).unwrap_or_default();
                (!text.is_empty()).then(|| AgentEvent::Text {
                    text: text.to_string(),
                })
            }
            Some("thinking") => {
                let text = b
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                (!text.is_empty()).then(|| AgentEvent::Thinking {
                    text: text.to_string(),
                })
            }
            Some("tool_use") => Some(AgentEvent::ToolCall {
                id: b
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                name: b
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input: b.get("input").cloned().unwrap_or(Value::Null),
            }),
            _ => None,
        })
        .collect()
}

fn tool_results(v: &Value) -> Vec<AgentEvent> {
    let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) else {
        return vec![];
    };
    blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        .map(|b| {
            let is_error = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
            AgentEvent::ToolResult {
                id: b
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                ok: !is_error,
                output: stringify_tool_content(b.get("content")),
            }
        })
        .collect()
}

fn stringify_tool_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn result_event(v: &Value) -> AgentEvent {
    let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
    if is_error {
        // `errors` when it failed before a turn started (`--resume` of a
        // session that is gone: "No conversation found with session ID: …").
        let errors = v
            .get("errors")
            .and_then(Value::as_array)
            .map(|e| {
                e.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|e| !e.is_empty());
        let message = v
            .get("result")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(errors)
            .unwrap_or_else(|| "claude reported an error".to_string());
        return AgentEvent::Error {
            message,
            stderr_tail: String::new(),
        };
    }
    let final_text = v
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // `total_cost_usd` sits next to `usage`, not inside it.
    let usd = v
        .get("total_cost_usd")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let usage = v
        .get("usage")
        .map(|u| claude_usage(u, usd))
        .unwrap_or_default();
    AgentEvent::Done { final_text, usage }
}

fn claude_usage(u: &Value, usd: f64) -> Usage {
    let field = |name: &str| u.get(name).and_then(Value::as_u64).unwrap_or(0);
    let input_tokens = field("input_tokens")
        + field("cache_creation_input_tokens")
        + field("cache_read_input_tokens");
    Usage {
        input_tokens,
        output_tokens: field("output_tokens"),
        usd,
    }
}

impl LineParser for ClaudeParser {
    fn on_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let mut events = if self.stdin_input {
            self.on_stdin_line(line)
        } else {
            parse_line(line)
        };
        let plans = plans(&events);
        // Before `Done`: the plan is part of the run, not after it.
        let at = events
            .iter()
            .position(|e| matches!(e, AgentEvent::Done { .. } | AgentEvent::Error { .. }))
            .unwrap_or(events.len());
        events.splice(at..at, plans);
        events
    }

    fn on_exit(&mut self, success: bool, stderr_tail: &str) -> AgentEvent {
        // A steerable run whose input was closed while a message was still
        // pending: its answers so far are the turn's.
        if success && self.results > 0 {
            return self.done();
        }
        AgentEvent::Error {
            message: if success {
                "claude exited without a final result event".to_string()
            } else {
                "claude exited with a non-zero status".to_string()
            },
            stderr_tail: stderr_tail.to_string(),
        }
    }

    fn on_input(&mut self) {
        self.pending += 1;
    }

    fn awaiting_input(&self) -> bool {
        self.pending > 0 && self.results > 0
    }

    /// `system` / `api_retry` events: Claude Code retrying its API.
    fn is_transport_notice(&self, line: &str) -> bool {
        line.len() < 16 * 1024
            && serde_json::from_str::<Value>(line).is_ok_and(|v| {
                v.get("type").and_then(Value::as_str) == Some("system")
                    && v.get("subtype").and_then(Value::as_str) == Some("api_retry")
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_block_becomes_text_event() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ok"}]}}"#;
        assert_eq!(
            parse_line(line),
            vec![AgentEvent::Text { text: "ok".into() }]
        );
    }

    #[test]
    fn thinking_block_becomes_thinking_event() {
        let line =
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hm"}]}}"#;
        assert_eq!(
            parse_line(line),
            vec![AgentEvent::Thinking { text: "hm".into() }]
        );
    }

    #[test]
    fn empty_thinking_block_is_dropped() {
        let line =
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":""}]}}"#;
        assert_eq!(parse_line(line), vec![]);
    }

    #[test]
    fn tool_use_becomes_tool_call() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"a.rs"}}]}}"#;
        assert_eq!(
            parse_line(line),
            vec![AgentEvent::ToolCall {
                id: "t1".into(),
                name: "Read".into(),
                input: serde_json::json!({"file_path": "a.rs"}),
            }]
        );
    }

    #[test]
    fn tool_result_becomes_tool_result_event() {
        let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":false,"content":"file contents"}]}}"#;
        assert_eq!(
            parse_line(line),
            vec![AgentEvent::ToolResult {
                id: "t1".into(),
                ok: true,
                output: "file contents".into()
            }]
        );
    }

    #[test]
    fn success_result_becomes_done() {
        let line = r#"{"type":"result","is_error":false,"result":"ok","total_cost_usd":0.097,"usage":{"input_tokens":2,"output_tokens":82}}"#;
        let events = parse_line(line);
        assert_eq!(
            events,
            vec![AgentEvent::Done {
                final_text: "ok".into(),
                usage: Usage {
                    input_tokens: 2,
                    output_tokens: 82,
                    usd: 0.097
                },
            }]
        );
    }

    #[test]
    fn error_result_becomes_error() {
        let line = r#"{"type":"result","is_error":true,"result":"boom"}"#;
        assert_eq!(
            parse_line(line),
            vec![AgentEvent::Error {
                message: "boom".into(),
                stderr_tail: String::new()
            }]
        );
    }

    #[test]
    fn system_lines_are_ignored() {
        let line = r#"{"type":"system","subtype":"init","cwd":"/tmp"}"#;
        assert_eq!(parse_line(line), vec![]);
        let line = r#"{"type":"system","subtype":"session_state_changed","state":"idle","session_id":"s1"}"#;
        assert_eq!(parse_line(line), vec![]);
    }

    #[test]
    fn init_reports_the_session() {
        let line = r#"{"type":"system","subtype":"init","cwd":"/tmp","session_id":"8f1c"}"#;
        assert_eq!(
            parse_line(line),
            vec![AgentEvent::Session { id: "8f1c".into() }]
        );
    }

    #[test]
    fn a_successful_result_repeats_the_session_before_done() {
        let line = r#"{"type":"result","is_error":false,"result":"ok","session_id":"8f1c"}"#;
        let events = parse_line(line);
        assert_eq!(events[0], AgentEvent::Session { id: "8f1c".into() });
        assert!(matches!(events[1], AgentEvent::Done { .. }));
    }

    /// Recorded from `claude -p … --resume <unknown id>` (2.1.310): no
    /// `init`, no `result` text, the reason in `errors`.
    #[test]
    fn a_gone_session_fails_with_its_reason_and_no_session() {
        let line = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":0,"session_id":"0000","total_cost_usd":0,"errors":["No conversation found with session ID: 0000"]}"#;
        assert_eq!(
            parse_line(line),
            vec![AgentEvent::Error {
                message: "No conversation found with session ID: 0000".into(),
                stderr_tail: String::new()
            }]
        );
    }

    #[test]
    fn garbage_line_is_ignored() {
        assert_eq!(parse_line("not json"), vec![]);
        assert_eq!(parse_line(""), vec![]);
    }

    #[test]
    fn exit_without_result_is_the_one_terminal_event() {
        let mut parser = ClaudeParser::new();
        let ev = parser.on_exit(false, "panic: x");
        assert_eq!(
            ev,
            AgentEvent::Error {
                message: "claude exited with a non-zero status".into(),
                stderr_tail: "panic: x".into(),
            }
        );
    }
}

#[cfg(test)]
mod read_only_tests {
    use super::*;

    #[test]
    fn read_only_runs_deny_shell_and_writes() {
        let req = theodore_ports::AgentRequest {
            agent: theodore_domain::AgentId::Claude,
            prompt: "p".into(),
            system: None,
            cwd: "/tmp".into(),
            mode: AgentMode::ReadOnly,
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
        let args = build_args(&req, &Default::default(), None);
        let i = args
            .iter()
            .position(|a| a == "--disallowedTools")
            .expect("denylist");
        assert!(args[i + 1].contains("Bash") && args[i + 1].contains("Edit"));
    }
}

#[cfg(test)]
mod mcp_config_tests {
    use super::*;
    use theodore_ports::agent::McpEndpoint;

    fn req_with_mcp(token: &str) -> AgentRequest {
        AgentRequest {
            agent: theodore_domain::AgentId::Claude,
            prompt: "p".into(),
            system: None,
            cwd: "/tmp".into(),
            mode: AgentMode::ReadOnly,
            mcp: Some(McpEndpoint {
                url: "http://127.0.0.1:1/mcp".into(),
                token: token.into(),
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

    /// The whole point of writing the config to a file: `ps`/`/proc` must
    /// never see the token. Argv only ever carries the file's path.
    #[test]
    fn mcp_config_token_never_reaches_argv() {
        let req = req_with_mcp("super-secret-token");
        let guard = write_mcp_config(&req).unwrap().expect("mcp config file");
        let args = build_args(&req, &Default::default(), Some(guard.path()));

        for arg in &args {
            assert!(
                !arg.contains("super-secret-token"),
                "token leaked into argv: {arg:?}"
            );
        }
        let i = args
            .iter()
            .position(|a| a == "--mcp-config")
            .expect("--mcp-config flag");
        assert_eq!(args[i + 1], guard.path().display().to_string());

        // The token does land in the file `--mcp-config` points at.
        let contents = std::fs::read_to_string(guard.path()).unwrap();
        assert!(contents.contains("super-secret-token"));
        assert!(contents.contains("Bearer"));
    }

    /// A space turn's server is `space-tools`: the config key and the
    /// pre-approved tool names follow the endpoint's name.
    #[test]
    fn the_server_name_follows_the_endpoint() {
        let mut req = req_with_mcp("t");
        req.mode = AgentMode::Edit;
        req.mcp.as_mut().unwrap().name = theodore_ports::SPACE_TOOLS.into();
        let config = mcp_config_json(req.mcp.as_ref().unwrap());
        assert!(config["mcpServers"]["space-tools"]["url"].is_string());
        // Never deferred behind ToolSearch.
        assert_eq!(config["mcpServers"]["space-tools"]["alwaysLoad"], true);
        assert!(config["mcpServers"].get("review-tools").is_none());
        let args = build_args(&req, &Default::default(), None);
        let i = args.iter().position(|a| a == "--allowedTools").unwrap();
        assert!(args[i + 1].ends_with(",mcp__space-tools__*"), "{args:?}");
    }

    #[test]
    fn no_mcp_means_no_config_file_and_no_flag() {
        let mut req = req_with_mcp("unused");
        req.mcp = None;
        assert!(write_mcp_config(&req).unwrap().is_none());
        let args = build_args(&req, &Default::default(), None);
        assert!(!args.iter().any(|a| a == "--mcp-config"));
        // …and still none of the user's own MCP servers.
        assert!(args.iter().any(|a| a == "--strict-mcp-config"));
    }

    /// `agents.user_mcp` off: only Theodore's own server, and only its
    /// tools pre-approved.
    #[test]
    fn user_mcp_off_keeps_the_run_to_theodores_own_server() {
        let req = req_with_mcp("t");
        let args = build_args(&req, &Default::default(), None);
        assert!(args.iter().any(|a| a == "--strict-mcp-config"));
        let i = args.iter().position(|a| a == "--allowedTools").unwrap();
        assert_eq!(
            args[i + 1],
            "Read,Grep,Glob,Skill,Task,TodoWrite,WebFetch,WebSearch,mcp__review-tools__*"
        );
    }

    /// Skills, subagents, todos and the web in every mode, as in Claude
    /// Code itself; a review run still can't write or run commands.
    #[test]
    fn every_mode_has_skills_subagents_and_the_web() {
        for mode in [AgentMode::ReadOnly, AgentMode::Edit] {
            let mut req = req_with_mcp("t");
            req.mode = mode;
            let args = build_args(&req, &Default::default(), None);
            let i = args.iter().position(|a| a == "--allowedTools").unwrap();
            for tool in ["Skill", "Task", "TodoWrite", "WebFetch", "WebSearch"] {
                assert!(args[i + 1].split(',').any(|t| t == tool), "{mode:?} {tool}");
            }
        }
        let args = build_args(&req_with_mcp("t"), &Default::default(), None);
        let d = args.iter().position(|a| a == "--disallowedTools").unwrap();
        assert_eq!(args[d + 1], "Bash,Edit,Write,MultiEdit,NotebookEdit");
    }

    /// `flag`'s value, if `args` has it.
    fn flag<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        let i = args.iter().position(|a| a == flag)?;
        Some(args[i + 1].as_str())
    }

    /// `agents.user_mcp` on: the user's own MCP servers load next to
    /// `--mcp-config`, and their calls are put to Theodore's `approve_tool`
    /// instead of denied (issue #83); a review run still can't write or
    /// run commands.
    #[test]
    fn user_mcp_on_loads_the_users_servers_and_asks_theodore() {
        for (mode, server) in [
            (AgentMode::ReadOnly, theodore_ports::REVIEW_TOOLS),
            (AgentMode::Edit, theodore_ports::SPACE_TOOLS),
        ] {
            let mut req = req_with_mcp("t");
            req.mode = mode;
            req.user_mcp = true;
            req.mcp.as_mut().unwrap().name = server.into();
            let guard = write_mcp_config(&req).unwrap().expect("mcp config file");
            let args = build_args(&req, &Default::default(), Some(guard.path()));
            assert!(!args.iter().any(|a| a == "--strict-mcp-config"), "{args:?}");
            assert!(args.iter().any(|a| a == "--mcp-config"), "{args:?}");
            assert_eq!(
                flag(&args, "--permission-prompt-tool"),
                Some(format!("mcp__{server}__approve_tool").as_str()),
                "{mode:?}"
            );
            // `none` would deny before the tool is asked.
            assert_eq!(flag(&args, "--permission-prompts"), Some("host"));
            let allowed: Vec<&str> = flag(&args, "--allowedTools").unwrap().split(',').collect();
            assert!(allowed.contains(&format!("mcp__{server}__*").as_str()));
            // Matches no server in Claude Code: dead weight, and misleading.
            assert!(!allowed.contains(&"mcp__*"), "{allowed:?}");
        }
        let mut req = req_with_mcp("t");
        req.user_mcp = true;
        let args = build_args(&req, &Default::default(), None);
        let denied = flag(&args, "--disallowedTools").expect("denylist");
        assert!(denied.contains("Bash") && denied.contains("Write"));
    }

    /// Off, without Theodore's server, or in Run: nothing to ask, so
    /// whatever isn't pre-approved stays denied, as before.
    #[test]
    fn no_approve_tool_without_user_mcp_a_server_or_prompts() {
        let off = req_with_mcp("t");
        let mut no_server = req_with_mcp("t");
        no_server.user_mcp = true;
        no_server.mcp = None;
        let mut run = req_with_mcp("t");
        run.user_mcp = true;
        run.mode = AgentMode::Run;
        for req in [off, no_server, run] {
            let args = build_args(&req, &Default::default(), None);
            assert!(
                !args.iter().any(|a| a == "--permission-prompt-tool"),
                "{args:?}"
            );
            assert_eq!(flag(&args, "--permission-prompts"), Some("none"));
            assert!(!args.iter().any(|a| a.split(',').any(|t| t == "mcp__*")));
        }
    }

    #[cfg(unix)]
    #[test]
    fn mcp_config_file_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let req = req_with_mcp("t");
        let guard = write_mcp_config(&req).unwrap().expect("mcp config file");
        let mode = std::fs::metadata(guard.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[cfg(test)]
mod run_mode_tests {
    use super::*;

    #[test]
    fn run_mode_may_edit_and_run_commands() {
        let req = AgentRequest {
            agent: theodore_domain::AgentId::Claude,
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
        let args = build_args(&req, &Default::default(), None);
        assert!(
            args.windows(2)
                .any(|w| w == ["--permission-mode", "bypassPermissions"])
        );
        assert!(!args.iter().any(|a| a == "--disallowedTools"));
        assert!(args.iter().any(|a| a == "--strict-mcp-config"));
    }

    #[test]
    fn run_mode_with_user_mcp_skips_strict_mcp_config() {
        let req = AgentRequest {
            agent: theodore_domain::AgentId::Claude,
            prompt: "p".into(),
            system: None,
            cwd: "/tmp".into(),
            mode: AgentMode::Run,
            mcp: None,
            model: None,
            effort: None,
            stall_after_ms: 1000,
            env: Vec::new(),
            user_mcp: true,
            attachments: vec![],
            resume: None,
            stream_text: false,
        };
        let args = build_args(&req, &Default::default(), None);
        assert!(!args.iter().any(|a| a == "--strict-mcp-config"));
        // bypassPermissions already allows every tool; no allowlist to widen.
        assert!(!args.iter().any(|a| a == "--allowedTools"));
    }

    fn settings_of(args: &[String]) -> Option<serde_json::Value> {
        let i = args.iter().position(|a| a == "--settings")?;
        Some(serde_json::from_str(&args[i + 1]).unwrap())
    }

    /// Edit and Run: the file-edit tools stay in the worktree and the
    /// attachment dirs (docs/spaces.md §5); a review run gets nothing.
    #[test]
    fn edits_are_kept_inside_the_worktree_and_attachments() {
        let mut req = AgentRequest {
            agent: theodore_domain::AgentId::Claude,
            prompt: "p".into(),
            system: None,
            cwd: "/data/spaces/a1".into(),
            mode: AgentMode::Edit,
            mcp: None,
            model: None,
            effort: None,
            stall_after_ms: 1000,
            env: Vec::new(),
            user_mcp: false,
            attachments: vec![theodore_ports::AgentAttachment {
                path: "/data/attachments/x/shot.png".into(),
                mime: "image/png".into(),
            }],
            resume: None,
            stream_text: false,
        };
        let exe = Path::new("/Applications/Theodore.app/Contents/MacOS/theodore");

        let args = build_args_guarded(&req, &Default::default(), None, Some(exe));
        let i = args.iter().position(|a| a == "--allowedTools").unwrap();
        let allowed: Vec<&str> = args[i + 1].split(',').collect();
        for bare in ["Edit", "Write", "MultiEdit", "NotebookEdit"] {
            assert!(!allowed.contains(&bare), "{bare} would reach any path");
        }
        assert!(allowed.contains(&"Bash"));
        let s = settings_of(&args).expect("settings");
        assert_eq!(
            s["permissions"]["allow"],
            serde_json::json!(["Edit(//data/spaces/a1/**)", "Edit(//data/attachments/x/**)"])
        );
        let hook = &s["hooks"]["PreToolUse"][0];
        assert_eq!(hook["matcher"], "Edit|Write|MultiEdit|NotebookEdit");
        assert_eq!(
            hook["hooks"][0]["command"],
            "'/Applications/Theodore.app/Contents/MacOS/theodore' --theodore-edit-guard \
             '/data/spaces/a1' '/data/attachments/x'"
        );

        // Run: no allow rules (everything is allowed), the hook still holds.
        req.mode = AgentMode::Run;
        let s = settings_of(&build_args_guarded(
            &req,
            &Default::default(),
            None,
            Some(exe),
        ))
        .expect("settings");
        assert!(s.get("permissions").is_none());
        assert!(s["hooks"]["PreToolUse"][0]["hooks"][0]["command"].is_string());
        // A binary that never installed the guard offers no hook.
        assert!(settings_of(&build_args_guarded(&req, &Default::default(), None, None)).is_none());

        // Plan: the guard's only root is Claude's plan directory, so the
        // plan file is written and the worktree never is.
        req.mode = AgentMode::Plan;
        let s = settings_of(&build_args_guarded(
            &req,
            &Default::default(),
            None,
            Some(exe),
        ))
        .expect("settings");
        assert!(s.get("permissions").is_none());
        let cmd = s["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(cmd.ends_with("/plans'"), "{cmd}");
        assert!(!cmd.contains("/data/spaces/a1"), "{cmd}");

        req.mode = AgentMode::ReadOnly;
        let args = build_args_guarded(&req, &Default::default(), None, Some(exe));
        assert!(settings_of(&args).is_none());
    }
}

#[cfg(test)]
mod model_and_effort_tests {
    use super::*;

    fn req(model: Option<&str>, effort: Option<&str>) -> AgentRequest {
        AgentRequest {
            agent: theodore_domain::AgentId::Claude,
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
    fn stream_text_asks_for_partial_messages() {
        let partial = "--include-partial-messages".to_string();
        let cfg = AgentBinaryConfig::default();
        let mut r = req(None, None);
        assert!(!build_args(&r, &cfg, None).contains(&partial));
        r.stream_text = true;
        assert!(build_args(&r, &cfg, None).contains(&partial));
    }

    /// Recorded from `claude -p … --output-format stream-json --verbose
    /// --include-partial-messages` (2.1.312).
    #[test]
    fn partial_text_deltas_become_text_delta_events() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"{\"groups\":[{\""}},"session_id":"s1"}"#;
        assert_eq!(
            parse_line(line),
            vec![AgentEvent::TextDelta {
                text: r#"{"groups":[{""#.into()
            }]
        );
        for other in [
            r#"{"type":"stream_event","event":{"type":"message_start","message":{}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hm"}}}"#,
            r#"{"type":"stream_event","event":{"type":"message_stop"}}"#,
        ] {
            assert_eq!(parse_line(other), vec![], "{other}");
        }
    }

    fn flag(args: &[String], name: &str) -> Option<String> {
        let i = args.iter().position(|a| a == name)?;
        args.get(i + 1).cloned()
    }

    #[test]
    fn model_and_effort_reach_their_flags() {
        let args = build_args(&req(Some("opus"), Some("high")), &Default::default(), None);
        assert_eq!(flag(&args, "--model").as_deref(), Some("opus"));
        assert_eq!(flag(&args, "--effort").as_deref(), Some("high"));
    }

    /// A resumed turn keeps every per-process flag: the system prompt,
    /// permissions, model.
    #[test]
    fn a_resumed_turn_passes_the_session_and_still_the_per_process_flags() {
        let mut r = req(Some("opus"), None);
        r.system = Some("rules".into());
        r.resume = Some("8f1c".into());
        let args = build_args(&r, &Default::default(), None);
        assert_eq!(flag(&args, "--resume").as_deref(), Some("8f1c"));
        assert_eq!(flag(&args, "-p").as_deref(), Some("p"));
        assert_eq!(
            flag(&args, "--append-system-prompt").as_deref(),
            Some("rules")
        );
        assert_eq!(flag(&args, "--model").as_deref(), Some("opus"));
        assert!(args.iter().any(|a| a == "--disallowedTools"));

        r.resume = None;
        let args = build_args(&r, &Default::default(), None);
        assert!(!args.iter().any(|a| a == "--resume"));
    }

    #[test]
    fn unset_means_the_cli_defaults() {
        let args = build_args(&req(None, None), &Default::default(), None);
        assert!(!args.iter().any(|a| a == "--model" || a == "--effort"));
    }

    #[test]
    fn every_listed_level_is_passed() {
        for level in EFFORT_LEVELS {
            let args = build_args(&req(None, Some(level)), &Default::default(), None);
            assert_eq!(flag(&args, "--effort").as_deref(), Some(level));
        }
    }

    #[test]
    fn an_unknown_level_is_left_out() {
        let args = build_args(&req(None, Some("ultra")), &Default::default(), None);
        assert!(!args.iter().any(|a| a == "--effort"));
    }

    #[test]
    fn the_request_model_wins_over_the_config_one() {
        let cfg = AgentBinaryConfig {
            model: Some("haiku".into()),
            ..Default::default()
        };
        let args = build_args(&req(Some("sonnet"), None), &cfg, None);
        assert_eq!(flag(&args, "--model").as_deref(), Some("sonnet"));
    }

    #[test]
    fn ultracode_runs_at_max_effort() {
        let args = build_args(&req(None, Some(ULTRACODE)), &Default::default(), None);
        let at = args.iter().position(|a| a == "--effort").unwrap();
        assert_eq!(args[at + 1], "max");
    }

    fn attachment(path: &str, mime: &str) -> theodore_ports::AgentAttachment {
        theodore_ports::AgentAttachment {
            path: path.into(),
            mime: mime.into(),
        }
    }

    #[test]
    fn each_attachment_directory_is_added_once() {
        let mut r = req(None, None);
        assert!(!build_args(&r, &Default::default(), None).contains(&"--add-dir".to_string()));
        r.attachments = vec![
            attachment("/data/attachments/a/shot.png", "image/png"),
            attachment("/data/attachments/b/spec.pdf", "application/pdf"),
            attachment("/data/attachments/a/shot.png", "image/png"),
        ];
        let args = build_args(&r, &Default::default(), None);
        let dirs: Vec<&str> = args
            .windows(2)
            .filter(|w| w[0] == "--add-dir")
            .map(|w| w[1].as_str())
            .collect();
        assert_eq!(dirs, ["/data/attachments/a", "/data/attachments/b"]);
    }

    // ── steerable runs (stdin input) ────────────────────────────────────

    fn replay(text: &str) -> String {
        serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": text },
            "isReplay": true,
        })
        .to_string()
    }

    fn result(text: &str) -> String {
        serde_json::json!({
            "type": "result", "subtype": "success", "is_error": false,
            "result": text, "session_id": "S",
            "usage": { "input_tokens": 1, "output_tokens": 2 },
        })
        .to_string()
    }

    fn done_text(events: &[AgentEvent]) -> Option<String> {
        events.iter().find_map(|e| match e {
            AgentEvent::Done { final_text, .. } => Some(final_text.clone()),
            _ => None,
        })
    }

    #[test]
    fn a_steerable_run_takes_the_prompt_from_stdin() {
        let r = req(None, None);
        let args = build_steerable_args(&r, &Default::default(), None);
        assert_eq!(args[0], "-p");
        assert_eq!(
            flag(&args, "--input-format").as_deref(),
            Some("stream-json")
        );
        assert!(args.contains(&"--replay-user-messages".to_string()));
        assert!(!args.contains(&r.prompt), "the prompt goes in on stdin");
        let line: Value = serde_json::from_str(&user_message_line("fix \"it\"")).unwrap();
        assert_eq!(line["type"], "user");
        assert_eq!(line["message"]["content"], "fix \"it\"");
    }

    #[test]
    fn a_message_taken_mid_turn_ends_in_one_done() {
        let mut p = ClaudeParser::with_stdin_input();
        p.on_input();
        assert!(p.on_line(&replay("first")).is_empty());
        p.on_input(); // steered while the tool ran
        assert!(p.on_line(&replay("also this")).is_empty());
        assert!(!p.awaiting_input());
        let events = p.on_line(&result("did both"));
        assert_eq!(done_text(&events).as_deref(), Some("did both"));
    }

    #[test]
    fn a_message_landing_as_the_turn_ends_waits_for_its_own_answer() {
        let mut p = ClaudeParser::with_stdin_input();
        p.on_input();
        p.on_line(&replay("first"));
        p.on_input(); // written, not taken yet
        let events = p.on_line(&result("one"));
        assert_eq!(done_text(&events), None, "held for the second message");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::Session { .. }))
        );
        assert!(p.awaiting_input());
        p.on_line(&replay("second"));
        assert!(!p.awaiting_input());
        let events = p.on_line(&result("two"));
        assert_eq!(done_text(&events).as_deref(), Some("one\n\ntwo"));
        let usage = events.iter().find_map(|e| match e {
            AgentEvent::Done { usage, .. } => Some(*usage),
            _ => None,
        });
        assert_eq!(usage.unwrap().output_tokens, 4, "both turns counted");
    }

    #[test]
    fn closed_input_with_a_held_answer_still_ends_in_done() {
        let mut p = ClaudeParser::with_stdin_input();
        p.on_input();
        p.on_line(&replay("first"));
        p.on_input();
        p.on_line(&result("one"));
        match p.on_exit(true, "") {
            AgentEvent::Done { final_text, .. } => assert_eq!(final_text, "one"),
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn tool_results_are_not_taken_for_replays() {
        let mut p = ClaudeParser::with_stdin_input();
        p.on_input();
        let tool = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}}"#;
        let events = p.on_line(tool);
        assert!(matches!(events.as_slice(), [AgentEvent::ToolResult { .. }]));
        p.on_line(&result("x"));
        assert!(p.awaiting_input(), "the prompt itself was never echoed");
    }
}
