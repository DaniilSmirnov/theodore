//! Composition root (plan §2.1): the only place that knows every adapter.
//! The desktop shell and the CLI both start the core through here.

mod grouper;
mod hosts;
pub mod identity;
mod local_host;
mod sandbox;
mod token;
mod tools;
mod worktree_gc;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use theodore_app::{Core, Deps, Receivers};
use theodore_ports::{
    AgentPort, Clock, DeviceLoginPort, GitPort, HostAdapter, IssueTracker, Secrets, Store,
    SystemClock,
};

pub use grouper::GroupingGrouper;
pub use hosts::Hosts;
pub use identity::BuildEnv;
pub use local_host::LocalHostAdapter;
pub use token::HostToken;
pub use tools::{CoreTools, McpRunTools, RootCodeTools};
pub use worktree_gc::{WorktreeGuard, WorktreeLocks};

/// How often the worktree GC sweep runs (`worktree_gc::spawn_periodic`).
/// Worktrees are cheap to keep around for a while and expensive to GC too
/// eagerly (a PR that flips closed→reopened would otherwise reclone), so
/// this errs slow.
const WORKTREE_GC_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// The Theodore account's WorkOS client id, baked in when the build ran
/// with `THEODORE_WORKOS_CLIENT_ID` set (`scripts/nightly-build.sh`, the
/// release workflow). The lowest-priority source: the same env var at run
/// time and the `account.workos.client_id` setting both win. A build made
/// without it (a plain `cargo run`) has no account, so no sign-in gate.
/// Ignored in fixture and sandbox modes, which have no account provider.
pub const BUILT_IN_CLIENT_ID: Option<&str> = option_env!("THEODORE_WORKOS_CLIENT_ID");

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// The build's own data dir by default (`identity::default_data_dir`):
    /// `~/.theodore` for production, `~/.theodore-<env>` for the others.
    pub data_dir: PathBuf,
    /// Run on fixture adapters from this directory instead of GitHub, git
    /// and real agents (dev and e2e, plan §12.1).
    pub fixtures: Option<PathBuf>,
    /// Run against the sandbox host built from this scenario directory
    /// (docs/sandbox.md): real git and agents, a simulated host.
    pub sandbox: Option<PathBuf>,
    /// Picks the keychain item the real adapters keep secrets in
    /// (`identity::keychain_service`); fixture and sandbox modes keep them
    /// in memory.
    pub build_env: BuildEnv,
}

