//! AgentPort (plan §8): runs the `claude` / `codex` / `gemini` CLIs and
//! normalizes their JSON streams into `AgentEvent`.
//!
//! `AgentCli` owns per-agent binary configs (path override, extra args,
//! model) and dispatches `AgentRequest` to the matching builder + parser in
//! `claude` / `codex` / `gemini`. Process lifecycle (spawn, stdin, stall
//! detection, cancellation) is shared in `process`; it is CLI-agnostic.
//!
//! Neither CLI needed the `theodore mcp-bridge` stdio→HTTP proxy the plan
//! sketches as a fallback: Claude Code takes an inline HTTP MCP server via
//! `--mcp-config`, and Codex's `-c mcp_servers.<name>.url=...` override also
//! speaks HTTP directly (confirmed against installed `codex exec --help` /
//! `codex mcp add --help`). Gemini is settings-file only but that's still
//! native HTTP (`httpUrl`), not stdio. If a future CLI only speaks stdio MCP,
//! the bridge becomes necessary and is a separate slice.

pub mod claude;
pub mod codex;
pub mod config;
pub mod detect;
pub mod edit_guard;
pub mod env;
pub mod exit_output;
pub mod gemini;
pub mod models;
pub mod process;
pub mod pty;
pub mod shell;

use async_trait::async_trait;
use theodore_domain::AgentId;
use theodore_ports::agent::{
    AgentError, AgentInfo, AgentPort, AgentRequest, AgentRun, ModelInfo, Steer,
};

pub use config::{AgentBinaryConfig, AgentCliConfig};
pub use pty::PtyRunner;
pub use shell::ShellRunner;

/// `AgentPort` over local CLI processes.
#[derive(Debug, Clone, Default)]
pub struct AgentCli {
    configs: AgentCliConfig,
    pid_dir: Option<std::path::PathBuf>,
    /// Each CLI's `--version` from the last `detect`, for the log.
    versions: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<AgentId, String>>>,
    /// `codex debug models`: a success for the process, a failure (not
    /// installed, or the listing broke) for `CODEX_RETRY_AFTER`.
    codex_models: std::sync::Arc<tokio::sync::Mutex<CodexModels>>,
}

/// How long a failed codex model listing stands before it's tried again:
/// the picker asks on every open, and a missing CLI stays missing.
const CODEX_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Why there's no codex model list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CodexListError {
    /// No `codex` binary: an expected state, logged at debug.
    NotInstalled,
    /// Installed, but `codex debug models` failed.
    Failed(String),
}

/// The cached outcome of listing codex's models.
#[derive(Debug, Default)]
struct CodexModels {
    listed: Option<Vec<ModelInfo>>,
    /// The last failure and when it happened.
    failed: Option<(std::time::Instant, CodexListError)>,
    /// A real failure was already warned about (once per process).
    warned: bool,
}

/// The level a failed listing is logged at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListLog {
    Debug,
    Warn,
}

impl CodexModels {
    /// The cached answer, if one still stands at `now`.
    fn cached(&self, now: std::time::Instant) -> Option<Vec<ModelInfo>> {
        if let Some(list) = &self.listed {
            return Some(list.clone());
        }
        match &self.failed {
            Some((at, _)) if now.duration_since(*at) < CODEX_RETRY_AFTER => Some(Vec::new()),
            _ => None,
        }
    }

    /// Keeps a fresh outcome; a failure also says how to log it.
    fn record(
        &mut self,
        now: std::time::Instant,
        outcome: Result<Vec<ModelInfo>, CodexListError>,
    ) -> (Vec<ModelInfo>, Option<ListLog>) {
        match outcome {
            Ok(list) => {
                self.listed = Some(list.clone());
                self.failed = None;
                (list, None)
            }
            Err(e) => {
                let level = match &e {
                    CodexListError::NotInstalled => ListLog::Debug,
                    CodexListError::Failed(_) if self.warned => ListLog::Debug,
                    CodexListError::Failed(_) => {
                        self.warned = true;
                        ListLog::Warn
                    }
                };
                self.failed = Some((now, e));
                (Vec::new(), Some(level))
            }
        }
    }
}

