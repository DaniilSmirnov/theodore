//! Injected dependencies (plan §2.1): `app` depends only on `domain` and
//! `ports`, adapters are wired in by whoever constructs `Deps` (the desktop
//! shell, the CLI, or a test's `testkit` fakes).

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use theodore_contract::TokenSource;
use theodore_domain::grouping::RepoContext;
use theodore_domain::{AgentId, Grouping, PrDetail, PrId, Revision};
use theodore_ports::{
    AccountPort, AgentPort, Clock, CodeIntelPort, CommandPort, DeviceLoginPort, GitPort,
    HostAdapter, McpEndpoint, Secrets, Store, TerminalPort, WorktreeLease,
};

use crate::error::GrouperError;

/// Which agent groups, and whether it may use the user's own MCP servers:
/// the `agents.grouping` and `agents.user_mcp` settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupingAgent {
    pub id: AgentId,
    pub user_mcp: bool,
}

/// Receives a grouping run's provisional groupings while it is in flight,
/// in order (streamed grouping, docs/eval/streamed-groups.md).
pub type OnPartial<'a> = &'a (dyn Fn(Grouping) + Send + Sync);

/// Groups a revision by meaning (plan §5). Implemented by the `grouping`
/// crate at integration time; `app` only needs the trait. `agent` comes
/// from the settings, read by the caller and passed in here so the
/// grouper never has to know about settings (plan §8.5). `ctx` is what the
/// app could read from the worktree and history (`Core::grouping_context`)
/// to help tell related files apart; possibly empty. `on_partial` gets the
/// groupings a streaming run publishes while the agent is still writing
/// (each `in_progress`); the run's result is still what `group` returns.
#[async_trait]
pub trait Grouper: Send + Sync {
    async fn group(
        &self,
        rev: &Revision,
        pr: &PrDetail,
        ctx: &RepoContext,
        worktree: &Path,
        agent: GroupingAgent,
        on_partial: OnPartial<'_>,
    ) -> Result<Grouping, GrouperError>;

    /// Re-group after a push: unchanged hunks keep their group (plan §5.3).
    async fn regroup(
        &self,
        prev: &Grouping,
        rev: &Revision,
        pr: &PrDetail,
        ctx: &RepoContext,
        worktree: &Path,
        agent: GroupingAgent,
    ) -> Result<Grouping, GrouperError>;
}

/// Which code-intelligence tools a run gets (docs/code-intel.md §7).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CodeTools {
    /// None listed: no port, or `code_intel.enabled: false`.
    #[default]
    Off,
    /// `find_symbol`, `get_symbol`, `references`, `definition`,
    /// `diagnostics`, `code_status`.
    Read,
    /// `Read` plus the edits, for a space turn that may write.
    Edit,
}

/// Per-run MCP tools for agents (plan §8.3). Implemented over the MCP
/// server by whoever wires `Deps`.
pub trait RunTools: Send + Sync {
    /// `review-tools` for a PR run. `space`: the space whose PR it is,
    /// which adds the read-only `read_logs` / `app_status`. `code`: the
    /// code tools it lists (never edits on a PR run).
    fn register(
        &self,
        run_id: &str,
        pr: &PrId,
        agent: AgentId,
        space: Option<&str>,
        code: CodeTools,
    ) -> McpEndpoint;
    /// `space-tools` for a space's agent turn (docs/spaces.md "Tools the
    /// agent gets").
    fn register_space(
        &self,
        run_id: &str,
        space: &str,
        agent: AgentId,
        code: CodeTools,
    ) -> McpEndpoint;
    fn unregister(&self, endpoint: &McpEndpoint);
}

/// Where the active host token came from (plan §6.8), and the ability to
/// drop a cached fallback (`gh`/env) lookup once the keychain token
/// changes. Implemented over `runtime::HostToken`; `app` stays ignorant of
/// keychains and the `gh` CLI.
pub trait TokenStatus: Send + Sync {
    fn source(&self) -> TokenSource;
    fn invalidate(&self);
}

#[derive(Clone)]
pub struct Deps {
    pub host: Arc<dyn HostAdapter>,
    pub git: Arc<dyn GitPort>,
    pub agents: Arc<dyn AgentPort>,
    pub store: Arc<dyn Store>,
    pub secrets: Arc<dyn Secrets>,
    pub clock: Arc<dyn Clock>,
    pub grouper: Arc<dyn Grouper>,
    /// GitHub's OAuth device flow (plan "login"). `NoDeviceLogin` when the
    /// shell has no client id story (fixtures, the CLI) — `login_start`
    /// then just reports `Unsupported`.
    pub device_login: Arc<dyn DeviceLoginPort>,
    /// `None` runs agents without MCP (tests, CLI).
    pub tools: Option<Arc<dyn RunTools>>,
    /// `None` when the shell doesn't wire one in (tests, the CLI): `viewer`
    /// and friends still work, `host_status` just reports `TokenSource::None`.
    pub token_status: Option<Arc<dyn TokenStatus>>,
    /// `~/.claude/skills` in production; whoever builds `Deps` resolves it
    /// (plan §8.4) — kept out of `app` itself so tests can point it at a
    /// throwaway directory instead of the real home directory.
    pub claude_home: std::path::PathBuf,
    /// Holds a PR's worktree open against the periodic GC sweep for the
    /// duration of an agent run (plan "worktree locks"). A no-op in tests
    /// and the CLI, where nothing sweeps worktrees.
    pub worktree_lease: Arc<dyn WorktreeLease>,
    /// The Theodore account provider (plan "Theodore account").
    /// `LocalAccount` where there is none (fixtures, sandbox, the CLI):
    /// the feature is then hidden.
    pub account: Arc<dyn AccountPort>,
    /// The account client id compiled into this build, if any: the lowest
    /// priority source, below the env and the setting
    /// (`usecases::account::resolve_client_id`). `None` in tests, the CLI
    /// and any build made without one.
    pub account_client_id: Option<String>,
    /// Runs a space's `setup` / `run` / `check` / `archive` commands
    /// (docs/config.md).
    pub commands: Arc<dyn CommandPort>,
    /// Runs a space's interactive terminals (docs/spaces.md "Terminal").
    pub terminals: Arc<dyn TerminalPort>,
    /// Theodore's data directory (`~/.theodore`): holds the global
    /// `config.yaml`. Tests point it at a throwaway directory.
    pub data_dir: std::path::PathBuf,
    /// Symbols, references and diagnostics over worktrees
    /// (docs/code-intel.md). `None` (tests, the CLI): the feature is off,
    /// runs get no code tools and `find_symbol` greps.
    pub code_intel: Option<Arc<dyn CodeIntelPort>>,
}