impl RuntimeConfig {
    /// `THEODORE_SANDBOX` → sandbox mode; else `THEODORE_DATA_DIR`, else
    /// `env`'s own data dir. The CLI has no build env: it passes
    /// `Production` and shares the production app's data.
    pub fn from_env(env: BuildEnv) -> Self {
        if let Some(dir) = std::env::var_os("THEODORE_SANDBOX") {
            return Self::sandbox(PathBuf::from(dir), env);
        }
        let data_dir = std::env::var_os("THEODORE_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| identity::default_data_dir(env, dirs::home_dir().as_deref()));
        let fixtures = std::env::var_os("THEODORE_FIXTURES").map(PathBuf::from);
        Self {
            data_dir,
            fixtures,
            sandbox: None,
            build_env: env,
        }
    }

    /// Sandbox mode on `scenario`: `THEODORE_DATA_DIR` or
    /// `~/.theodore-sandbox`, never a build's own data dir (`start`
    /// refuses those).
    pub fn sandbox(scenario: PathBuf, env: BuildEnv) -> Self {
        let data_dir = std::env::var_os("THEODORE_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(sandbox::default_data_dir);
        Self {
            data_dir,
            fixtures: None,
            sandbox: Some(scenario),
            build_env: env,
        }
    }
}

pub use sandbox::default_data_dir as sandbox_data_dir;

pub struct Runtime {
    pub core: Core,
    pub receivers: Receivers,
    pub mcp: Arc<McpRunTools>,
    /// Fixture triggers (author push, checks green, agent crash) in fixture mode.
    pub triggers: Option<theodore_fixtures::Triggers>,
    /// The controlled clock in fixture mode, so tests can advance it past the
    /// poller's due time deterministically (`None` on the real clock).
    pub fixture_clock: Option<theodore_fixtures::ManualClock>,
    /// The fixture host adapter in fixture mode, so tests can assert on the
    /// calls it received (`None` on the real host).
    pub fixture_host: Option<Arc<theodore_fixtures::FixtureHost>>,
    /// Holds a worktree open against the periodic GC sweep
    /// (`worktree_gc::spawn_periodic`, started here) for as long as an
    /// agent run or similar needs it alive, even past its PR closing.
    pub worktree_locks: Arc<WorktreeLocks>,
    /// The sandbox in sandbox mode (status, faults, events); its ticker is
    /// already running.
    pub sandbox: Option<theodore_sandbox::Sandbox>,
    /// Posts "Report a problem" issues with the user's GitHub token; `None`
    /// in sandbox and fixture runs, which never reach real GitHub.
    pub issues: Option<Arc<dyn IssueTracker>>,
}

struct Adapters {
    host: Arc<dyn HostAdapter>,
    git: Arc<dyn GitPort>,
    agents: Arc<dyn AgentPort>,
    store: Arc<dyn Store>,
    secrets: Arc<dyn Secrets>,
    clock: Arc<dyn Clock>,
    triggers: Option<theodore_fixtures::Triggers>,
    fixture_clock: Option<theodore_fixtures::ManualClock>,
    fixture_host: Option<Arc<theodore_fixtures::FixtureHost>>,
    /// `None` in fixture mode: there's no keychain/`gh`/env token story to
    /// report (plan §6.8).
    token_status: Option<Arc<dyn theodore_app::TokenStatus>>,
    device_login: Arc<dyn DeviceLoginPort>,
    account: Arc<dyn theodore_ports::AccountPort>,
    /// Real GitHub only: sandbox and fixture runs can't post issues.
    issues: Option<Arc<dyn IssueTracker>>,
}

fn real_adapters(cfg: &RuntimeConfig) -> anyhow::Result<Adapters> {
    std::fs::create_dir_all(&cfg.data_dir)
        .with_context(|| format!("creating {}", cfg.data_dir.display()))?;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let secrets: Arc<dyn Secrets> = Arc::new(theodore_secrets::KeychainSecrets::with_service(
        identity::keychain_service(cfg.build_env),
    ));
    let token = Arc::new(HostToken::new(secrets.clone()));
    let github = Arc::new(theodore_host_github::GithubAdapter::new(
        token.clone(),
        clock.clone(),
    ));
    let gitlab = Arc::new(theodore_host_gitlab::GitlabAdapter::new(
        Arc::new(token::GitlabToken::new(secrets.clone())),
        clock.clone(),
    ));
    let bitbucket = Arc::new(theodore_host_bitbucket::BitbucketAdapter::new(
        Arc::new(token::BitbucketToken::new(secrets.clone())),
        clock.clone(),
    ));
    let git: Arc<dyn GitPort> = Arc::new(theodore_git::GitCli::new(
        cfg.data_dir.clone(),
        token.clone(),
    ));
    let store: Arc<dyn Store> = Arc::new(
        theodore_store::SqliteStore::open(cfg.data_dir.join("theodore.db"))
            .context("opening the database")?,
    );
    let local = Arc::new(local_host::LocalHostAdapter::new(
        store.clone(),
        git.clone(),
    ));
    let issues: Arc<dyn IssueTracker> = github.clone();
    let host = Arc::new(Hosts::new(github, Some(gitlab), Some(bitbucket), local));
    Ok(Adapters {
        issues: Some(issues),
        host,
        git,
        agents: Arc::new(
            theodore_agents::AgentCli::new().with_pid_dir(cfg.data_dir.join("agents")),
        ),
        store,
        secrets,
        clock,
        triggers: None,
        fixture_clock: None,
        fixture_host: None,
        token_status: Some(token as Arc<dyn theodore_app::TokenStatus>),
        device_login: Arc::new(theodore_host_github::GithubDeviceLogin::new()),
        account: workos_account(),
    })
}

fn fixture_adapters(dir: &std::path::Path) -> anyhow::Result<Adapters> {
    let (host, git, agents, clock, triggers) = theodore_fixtures::FixtureSet::load(dir)
        .with_context(|| format!("loading fixtures from {}", dir.display()))?;
    let host = Arc::new(host);
    Ok(Adapters {
        host: host.clone(),
        git: Arc::new(git),
        agents: Arc::new(agents),
        store: Arc::new(theodore_store::SqliteStore::open_in_memory()?),
        secrets: Arc::new(theodore_secrets::MemorySecrets::new()),
        clock: Arc::new(clock.clone()),
        triggers: Some(triggers),
        fixture_clock: Some(clock),
        fixture_host: Some(host),
        token_status: None,
        device_login: Arc::new(theodore_ports::NoDeviceLogin),
        account: Arc::new(theodore_ports::LocalAccount),
        issues: None,
    })
}

/// The Theodore account over WorkOS (plan "Theodore account"). Without a
/// client id configured the app hides the feature, so this is always
/// wired. `THEODORE_WORKOS_API_BASE` points it elsewhere (a staging proxy,
/// a mock); `THEODORE_WORKOS_ISSUER` accepts a custom auth domain's `iss`.
fn workos_account() -> Arc<dyn theodore_ports::AccountPort> {
    let base = std::env::var("THEODORE_WORKOS_API_BASE")
        .ok()
        .filter(|b| !b.trim().is_empty())
        .and_then(|b| {
            let b = if b.ends_with('/') { b } else { format!("{b}/") };
            url::Url::parse(&b).ok()
        });
    let mut account = match base {
        Some(b) => theodore_account_workos::WorkosAccount::with_api_base(b),
        None => theodore_account_workos::WorkosAccount::new(),
    };
    if let Ok(iss) = std::env::var("THEODORE_WORKOS_ISSUER")
        && !iss.trim().is_empty()
    {
        account = account.with_issuer(iss.trim());
    }
    Arc::new(account)
}

/// Code intelligence over `<data dir>/code-intel` (docs/code-intel.md).
/// Off in sandbox and fixture runs: they have no real worktrees to index.
/// A facade that fails to start leaves the feature off, never the app.
/// Started with the global config's machine-wide limits (one pool, one
/// index for the app); the core applies later edits of that file live.
async fn code_intel(cfg: &RuntimeConfig) -> Option<Arc<dyn theodore_ports::CodeIntelPort>> {
    if cfg.sandbox.is_some() || cfg.fixtures.is_some() {
        return None;
    }
    let limits = theodore_app::code_intel_limits(&cfg.data_dir).await;
    let options = theodore_code_intel::CodeIntelOptions::new(cfg.data_dir.join("code-intel"))
        .with_limits(limits);
    match theodore_code_intel::CodeIntel::new(options) {
        Ok(ci) => Some(Arc::new(ci)),
        Err(e) => {
            tracing::warn!(
                error.code = "code_intel.start_failed",
                error = %e,
                "code intelligence is off: it couldn't start"
            );
            None
        }
    }
}

/// Build every adapter, start the MCP server and the core.
pub async fn start(cfg: &RuntimeConfig) -> anyhow::Result<Runtime> {
    let (a, sandbox) = match (&cfg.sandbox, &cfg.fixtures) {
        (Some(_), Some(_)) => {
            anyhow::bail!("THEODORE_SANDBOX and THEODORE_FIXTURES can't be combined")
        }
        (Some(scenario), None) => {
            let (a, sb) = sandbox::adapters(&cfg.data_dir, scenario)?;
            (a, Some(sb))
        }
        (None, Some(dir)) => (fixture_adapters(dir)?, None),
        (None, None) => (real_adapters(cfg)?, None),
    };
    let backend = Arc::new(CoreTools::default());
    let mcp =
        Arc::new(McpRunTools::start(backend.clone(), backend.clone(), backend.clone()).await?);
    let claude_home = skills_dir(cfg.sandbox.is_some(), &cfg.data_dir, dirs::home_dir());

    let worktree_locks = WorktreeLocks::new();
    // Every run, grouping included, gets the per-agent model/effort settings.
    let agents: Arc<dyn theodore_ports::AgentPort> =
        Arc::new(theodore_app::AgentDefaults::new(a.agents, a.store.clone()));
    let deps = Deps {
        host: a.host,
        git: a.git.clone(),
        grouper: Arc::new(GroupingGrouper::new(agents.clone())),
        agents,
        store: a.store.clone(),
        secrets: a.secrets,
        clock: a.clock,
        tools: Some(mcp.clone()),
        token_status: a.token_status,
        device_login: a.device_login,
        account: a.account,
        account_client_id: BUILT_IN_CLIENT_ID.map(str::to_string),
        claude_home,
        commands: Arc::new(theodore_agents::ShellRunner::new()),
        terminals: Arc::new(theodore_agents::PtyRunner::new()),
        data_dir: cfg.data_dir.clone(),
        worktree_lease: Arc::new(worktree_gc::WorktreeLeaseAdapter::new(
            worktree_locks.clone(),
        )),
        code_intel: code_intel(cfg).await,
    };
    let (core, receivers) = Core::start(deps);
    if let Some(sb) = &sandbox {
        core.set_auto_skills(sb.auto_skills());
    }
    backend.attach(core.clone());
    core.ensure_default_settings().await?;

    // Detached on purpose: there's no runtime-wide shutdown signal to tie
    // this to yet (the mcp server above is the same), and a 30-minute
    // interval harmlessly outlives a short-lived process (tests, `theodore
    // group`) before ever firing.
    worktree_gc::spawn_periodic(
        a.git,
        a.store,
        worktree_locks.clone(),
        core.clone(),
        WORKTREE_GC_INTERVAL,
    );

    if let Some(sb) = &sandbox {
        sb.spawn();
        sandbox::seed_projects(&core, sb).await;
    }

    Ok(Runtime {
        core,
        receivers,
        mcp,
        triggers: a.triggers,
        fixture_clock: a.fixture_clock,
        fixture_host: a.fixture_host,
        worktree_locks,
        sandbox,
        issues: a.issues,
    })
}

/// Where "my skills" live. The sandbox keeps its own under its data dir:
/// creating or forking a skill there must never write into the user's real
/// `~/.claude/skills`.
fn skills_dir(sandbox: bool, data_dir: &Path, home: Option<PathBuf>) -> PathBuf {
    if sandbox {
        return data_dir.join("claude-skills");
    }
    home.map(|h| h.join(".claude/skills"))
        .unwrap_or_else(|| PathBuf::from(".claude/skills"))
}

#[cfg(test)]
mod skills_dir_tests {
    use super::*;

    #[test]
    fn the_sandbox_never_points_at_the_real_skills_folder() {
        let home = Some(PathBuf::from("/Users/me"));
        assert_eq!(
            skills_dir(true, Path::new("/data/sb"), home.clone()),
            PathBuf::from("/data/sb/claude-skills")
        );
        assert_eq!(
            skills_dir(false, Path::new("/data/real"), home),
            PathBuf::from("/Users/me/.claude/skills")
        );
    }
}
