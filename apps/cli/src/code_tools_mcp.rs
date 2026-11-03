//! `theodore code-tools-mcp`: the code tools agents get in the app
//! (docs/code-intel.md §7) as a standalone stdio MCP server over one
//! worktree, and the facade helpers the agent-tools eval shares with it.
//!
//! ```text
//! agent CLI ─stdio─▶ code-tools-mcp ─▶ ReviewToolsServer (ToolSet::Code) ─▶ RootCodeTools
//!                                                                          └─▶ CodeRoot ─▶ CodeIntel
//!                                                                              <data>/eval-tools/code-intel
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Serialize;
use theodore_code_intel::{CodeIntel, CodeIntelOptions};
use theodore_ports::{
    CodeIntelPort, InstallState, LanguageStatus, ServerState, TrustMode, Workspace,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum TrustArg {
    /// A space's trust: language servers run with the project's build
    /// scripts and proc macros (docs/code-intel.md §8).
    Trusted,
    /// A review run's: syntax and text only unless `code_intel.lsp.review`.
    Review,
}

impl From<TrustArg> for TrustMode {
    fn from(t: TrustArg) -> Self {
        match t {
            TrustArg::Trusted => TrustMode::Trusted,
            TrustArg::Review => TrustMode::Review,
        }
    }
}

#[derive(Debug, Clone, clap::Args)]
pub struct CodeToolsMcpArgs {
    /// The worktree the tools answer about (a git checkout).
    #[arg(long)]
    pub root: PathBuf,
    #[arg(long, value_enum, default_value = "trusted")]
    pub trust: TrustArg,
    /// Also the edits: replace_symbol_body, insert_after_symbol,
    /// rename_symbol. Trusted only.
    #[arg(long)]
    pub edit: bool,
    /// The code-intel cache (index, grammars, servers). Default:
    /// `<data dir>/eval-tools/code-intel`, shared by every run.
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
}

/// `<data dir>/eval-tools`: the CLI's own caches for the agent-tools eval
/// (repos, worktrees, the code-intel cache), apart from the app's.
pub fn eval_tools_dir() -> PathBuf {
    theodore_runtime::RuntimeConfig::from_env(theodore_runtime::BuildEnv::Production)
        .data_dir
        .join("eval-tools")
}

/// The code-intel cache `code-tools-mcp` and the eval use by default.
pub fn default_code_intel_dir() -> PathBuf {
    eval_tools_dir().join("code-intel")
}

/// A facade over `data_dir`, with the defaults of docs/code-intel.md §9
/// (language servers in spaces, not in reviews). Inside a tokio runtime.
pub fn facade(data_dir: &Path) -> Result<CodeIntel> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("creating {}", data_dir.display()))?;
    CodeIntel::new(CodeIntelOptions::new(data_dir)).context("starting code intelligence")
}

/// Opens `root` on `ci` the way the app opens a worktree; returns its
/// canonical path, the key every later call uses.
pub async fn open_root(ci: &CodeIntel, root: &Path, trust: TrustMode) -> Result<PathBuf> {
    let root = root
        .canonicalize()
        .with_context(|| format!("no worktree at {}", root.display()))?;
    ci.open(Workspace {
        repo_key: format!("local:{}", root.display()),
        root: root.clone(),
        trust,
        languages: vec![],
        focus_paths: vec![],
        lsp: None,
    })
    .await
    .with_context(|| format!("opening {}", root.display()))?;
    Ok(root)
}

/// The `code-tools` backend over an open root.
pub fn backend(ci: &CodeIntel, root: PathBuf) -> Arc<dyn theodore_mcp::CodeToolsBackend> {
    let port: Arc<dyn CodeIntelPort> = Arc::new(ci.clone());
    Arc::new(theodore_runtime::RootCodeTools::new(
        theodore_app::CodeRoot::new(port, root),
    ))
}

/// Serves the tools until stdin closes. Nothing else may write to stdout.
pub async fn run(args: &CodeToolsMcpArgs) -> Result<()> {
    if args.edit && args.trust == TrustArg::Review {
        anyhow::bail!("--edit needs --trust trusted: a review run is read-only");
    }
    let data = args.data_dir.clone().unwrap_or_else(default_code_intel_dir);
    let ci = facade(&data)?;
    let root = open_root(&ci, &args.root, args.trust.into()).await?;
    let run = theodore_mcp::code_run(theodore_domain::AgentId::Claude, args.edit);
    let served =
        theodore_mcp::serve_code_tools(backend(&ci, root), run, rmcp::transport::stdio()).await;
    ci.shutdown().await;
    served?;
    Ok(())
}

// ── warm-up ─────────────────────────────────────────────────────────────────

/// One language once warm-up ended.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WarmLanguage {
    pub language: String,
    pub files: usize,
    pub index_percent: u8,
    pub grammar: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_state: Option<String>,
}

/// How warming a worktree went.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Warm {
    pub secs: f64,
    /// Every language settled before the timeout.
    pub ready: bool,
    pub languages: Vec<WarmLanguage>,
}

/// Still settling: a grammar downloading or indexing, or a server on disk
/// warming up. Missing grammars are fetched on their own; a language with
/// none (or a server that failed, or isn't allowed) is settled as it is.
pub fn settled(l: &LanguageStatus) -> bool {
    let grammar = match &l.grammar {
        InstallState::Installed { .. } | InstallState::System { .. } => l.index_percent >= 100,
        InstallState::Missing | InstallState::Downloading => l.files == 0,
        InstallState::Unavailable | InstallState::Declined | InstallState::Failed { .. } => true,
    };
    let server = match &l.server {
        Some(s) => s.state != ServerState::Warming,
        None => true,
    };
    grammar && server
}

fn install_name(s: &InstallState) -> &'static str {
    match s {
        InstallState::Missing => "missing",
        InstallState::Downloading => "downloading",
        InstallState::System { .. } => "system",
        InstallState::Installed { .. } => "installed",
        InstallState::Unavailable => "unavailable",
        InstallState::Declined => "declined",
        InstallState::Failed { .. } => "failed",
    }
}

fn warm_language(l: &LanguageStatus) -> WarmLanguage {
    WarmLanguage {
        language: l.language.0.clone(),
        files: l.files,
        index_percent: l.index_percent,
        grammar: install_name(&l.grammar).into(),
        server: l.server.as_ref().map(|s| s.name.clone()),
        server_state: l.server.as_ref().map(|s| {
            match s.state {
                ServerState::NotRunning => "not_running",
                ServerState::Warming => "warming",
                ServerState::Ready => "ready",
                ServerState::Failed => "failed",
            }
            .into()
        }),
    }
}

/// Waits until every language of the open `root` is [`settled`], or
/// `timeout`. `install`: download the servers that are missing first (the
/// app asks; the eval opts in).
pub async fn warm(ci: &CodeIntel, root: &Path, timeout: Duration, install: bool) -> Result<Warm> {
    let started = Instant::now();
    if install {
        for l in ci.status(root).await? {
            let missing = l
                .server
                .as_ref()
                .is_some_and(|s| matches!(s.install, InstallState::Missing));
            if missing && let Err(e) = ci.install(&l.language, None).await {
                eprintln!("  install {}: {e}", l.language);
            }
        }
    }
    loop {
        let status = ci.status(root).await?;
        let ready = status.iter().all(settled);
        if ready || started.elapsed() >= timeout {
            return Ok(Warm {
                secs: started.elapsed().as_secs_f64(),
                ready,
                languages: status.iter().map(warm_language).collect(),
            });
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