impl AgentCli {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record running agents' pids under `dir` and kill any left behind by a
    /// previous run (see `process::reap_orphans`). The reap itself runs in a
    /// spawned task — `ps`/`kill` are still a process spawn + wait each, and
    /// this must not block whatever runtime is starting Theodore up.
    pub fn with_pid_dir(mut self, dir: std::path::PathBuf) -> Self {
        let reap_dir = dir.clone();
        tokio::spawn(async move {
            let n = process::reap_orphans(&reap_dir).await;
            if n > 0 {
                tracing::warn!(n, "killed agent processes left over from a previous run");
            }
        });
        self.pid_dir = Some(dir);
        self
    }

    pub fn with_config(mut self, id: AgentId, cfg: AgentBinaryConfig) -> Self {
        self.configs.set(id, cfg);
        self
    }
}

impl AgentCli {
    /// One CLI process for `req`; `steerable` (Claude only) keeps its stdin
    /// open for steered messages.
    async fn spawn(
        &self,
        req: AgentRequest,
        steerable: bool,
    ) -> Result<(AgentRun, Option<Steer>), AgentError> {
        let cfg = self.configs.get(req.agent);
        let program =
            detect::locate_binary(req.agent, &cfg).ok_or(AgentError::NotInstalled(req.agent))?;
        let mut child_env = env::filtered_env();
        child_env.extend(req.env.iter().cloned());
        let mut guards: Vec<Box<dyn std::any::Any + Send>> = Vec::new();

        let args = match req.agent {
            AgentId::Claude => {
                // Written to a 0600 temp file, not argv: `--mcp-config`'s
                // JSON carries the review-tools bearer token, and argv is
                // visible to any process on the host via `ps`/`/proc`.
                let mcp_guard =
                    claude::write_mcp_config(&req).map_err(|e| AgentError::Spawn(e.to_string()))?;
                let mcp_path = mcp_guard.as_ref().map(|f| f.path().to_path_buf());
                if let Some(g) = mcp_guard {
                    guards.push(Box::new(g));
                }
                if steerable {
                    claude::build_steerable_args(&req, &cfg, mcp_path.as_deref())
                } else {
                    claude::build_args(&req, &cfg, mcp_path.as_deref())
                }
            }
            AgentId::Codex => {
                if let Some(mcp) = &req.mcp {
                    child_env.push((codex::MCP_TOKEN_ENV_VAR.to_string(), mcp.token.clone()));
                }
                codex::build_args(&req, &cfg)
            }
            AgentId::Gemini => {
                let (extra_env, guard) =
                    gemini::settings_env(&req).map_err(|e| AgentError::Spawn(e.to_string()))?;
                child_env.extend(extra_env);
                guards.push(Box::new(guard));
                gemini::build_args(&req, &cfg)
            }
        };

        let parser: Box<dyn process::LineParser> = match req.agent {
            AgentId::Claude if steerable => Box::new(claude::ClaudeParser::with_stdin_input()),
            AgentId::Claude => Box::new(claude::ClaudeParser::new()),
            AgentId::Codex => Box::new(codex::CodexParser::new()),
            AgentId::Gemini => Box::new(gemini::GeminiParser::new(req.stream_text)),
        };

        let spec = process::SpawnSpec {
            program,
            args,
            cwd: req.cwd.clone(),
            env: child_env,
            stall_after_ms: req.stall_after_ms,
            max_runtime_ms: process::DEFAULT_MAX_RUNTIME_MS,
            guards,
            pid_dir: self.pid_dir.clone(),
            agent: req.agent.as_str().to_string(),
            version: self
                .versions
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&req.agent)
                .cloned(),
        };
        if !steerable {
            return process::spawn_and_stream(spec, parser).map(|run| (run, None));
        }
        let (steer, messages) = Steer::channel();
        let input = process::Input {
            first: claude::user_message_line(&req.prompt),
            messages,
            encode: claude::user_message_line,
        };
        process::spawn_with_input(spec, parser, input).map(|run| (run, Some(steer)))
    }
}

#[async_trait]
impl AgentPort for AgentCli {
    async fn detect(&self) -> Vec<AgentInfo> {
        let mut out = Vec::with_capacity(AgentId::ALL.len());
        for id in AgentId::ALL {
            let cfg = self.configs.get(id);
            let info = detect::detect_one(id, &cfg).await;
            if let Some(v) = &info.version {
                self.versions
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(id, v.clone());
            }
            out.push(info);
        }
        out
    }

    async fn run(&self, req: AgentRequest) -> Result<AgentRun, AgentError> {
        self.spawn(req, false).await.map(|(run, _)| run)
    }

    /// Claude Code reads its messages from stdin while it runs; Codex and
    /// Gemini take one prompt per process, so theirs is a plain run.
    async fn run_steerable(
        &self,
        req: AgentRequest,
    ) -> Result<(AgentRun, Option<Steer>), AgentError> {
        let steerable = req.agent == AgentId::Claude;
        self.spawn(req, steerable).await
    }

    async fn models(&self, agent: AgentId) -> Vec<ModelInfo> {
        match agent {
            AgentId::Claude => models::claude_models(),
            AgentId::Codex => {
                let mut cache = self.codex_models.lock().await;
                let now = std::time::Instant::now();
                if let Some(list) = cache.cached(now) {
                    return list;
                }
                let cfg = self.configs.get(AgentId::Codex);
                let outcome = match detect::locate_binary(AgentId::Codex, &cfg) {
                    None => Err(CodexListError::NotInstalled),
                    Some(program) => models::codex_models(&program)
                        .await
                        .map_err(CodexListError::Failed),
                };
                let (list, log) = cache.record(now, outcome);
                if let (Some(level), Some((_, e))) = (log, &cache.failed) {
                    match (level, e) {
                        (ListLog::Warn, CodexListError::Failed(reason)) => {
                            tracing::warn!(reason = %reason, "couldn't list codex models");
                        }
                        (_, e) => tracing::debug!(reason = ?e, "no codex models"),
                    }
                }
                list
            }
            AgentId::Gemini => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn model(id: &str) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            label: id.into(),
            description: None,
            efforts: vec![],
            default_effort: None,
            older: false,
        }
    }

    #[test]
    fn not_installed_is_debug_and_stands_for_a_while() {
        let mut cache = CodexModels::default();
        let t0 = Instant::now();
        assert_eq!(cache.cached(t0), None);
        let (list, log) = cache.record(t0, Err(CodexListError::NotInstalled));
        assert!(list.is_empty());
        assert_eq!(log, Some(ListLog::Debug));
        assert_eq!(cache.cached(t0 + Duration::from_secs(60)), Some(vec![]));
        assert_eq!(cache.cached(t0 + CODEX_RETRY_AFTER), None, "tried again");
    }

    #[test]
    fn a_real_failure_warns_once_per_process() {
        let mut cache = CodexModels::default();
        let t0 = Instant::now();
        let fail = || Err(CodexListError::Failed("exited with 1".into()));
        assert_eq!(cache.record(t0, fail()).1, Some(ListLog::Warn));
        let later = t0 + CODEX_RETRY_AFTER;
        assert_eq!(cache.cached(later), None);
        assert_eq!(cache.record(later, fail()).1, Some(ListLog::Debug));
    }

    #[test]
    fn a_listing_is_kept_for_the_process() {
        let mut cache = CodexModels::default();
        let t0 = Instant::now();
        cache.record(t0, Err(CodexListError::NotInstalled));
        let (list, log) = cache.record(t0 + CODEX_RETRY_AFTER, Ok(vec![model("m")]));
        assert_eq!((list.len(), log), (1, None));
        let much_later = t0 + CODEX_RETRY_AFTER * 10;
        assert_eq!(cache.cached(much_later), Some(vec![model("m")]));
    }

    #[tokio::test]
    async fn a_missing_codex_is_not_looked_for_again_on_every_call() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex");
        let cli = AgentCli::new().with_config(
            AgentId::Codex,
            AgentBinaryConfig {
                path: Some(path.clone()),
                ..Default::default()
            },
        );
        assert!(cli.models(AgentId::Codex).await.is_empty());

        // Installed now, but the "not installed" answer still stands.
        std::fs::write(
            &path,
            "#!/bin/sh\necho '{\"models\":[{\"slug\":\"m\",\"visibility\":\"list\"}]}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(cli.models(AgentId::Codex).await.is_empty());
        let cache = cli.codex_models.lock().await;
        assert!(matches!(
            cache.failed,
            Some((_, CodexListError::NotInstalled))
        ));
    }
}
