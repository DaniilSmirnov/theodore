//! Code intelligence above the port (docs/code-intel.md): which worktrees
//! are open on `CodeIntelPort`, under which trust, the install flow, the
//! MCP tools' calls (queries, and edits applied here), and project
//! settings → Languages.
//!
//! ```text
//! PR opened / review run ─▶ open(PR worktree, Review) ─┐
//! space created / turn ───▶ open(space worktree,       ├─▶ server missing? ─▶ install: always ─▶ install()
//!                           Trusted if its repo        │                     ├─ ask ─▶ CodeIntelSuggest (once)
//!                           commands are allowed)      │                     └─ never ─▶ nothing
//! archive / worktree gone ─▶ close                     ┘
//! edit tool ─▶ Ask mode? refuse ─▶ edits back to front ─▶ write ─▶ file_changed
//! ```
//!
//! `code_intel.enabled: false` or no port in `Deps` (tests, the CLI): no
//! workspace is opened and runs get no code tools.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use theodore_contract::{
    CodeHover, CodeHoverRequest, CodeInstall, CodeInstallItem, CodeInstallPolicy,
    CodeInstallProgress, CodeIntelLspRequest, CodeIntelStatus, CodeLanguage, CodeLsp, CodeLspScope,
    CodeLspToggle, CodeServer, CodeServerState, ConfigLayer, Ephemeral, Project, Setting, Space,
    SpaceMode, SpaceStatus, SymbolSource, models,
};
use theodore_domain::{PrId, RepoRef};
use theodore_ports::{
    Answer, CodeHit, CodeIntelError, CodeIntelLimits, CodeIntelPort, CodeSymbol, CodeSymbolKind,
    Diagnostic, InstallItem, InstallProgress, InstallProgressFn, InstallProgressState,
    InstallState, LanguageId, LanguageStatus, NewAction, Position, ServerState, Source,
    SyncActionCode, TextEdit, TrustMode, Workspace,
};

use crate::config::{
    CodeIntelConfig, DEFAULT_LSP_REVIEW, DEFAULT_LSP_SPACES, InstallPolicy, Merged,
};
use crate::core::{Core, RevisionEntry};
use crate::deps::CodeTools;
use crate::error::{AppError, AppResult};
use crate::settings::SCOPE_USER;

/// The setting holding the languages whose server you declined
/// (`code_intel.install: ask`).
pub(crate) const DECLINED_KEY: &str = "code_intel.declined";
/// The config keys of the two switches. Also the app-wide setting rows
/// the old Settings → Languages page wrote: still read, below the files,
/// so a switch flipped there keeps its value until a file sets the key.
pub(crate) const LSP_REVIEW_KEY: &str = "code_intel.lsp.review";
pub(crate) const LSP_SPACES_KEY: &str = "code_intel.lsp.spaces";

/// Where language servers may run (`code_intel.lsp`), resolved: the
/// project's local file (where its switches write), else your global
/// file, else an app-wide switch from before, else the default. The
/// repo's file never sets it (`config::repo_may_not_set`).
/// Handed to the port per workspace (`Workspace::lsp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspPolicy {
    /// In review runs: a PR's code may run.
    pub review: bool,
    pub spaces: bool,
}

/// Why a worktree is open on the port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenedFor {
    Review,
    Space,
    /// Only project settings → Languages (a local folder's checkout): closed
    /// before a Remove, reopened by the next status.
    Settings,
}

impl OpenedFor {
    /// Whether language servers may serve it. An untrusted space is served
    /// like a review, on top of the spaces switch.
    fn lsp(self, trust: TrustMode, policy: LspPolicy) -> bool {
        match (self, trust) {
            (Self::Space, TrustMode::Trusted) => policy.spaces,
            (Self::Space, TrustMode::Review) => policy.spaces && policy.review,
            (_, TrustMode::Trusted) => policy.spaces,
            (_, TrustMode::Review) => policy.review,
        }
    }
}

/// One worktree open on the port.
#[derive(Debug, Clone, PartialEq)]
struct Opened {
    trust: TrustMode,
    project: Option<String>,
    /// Language servers may serve it ([`OpenedFor::lsp`]).
    lsp: bool,
    purpose: OpenedFor,
}

/// In-memory: what is open, which languages were suggested this session,
/// and the limits last handed to the port.
#[derive(Default)]
pub(crate) struct CodeIntelState {
    open: std::sync::Mutex<HashMap<PathBuf, Opened>>,
    suggested: std::sync::Mutex<HashSet<String>>,
    limits: std::sync::Mutex<Option<CodeIntelLimits>>,
    /// One read-modify-write of the declined list at a time: "Not now"
    /// declines several languages at once.
    declined: tokio::sync::Mutex<()>,
}

/// Who a code tool call is for: the root comes from the run, never from
/// the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeScope {
    /// A review run (skill, chat, fix): the PR's worktree, read-only tools.
    Pr(PrId),
    /// A space's agent turn: the space's worktree.
    Space(String),
}

/// What an edit tool changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeEdits {
    /// Repo-relative, sorted.
    pub files: Vec<String>,
    pub edits: usize,
}

/// The system prompt's line about the code tools a run got, if any.
pub fn code_prompt(code: CodeTools) -> Option<&'static str> {
    match code {
        CodeTools::Off => None,
        CodeTools::Read => Some(
            "For code navigation prefer the symbol tools: find_symbol, get_symbol (one \
             symbol's source, not the whole file), definition, references and diagnostics. \
             Keep Grep for strings, config, comments and error texts.",
        ),
        CodeTools::Edit => Some(
            "For code navigation prefer the symbol tools: find_symbol, get_symbol (one \
             symbol's source, not the whole file), definition, references and diagnostics; \
             to change code by symbol use replace_symbol_body, insert_after_symbol and \
             rename_symbol, and diagnostics after an edit. Keep Grep for strings, config, \
             comments and error texts.",
        ),
    }
}

fn code_err(e: CodeIntelError) -> AppError {
    match e {
        CodeIntelError::FileNotFound(_) | CodeIntelError::NoSymbol { .. } => {
            AppError::NotFound(e.to_string())
        }
        other => AppError::Invalid(other.to_string()),
    }
}

fn off() -> AppError {
    AppError::Invalid("code intelligence is off for this project".into())
}

/// `github:owner/name`: worktrees of one repo share a cache.
pub(crate) fn repo_key(repo: &RepoRef) -> String {
    let host = serde_json::to_value(repo.host)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "host".into());
    format!("{host}:{}", repo.slug())
}

fn languages(cfg: &CodeIntelConfig) -> Vec<LanguageId> {
    cfg.language_ids().into_iter().map(LanguageId).collect()
}

/// A repo-relative path from an agent: no absolute path, no `..`.
fn relative(path: &str) -> AppResult<&str> {
    let p = Path::new(path);
    if path.is_empty()
        || p.is_absolute()
        || p.components().any(|c| {
            !matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
    {
        return Err(AppError::Invalid(format!(
            "path must be relative to the worktree: {path}"
        )));
    }
    Ok(path)
}

impl Core {
    fn code_port(&self) -> Option<Arc<dyn CodeIntelPort>> {
        self.0.deps.code_intel.clone()
    }

    /// The project of `repo`, if one is added.
    async fn project_of_repo(&self, repo: &RepoRef) -> Option<Project> {
        let store = self.0.deps.store.clone();
        tokio::task::spawn_blocking(move || store.all(models::PROJECT))
            .await
            .ok()?
            .ok()?
            .into_iter()
            .filter_map(|r| serde_json::from_value::<Project>(r.data).ok())
            .find(|p| &p.repo == repo)
    }

    /// `code_intel:` of an open review: the repo layer at the base.
    async fn review_code_config(&self, entry: &RevisionEntry) -> CodeIntelConfig {
        let layers = self.pr_config_layers(entry.config_source()).await;
        self.merge_cached(&layers).config.code_intel.clone()
    }

    /// `cfg.lsp` (the merged files) with the old app-wide switches and the
    /// defaults under it.
    pub(crate) async fn lsp_policy(&self, cfg: &CodeIntelConfig) -> LspPolicy {
        let setting = |key: &'static str| async move {
            self.setting_value_scoped(key, None)
                .await
                .and_then(|v| v.as_bool())
        };
        LspPolicy {
            review: match cfg.lsp.review {
                Some(on) => on,
                None => setting(LSP_REVIEW_KEY).await.unwrap_or(DEFAULT_LSP_REVIEW),
            },
            spaces: match cfg.lsp.spaces {
                Some(on) => on,
                None => setting(LSP_SPACES_KEY).await.unwrap_or(DEFAULT_LSP_SPACES),
            },
        }
    }

    /// Opens an open PR's worktree in `TrustMode::Review`. `Read` when its
    /// tools are on.
    pub(crate) async fn open_review_code(&self, entry: &RevisionEntry) -> CodeTools {
        if self.code_port().is_none() {
            return CodeTools::Off;
        }
        let cfg = self.review_code_config(entry).await;
        if !cfg.enabled {
            return CodeTools::Off;
        }
        let project = self.project_of_repo(&entry.pr_ref.repo).await.map(|p| p.id);
        let lsp = OpenedFor::Review.lsp(TrustMode::Review, self.lsp_policy(&cfg).await);
        let ws = Workspace {
            repo_key: repo_key(&entry.pr_ref.repo),
            root: entry.worktree_path.clone(),
            trust: TrustMode::Review,
            languages: languages(&cfg),
            focus_paths: entry
                .revision
                .files
                .iter()
                .map(|f| f.path.clone())
                .collect(),
            lsp: None,
            servers: cfg.servers.clone(),
        };
        if self
            .open_code_workspace(ws, lsp, cfg.install, project, OpenedFor::Review)
            .await
        {
            CodeTools::Read
        } else {
            CodeTools::Off
        }
    }

    /// The code tools of a review run on `pr` (opening its worktree).
    pub(crate) async fn review_code_tools(&self, pr: &PrId) -> CodeTools {
        match self.open_entry(pr).await {
            Some(entry) => self.open_review_code(&entry).await,
            None => CodeTools::Off,
        }
    }

    /// Opens a space's worktree: `Trusted` when nothing from the repo
    /// layer waits for approval (its commands allowed, or none), `Review`
    /// otherwise (docs/code-intel.md §8). `false` when off.
    pub(crate) async fn open_space_code(&self, space: &Space, project: &Project) -> bool {
        if self.code_port().is_none() || space.status == SpaceStatus::Archived {
            return false;
        }
        let cfg = self.space_config_of(space, project).await;
        let code = &cfg.merged.config.code_intel;
        if !code.enabled {
            return false;
        }
        let trusted = !cfg.denied && cfg.repo.as_ref().is_none_or(|r| r.trusted);
        let trust = if trusted {
            TrustMode::Trusted
        } else {
            TrustMode::Review
        };
        let lsp = OpenedFor::Space.lsp(trust, self.lsp_policy(code).await);
        let ws = Workspace {
            repo_key: repo_key(&project.repo),
            root: PathBuf::from(&space.worktree),
            trust,
            languages: languages(code),
            focus_paths: Vec::new(),
            lsp: None,
            servers: code.servers.clone(),
        };
        self.open_code_workspace(
            ws,
            lsp,
            code.install,
            Some(project.id.clone()),
            OpenedFor::Space,
        )
        .await
    }

    /// `open_space_code` in the background: a space just created or
    /// unarchived starts indexing before its first turn.
    pub(crate) fn spawn_space_code(&self, space: &Space, project: &Project) {
        if self.code_port().is_none() {
            return;
        }
        let (core, space, project) = (self.clone(), space.clone(), project.clone());
        tokio::spawn(async move {
            core.open_space_code(&space, &project).await;
        });
    }

    /// The code tools of a space turn in `mode`: none in Ask mode's edits.
    pub(crate) async fn space_code_tools(
        &self,
        space: &Space,
        project: &Project,
        mode: SpaceMode,
    ) -> CodeTools {
        if !self.open_space_code(space, project).await {
            return CodeTools::Off;
        }
        match mode {
            SpaceMode::Ask | SpaceMode::Plan => CodeTools::Read,
            SpaceMode::Edit | SpaceMode::Run => CodeTools::Edit,
        }
    }

    /// `open` on the port unless `ws.root` is open already under the same
    /// trust and LSP policy (a change reopens it), then the install flow in
    /// the background. Already open: its servers come back if they went idle.
    /// `false` when the port refused.
    async fn open_code_workspace(
        &self,
        ws: Workspace,
        lsp: bool,
        install: InstallPolicy,
        project: Option<String>,
        purpose: OpenedFor,
    ) -> bool {
        let Some(port) = self.code_port() else {
            return false;
        };
        // A hand edit of the global file lands here at the latest.
        self.apply_code_limits().await;
        let want = Opened {
            trust: ws.trust,
            project: project.clone(),
            lsp,
            purpose,
        };
        let before = self
            .0
            .code_intel
            .open
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&ws.root)
            .cloned();
        match before {
            Some(o) if o.trust == want.trust && o.lsp == want.lsp => {
                let root = ws.root;
                tokio::spawn(async move { port.warm(&root).await });
                return true;
            }
            Some(_) => port.close(&ws.root).await,
            None => {}
        }
        let root = ws.root.clone();
        // The decision is made here, per project; the facade only applies
        // it to this workspace's trust.
        let ws = Workspace {
            lsp: Some(theodore_ports::LspPolicy {
                review: lsp,
                spaces: lsp,
            }),
            ..ws
        };
        if let Err(e) = port.open(ws).await {
            tracing::warn!(
                error.code = "code_intel.open_failed",
                error = %e,
                "code intelligence couldn't open a worktree"
            );
            self.forget_code_root(&root);
            return false;
        }
        self.0
            .code_intel
            .open
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(root.clone(), want);
        let core = self.clone();
        tokio::spawn(async move { core.code_install_flow(&root, install, project).await });
        true
    }

    /// A space's worktree moved from commit `before` to `after` — a turn
    /// (the files the agent wrote with its own tools), Undo, Back to here,
    /// Revert, Update from base and its Abort, Continue: the files that
    /// differ are reindexed, in the background.
    pub(crate) fn spawn_space_code_changed(
        &self,
        root: PathBuf,
        before: theodore_domain::Sha,
        after: theodore_domain::Sha,
    ) {
        let Some(port) = self.code_port() else {
            return;
        };
        if before == after {
            return;
        }
        let open = self
            .0
            .code_intel
            .open
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(&root);
        if !open {
            return;
        }
        let git = self.0.deps.git.clone();
        tokio::spawn(async move {
            let opts = theodore_ports::DiffOptions {
                context: 0,
                max_file_lines: 0,
                ..Default::default()
            };
            let rev = match git.diff(&root, &before, &after, &opts).await {
                Ok(rev) => rev,
                Err(e) => {
                    tracing::warn!(error = %e, "code intelligence: no diff of the turn");
                    return;
                }
            };
            for f in &rev.files {
                for path in std::iter::once(&f.path).chain(f.old_path.as_ref()) {
                    port.file_changed(&root, path).await;
                }
            }
        });
    }

    fn forget_code_root(&self, root: &Path) -> bool {
        self.0
            .code_intel
            .open
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(root)
            .is_some()
    }

    /// §5.1: for each language whose server is missing, `always` installs
    /// it, `ask` suggests it once per session unless you declined it,
    /// `never` leaves it.
    pub(crate) async fn code_install_flow(
        &self,
        root: &Path,
        install: InstallPolicy,
        project: Option<String>,
    ) {
        let Some(port) = self.code_port() else {
            return;
        };
        if install == InstallPolicy::Never {
            return;
        }
        let Ok(langs) = port.status(root).await else {
            return;
        };
        let declined = self.declined_languages().await;
        for l in langs {
            let Some(server) = l.server.filter(|s| s.install == InstallState::Missing) else {
                continue;
            };
            match install {
                // A language you removed stays removed until Install.
                InstallPolicy::Always if declined.iter().any(|d| d == l.language.as_str()) => {}
                InstallPolicy::Always => {
                    let progress = self.install_progress(project.clone());
                    if let Err(e) = port.install(&l.language, Some(progress)).await {
                        tracing::warn!(
                            error.code = "code_intel.install_failed",
                            language = %l.language,
                            error = %e,
                            "language server download failed"
                        );
                    }
                }
                InstallPolicy::Ask => {
                    let lang = l.language.as_str().to_string();
                    if declined.contains(&lang) {
                        continue;
                    }
                    let fresh = self
                        .0
                        .code_intel
                        .suggested
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(lang.clone());
                    if fresh {
                        self.emit(Ephemeral::CodeIntelSuggest {
                            project: project.clone(),
                            language: lang,
                            server: server.name,
                        });
                    }
                }
                InstallPolicy::Never => {}
            }
        }
    }

    /// Stops serving `root` (a space archived, a worktree removed).
    pub(crate) async fn close_code_root(&self, root: &Path) {
        if self.forget_code_root(root)
            && let Some(port) = self.code_port()
        {
            port.close(root).await;
        }
    }

    /// Closes every open worktree that is gone from disk: the periodic
    /// worktree GC (runtime) calls this after each sweep.
    pub async fn code_intel_sweep(&self) {
        let gone: Vec<PathBuf> = self
            .0
            .code_intel
            .open
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .filter(|root| !root.exists())
            .cloned()
            .collect();
        for root in gone {
            self.close_code_root(&root).await;
        }
    }

    async fn declined_languages(&self) -> Vec<String> {
        self.setting_value_scoped(DECLINED_KEY, None)
            .await
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }

    // ── project settings → Languages ────────────────────────────────────

    /// Each language of `project`, over every worktree of it open, else (a
    /// local folder) the folder itself, read-only. Reading it starts what
    /// the policy allows: a server stopped for being idle shows as warming,
    /// not as dead.
    ///
    /// ```text
    /// open roots ─▶ warm ─▶ status ─┐ per language: the most alive server,
    ///                  …            ┴▶ the largest count of files
    /// ```
    pub async fn code_intel_status(&self, project: &str) -> AppResult<CodeIntelStatus> {
        let p = self
            .project_row(project)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("project {project}")))?;
        let layers = self.project_layers(&p, None).await;
        let merged = self.merge_cached(&layers);
        let cfg = merged.config.code_intel.clone();
        let policy = self.lsp_policy(&cfg).await;
        let lsp = CodeLsp {
            review: toggle(policy.review, &merged, LSP_REVIEW_KEY),
            spaces: toggle(policy.spaces, &merged, LSP_SPACES_KEY),
        };
        let install = match cfg.install {
            InstallPolicy::Ask => CodeInstallPolicy::Ask,
            InstallPolicy::Always => CodeInstallPolicy::Always,
            InstallPolicy::Never => CodeInstallPolicy::Never,
        };
        let (Some(port), true) = (self.code_port(), cfg.enabled) else {
            return Ok(CodeIntelStatus {
                enabled: false,
                install,
                lsp,
                languages: Vec::new(),
            });
        };
        let mut roots: Vec<PathBuf> = self
            .0
            .code_intel
            .open
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|(_, o)| o.project.as_deref() == Some(project))
            .map(|(r, _)| r.clone())
            .collect();
        roots.sort();
        if roots.is_empty()
            && let Some(folder) = &p.path
        {
            let ws = Workspace {
                repo_key: repo_key(&p.repo),
                root: PathBuf::from(folder),
                trust: TrustMode::Review,
                languages: languages(&cfg),
                focus_paths: Vec::new(),
                lsp: None,
                servers: cfg.servers.clone(),
            };
            if self
                .open_code_workspace(
                    ws,
                    policy.review,
                    cfg.install,
                    Some(p.id.clone()),
                    OpenedFor::Settings,
                )
                .await
            {
                roots.push(PathBuf::from(folder));
            }
        }
        let mut languages = Vec::new();
        if !roots.is_empty() {
            let mut merged: Vec<LanguageStatus> = Vec::new();
            let mut last_error = None;
            let mut answered = false;
            for root in &roots {
                port.warm(root).await;
                match port.status(root).await {
                    Ok(statuses) => {
                        answered = true;
                        merge_statuses(&mut merged, statuses);
                    }
                    Err(e) => last_error = Some(e),
                }
            }
            if !answered && let Some(e) = last_error {
                return Err(code_err(e));
            }
            merged.sort_by(|a, b| {
                b.files
                    .cmp(&a.files)
                    .then_with(|| a.language.cmp(&b.language))
            });
            let declined = self.declined_languages().await;
            languages = merged
                .into_iter()
                .map(|l| language_view(l, &declined))
                .collect();
        }
        Ok(CodeIntelStatus {
            enabled: true,
            install,
            lsp,
            languages,
        })
    }

    /// Project settings → Languages: one switch of one project, written as
    /// `code_intel.lsp.<scope>` into its local file — the folder's
    /// `.theodore/config.local.yaml` (gitignored), or a host project's in
    /// the data directory. Never the repo's file (docs/code-intel.md §8).
    /// The local layer wins over the global file, which stays the
    /// default under it. Worktrees of the project open now follow at once.
    ///
    /// ```text
    /// switch ─▶ the folder commits its local file? ─▶ refused: set it in the global file
    ///    └─▶ local file: line edit (comments kept) ─▶ checked ─▶ written
    ///              │       └─ a shape a line edit can't handle ─▶ refused, nothing written
    ///              └─▶ open worktrees whose answer changed ─▶ servers stop / start
    /// ```
    pub async fn code_intel_set_lsp(&self, req: CodeIntelLspRequest) -> AppResult<()> {
        let leaf = match req.scope {
            CodeLspScope::Review => "review",
            CodeLspScope::Spaces => "spaces",
        };
        // One writer at a time with the config editor's save.
        let _guard = self.0.ship_lock.lock().await;
        let project = self
            .project_row(&req.project)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("project {}", req.project)))?;
        let folder = project.path.as_deref().map(Path::new);
        if let Some(folder) = folder
            && self
                .0
                .deps
                .git
                .is_tracked(folder, crate::config::LOCAL_FILE)
                .await
                .unwrap_or(false)
        {
            // Read as the repo's file (`folder_local_layer`), whose `lsp`
            // is ignored: writing it would dirty the checkout for nothing.
            return Err(AppError::Invalid(format!(
                "this repo commits {}, so Theodore reads it as the repo's config and \
                 ignores code_intel.lsp there. Set code_intel.lsp.{leaf} in your global \
                 config ({}) instead",
                crate::config::LOCAL_FILE,
                crate::config::global_path(&self.0.deps.data_dir).display()
            )));
        }
        let path = match folder {
            Some(folder) => folder.join(crate::config::LOCAL_FILE),
            None => crate::config::host_local_path(&self.0.deps.data_dir, &project.repo),
        };
        let text = tokio::fs::read_to_string(&path).await.ok();
        let next = crate::config::editor::set_leaf(
            text.as_deref(),
            &["code_intel", "lsp", leaf],
            if req.on { "true" } else { "false" },
        )
        .map_err(|why| {
            AppError::Invalid(format!(
                "couldn't edit {}: {why}. Set code_intel.lsp.{leaf} there by hand",
                path.display()
            ))
        })?;
        crate::usecases::config_editor::write_file(&path, &next).await?;
        if let Some(folder) = folder
            && self.0.deps.git.is_repo(folder).await.unwrap_or(false)
        {
            // Not the repo's `.gitignore`: a switch must leave the checkout
            // clean, or mirroring and merging locally refuse it as dirty.
            self.0
                .deps
                .git
                .exclude_locally(folder, crate::config::LOCAL_FILE)
                .await?;
        }
        drop(_guard);
        self.apply_lsp_switch(&project).await;
        Ok(())
    }

    /// `project`'s open worktrees take its switches as they are now.
    async fn apply_lsp_switch(&self, project: &Project) {
        let Some(port) = self.code_port() else {
            return;
        };
        let layers = self.project_layers(project, None).await;
        let merged = self.merge_cached(&layers);
        let policy = self.lsp_policy(&merged.config.code_intel).await;
        let changed: Vec<(PathBuf, bool)> = self
            .0
            .code_intel
            .open
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter_mut()
            .filter(|(_, o)| o.project.as_deref() == Some(project.id.as_str()))
            .filter_map(|(root, o)| {
                let on = o.purpose.lsp(o.trust, policy);
                (on != o.lsp).then(|| {
                    o.lsp = on;
                    (root.clone(), on)
                })
            })
            .collect();
        for (root, on) in changed {
            port.set_lsp(&root, on).await;
        }
    }

    /// "Install": downloads `language`'s grammar and server, and forgets
    /// an earlier No.
    pub async fn code_intel_install(&self, language: &str) -> AppResult<()> {
        let port = self.code_port().ok_or_else(off)?;
        let language = language.trim();
        if language.is_empty() {
            return Err(AppError::Invalid("no language".into()));
        }
        self.edit_declined(|d| d.retain(|l| l != language)).await?;
        port.install(
            &LanguageId::new(language),
            Some(self.install_progress(None)),
        )
        .await
        .map_err(code_err)
    }

    /// The port's install progress as `Ephemeral::CodeIntelProgress`.
    fn install_progress(&self, project: Option<String>) -> InstallProgressFn {
        let core = self.clone();
        Arc::new(move |p: InstallProgress| {
            core.emit(Ephemeral::CodeIntelProgress {
                project: project.clone(),
                language: p.language.0,
                what: match p.what {
                    InstallItem::Grammar => CodeInstallItem::Grammar,
                    InstallItem::Server => CodeInstallItem::Server,
                },
                name: p.name,
                state: match p.state {
                    InstallProgressState::Downloading { received, total } => {
                        CodeInstallProgress::Downloading { received, total }
                    }
                    InstallProgressState::Installed => CodeInstallProgress::Installed,
                    InstallProgressState::Failed { error } => CodeInstallProgress::Failed { error },
                },
            });
        })
    }

    /// Project settings → Languages → Remove: deletes what was downloaded for
    /// `language` (its server, its grammar). Worktrees opened only for this
    /// page close first; a review or a space with the language open makes
    /// the port refuse. Afterwards it counts as declined: not suggested,
    /// not installed by `install: always`, until Install.
    pub async fn code_intel_remove(&self, language: &str) -> AppResult<()> {
        let port = self.code_port().ok_or_else(off)?;
        let language = language.trim();
        if language.is_empty() {
            return Err(AppError::Invalid("no language".into()));
        }
        let settings_only: Vec<PathBuf> = self
            .0
            .code_intel
            .open
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|(_, o)| o.purpose == OpenedFor::Settings)
            .map(|(r, _)| r.clone())
            .collect();
        for root in settings_only {
            self.close_code_root(&root).await;
        }
        match port.remove(&LanguageId::new(language)).await {
            Ok(()) => {}
            Err(CodeIntelError::InUse { language, roots }) => {
                return Err(AppError::Invalid(self.in_use(&language, &roots).await));
            }
            Err(e) => return Err(code_err(e)),
        }
        self.code_intel_decline(language).await
    }

    /// Why Remove was refused, by what the user knows: spaces by name,
    /// reviews counted. A worktree path may be another project's.
    async fn in_use(&self, language: &str, roots: &[PathBuf]) -> String {
        let store = self.0.deps.store.clone();
        let spaces: Vec<Space> = tokio::task::spawn_blocking(move || store.all(models::SPACE))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| serde_json::from_value::<Space>(r.data).ok())
            .collect();
        let mut parts: Vec<String> = Vec::new();
        let mut reviews = 0;
        for root in roots {
            match spaces
                .iter()
                .find(|s| same_path(Path::new(&s.worktree), root))
            {
                Some(s) => parts.push(format!("space “{}”", s.name)),
                None => reviews += 1,
            }
        }
        match reviews {
            0 => {}
            1 => parts.push("1 review".into()),
            n => parts.push(format!("{n} reviews")),
        }
        format!(
            "{language} is in use: a space or review is using it ({}); close them, or \
             remove it later",
            parts.join(" and ")
        )
    }

    /// Hover in a PR's diff (docs/code-intel.md §7.1): at the PR's current
    /// head worktree, like `find_symbol`. `None`: nothing to show, or code
    /// intelligence is off for the project.
    pub async fn code_intel_hover(&self, req: CodeHoverRequest) -> AppResult<Option<CodeHover>> {
        let Some(port) = self.code_port() else {
            return Ok(None);
        };
        let path = relative(&req.path)?.to_string();
        self.ensure_open(&req.pr).await?;
        let detail = self.load_pr_detail(&req.pr).await?;
        let head = self
            .0
            .current_head
            .read()
            .await
            .get(&req.pr)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("pr {}", req.pr)))?;
        let worktree = self.0.deps.git.worktree(&detail.summary.pr, &head).await?;
        if self.review_code_tools(&req.pr).await == CodeTools::Off {
            return Ok(None);
        }
        let at = Position {
            line: req.line.max(1),
            col: req.col.max(1),
        };
        let hover = port
            .hover(&worktree.path, &path, at)
            .await
            .map_err(code_err)?;
        Ok(hover.map(|h| CodeHover {
            text: h.text,
            source: match h.source {
                Source::Lsp => SymbolSource::Lsp,
                Source::Syntax => SymbolSource::Syntax,
                Source::Text => SymbolSource::Text,
            },
        }))
    }

    /// Hands the global file's machine-wide limits (`config::MACHINE_WIDE`)
    /// to the port when they changed since the last call. Called on every
    /// worktree open and after a config save, so a hand edit of the file
    /// applies by the next review or space at the latest.
    pub async fn apply_code_limits(&self) {
        let Some(port) = self.code_port() else {
            return;
        };
        let layers = crate::config::read_layers(&self.0.deps.data_dir, None, None).await;
        let limits = self.merge_cached(&layers).config.code_intel.limits();
        let changed = {
            let mut last = self
                .0
                .code_intel
                .limits
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let changed = *last != Some(limits);
            *last = Some(limits);
            changed
        };
        if changed {
            port.set_limits(limits).await;
        }
    }

    /// "No": `language`'s server isn't suggested again.
    pub async fn code_intel_decline(&self, language: &str) -> AppResult<()> {
        let language = language.trim();
        if language.is_empty() {
            return Err(AppError::Invalid("no language".into()));
        }
        self.edit_declined(|d| {
            if !d.iter().any(|l| l == language) {
                d.push(language.to_string());
                d.sort();
            }
        })
        .await
    }

    /// Reads the declined list, applies `edit`, writes it if it changed:
    /// under a lock, so concurrent answers never overwrite each other.
    async fn edit_declined(&self, edit: impl FnOnce(&mut Vec<String>)) -> AppResult<()> {
        let _guard = self.0.code_intel.declined.lock().await;
        let before = self.declined_languages().await;
        let mut after = before.clone();
        edit(&mut after);
        if after == before {
            return Ok(());
        }
        self.write_user_setting(DECLINED_KEY, serde_json::json!(after))
            .await
    }

    async fn write_user_setting(&self, key: &str, value: serde_json::Value) -> AppResult<()> {
        let setting = Setting {
            key: key.into(),
            scope: SCOPE_USER.into(),
            value,
        };
        self.apply(vec![NewAction {
            model: models::SETTING.into(),
            model_id: format!("{SCOPE_USER}:{key}"),
            action: SyncActionCode::U,
            data: Some(
                serde_json::to_value(&setting).map_err(|e| AppError::Invalid(e.to_string()))?,
            ),
        }])
        .await?;
        Ok(())
    }

    // ── the MCP tools ───────────────────────────────────────────────────

    /// The worktree a run's code tools work on, opened if it isn't yet.
    async fn code_root(&self, scope: &CodeScope) -> AppResult<CodeRoot> {
        let port = self.code_port().ok_or_else(off)?;
        match scope {
            CodeScope::Pr(pr) => {
                let entry = self
                    .open_entry(pr)
                    .await
                    .ok_or_else(|| AppError::NotFound(format!("pr {pr} is not open")))?;
                if self.open_review_code(&entry).await == CodeTools::Off {
                    return Err(off());
                }
                Ok(CodeRoot::new(port, entry.worktree_path.clone()))
            }
            CodeScope::Space(id) => {
                let (space, project) = self.code_space(id).await?;
                if !self.open_space_code(&space, &project).await {
                    return Err(off());
                }
                Ok(CodeRoot::new(port, PathBuf::from(&space.worktree)))
            }
        }
    }

    async fn code_space(&self, id: &str) -> AppResult<(Space, Project)> {
        let space = self.space_row(id).await?;
        if space.status == SpaceStatus::Archived {
            return Err(AppError::Invalid("this space is archived".into()));
        }
        let project = self.space_project(&space).await?;
        Ok((space, project))
    }

    pub async fn code_find_symbol(
        &self,
        scope: &CodeScope,
        name: &str,
        kind: Option<CodeSymbolKind>,
    ) -> AppResult<Answer<CodeSymbol>> {
        self.code_root(scope).await?.find_symbol(name, kind).await
    }

    /// The symbol and its source text.
    pub async fn code_get_symbol(
        &self,
        scope: &CodeScope,
        path: &str,
        name: &str,
    ) -> AppResult<(CodeSymbol, String)> {
        self.code_root(scope).await?.get_symbol(path, name).await
    }

    pub async fn code_definition(
        &self,
        scope: &CodeScope,
        path: &str,
        at: Position,
    ) -> AppResult<Answer<CodeHit>> {
        self.code_root(scope).await?.definition(path, at).await
    }

    pub async fn code_references(
        &self,
        scope: &CodeScope,
        path: &str,
        at: Position,
    ) -> AppResult<Answer<CodeHit>> {
        self.code_root(scope).await?.references(path, at).await
    }

    pub async fn code_diagnostics(
        &self,
        scope: &CodeScope,
        path: Option<&str>,
    ) -> AppResult<Answer<Diagnostic>> {
        self.code_root(scope).await?.diagnostics(path).await
    }

    pub async fn code_status(&self, scope: &CodeScope) -> AppResult<Vec<LanguageStatus>> {
        self.code_root(scope).await?.status().await
    }

    /// The worktree of a space turn that may edit: refused in Ask and Plan mode
    /// (the space's `mode` is the current turn's) and on a review run.
    async fn edit_root(&self, scope: &CodeScope) -> AppResult<CodeRoot> {
        let CodeScope::Space(id) = scope else {
            return Err(AppError::Invalid(
                "code edits are for a space's agent turn only; a review run is read-only".into(),
            ));
        };
        let (space, _) = self.code_space(id).await?;
        if matches!(space.mode, SpaceMode::Ask | SpaceMode::Plan) {
            return Err(AppError::Invalid(format!(
                "this turn is in {} mode: code edits are off. Ask the user to switch to Edit",
                if space.mode == SpaceMode::Plan {
                    "Plan"
                } else {
                    "Ask"
                }
            )));
        }
        self.code_root(scope).await
    }

    /// `replace_symbol_body`: `name`'s whole definition in `path` becomes
    /// `body`.
    pub async fn code_replace_symbol(
        &self,
        scope: &CodeScope,
        path: &str,
        name: &str,
        body: &str,
    ) -> AppResult<CodeEdits> {
        self.edit_root(scope)
            .await?
            .replace_symbol(path, name, body)
            .await
    }

    /// `insert_after_symbol`: `text` on the line after `name`'s definition.
    pub async fn code_insert_after_symbol(
        &self,
        scope: &CodeScope,
        path: &str,
        name: &str,
        text: &str,
    ) -> AppResult<CodeEdits> {
        self.edit_root(scope)
            .await?
            .insert_after_symbol(path, name, text)
            .await
    }

    /// `rename_symbol`: the server's rename edits, in every file.
    pub async fn code_rename_symbol(
        &self,
        scope: &CodeScope,
        path: &str,
        at: Position,
        new_name: &str,
    ) -> AppResult<CodeEdits> {
        self.edit_root(scope)
            .await?
            .rename_symbol(path, at, new_name)
            .await
    }
}

/// One worktree's code tools over the port: the queries and the edits the
/// MCP tools make, on a root the caller decided (a run's PR or space here;
/// one fixed worktree in `theodore code-tools-mcp`). Paths are the
/// agent's, checked to stay relative; whether the caller may edit is the
/// caller's check.
#[derive(Clone)]
pub struct CodeRoot {
    port: Arc<dyn CodeIntelPort>,
    root: PathBuf,
}

impl CodeRoot {
    /// `root` must be open on `port` already.
    pub fn new(port: Arc<dyn CodeIntelPort>, root: PathBuf) -> Self {
        Self { port, root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn find_symbol(
        &self,
        name: &str,
        kind: Option<CodeSymbolKind>,
    ) -> AppResult<Answer<CodeSymbol>> {
        self.port
            .find_symbol(&self.root, name, kind)
            .await
            .map_err(code_err)
    }

    /// The symbol and its source text.
    pub async fn get_symbol(&self, path: &str, name: &str) -> AppResult<(CodeSymbol, String)> {
        self.port
            .symbol_body(&self.root, relative(path)?, name)
            .await
            .map_err(code_err)
    }

    pub async fn definition(&self, path: &str, at: Position) -> AppResult<Answer<CodeHit>> {
        self.port
            .definition(&self.root, relative(path)?, at)
            .await
            .map_err(code_err)
    }

    pub async fn references(&self, path: &str, at: Position) -> AppResult<Answer<CodeHit>> {
        self.port
            .references(&self.root, relative(path)?, at)
            .await
            .map_err(code_err)
    }

    pub async fn diagnostics(&self, path: Option<&str>) -> AppResult<Answer<Diagnostic>> {
        let path = path.map(relative).transpose()?;
        self.port
            .diagnostics(&self.root, path)
            .await
            .map_err(code_err)
    }

    pub async fn status(&self) -> AppResult<Vec<LanguageStatus>> {
        self.port.status(&self.root).await.map_err(code_err)
    }

    /// `name`'s whole definition in `path` becomes `body`.
    pub async fn replace_symbol(&self, path: &str, name: &str, body: &str) -> AppResult<CodeEdits> {
        let path = relative(path)?;
        let (symbol, text) = self
            .port
            .symbol_body(&self.root, path, name)
            .await
            .map_err(code_err)?;
        let (range, text) = with_modifiers(&self.root, path, symbol.range, text, body).await;
        let edit = TextEdit {
            path: path.to_string(),
            range,
            new_text: body.to_string(),
            expect: None,
        };
        let check = Check { range, text };
        apply_checked(&*self.port, &self.root, &[edit], Some(&check)).await
    }

    /// `text` on the line after `name`'s definition.
    pub async fn insert_after_symbol(
        &self,
        path: &str,
        name: &str,
        text: &str,
    ) -> AppResult<CodeEdits> {
        let path = relative(path)?;
        let (symbol, body) = self
            .port
            .symbol_body(&self.root, path, name)
            .await
            .map_err(code_err)?;
        let at = symbol.range.end;
        let edit = TextEdit {
            path: path.to_string(),
            range: theodore_ports::Range { start: at, end: at },
            new_text: format!("\n{}", text.trim_end_matches('\n')),
            expect: None,
        };
        let check = Check {
            range: symbol.range,
            text: body,
        };
        apply_checked(&*self.port, &self.root, &[edit], Some(&check)).await
    }

    /// The server's rename edits, in every file.
    pub async fn rename_symbol(
        &self,
        path: &str,
        at: Position,
        new_name: &str,
    ) -> AppResult<CodeEdits> {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            return Err(AppError::Invalid("the new name is empty".into()));
        }
        let edits = self
            .port
            .rename(&self.root, relative(path)?, at, new_name)
            .await
            .map_err(code_err)?;
        if edits.is_empty() {
            return Err(AppError::Invalid(
                "nothing to rename here (no language server answered)".into(),
            ));
        }
        apply_checked(&*self.port, &self.root, &edits, None).await
    }
}

/// A toggle's value and the global / local file that sets it, if one does.
fn toggle(on: bool, merged: &Merged, key: &str) -> CodeLspToggle {
    let from_config = merged.layer_of(key).and_then(|layer| {
        debug_assert_ne!(layer, ConfigLayer::Repo, "stripped by merge");
        merged
            .files
            .iter()
            .find(|f| f.layer == layer)
            .map(|f| f.path.clone())
    });
    CodeLspToggle { on, from_config }
}

fn install_view(s: InstallState, declined: bool) -> CodeInstall {
    match s {
        InstallState::Missing if declined => CodeInstall::Declined,
        InstallState::Missing => CodeInstall::Missing,
        InstallState::Downloading => CodeInstall::Downloading,
        InstallState::System { path, version } => CodeInstall::System {
            path: path.display().to_string(),
            version,
        },
        InstallState::Installed { version } => CodeInstall::Installed { version },
        InstallState::Unavailable => CodeInstall::Unavailable,
        InstallState::NeedsRuntime {
            runtime,
            min,
            found,
        } => CodeInstall::NeedsRuntime {
            runtime,
            min,
            found,
        },
        InstallState::TrustedOnly => CodeInstall::TrustedOnly,
        InstallState::Declined => CodeInstall::Declined,
        InstallState::Failed { error } => CodeInstall::Failed { error },
    }
}

/// How alive a server is, for choosing what one language shows across
/// worktrees.
fn liveness(state: ServerState) -> u8 {
    match state {
        ServerState::Off => 0,
        ServerState::NotRunning => 1,
        ServerState::Failed => 2,
        ServerState::Warming => 3,
        ServerState::Ready => 4,
    }
}

/// One worktree's statuses into `into`. Per language: the server of the
/// worktree where it is most alive, and the files and index of the worktree
/// with the most files.
pub(crate) fn merge_statuses(into: &mut Vec<LanguageStatus>, statuses: Vec<LanguageStatus>) {
    for next in statuses {
        let Some(have) = into.iter_mut().find(|l| l.language == next.language) else {
            into.push(next);
            continue;
        };
        let alive = |l: &LanguageStatus| l.server.as_ref().map(|s| liveness(s.state));
        let spaces_only = |l: &LanguageStatus| {
            l.server
                .as_ref()
                .is_some_and(|s| s.install == InstallState::TrustedOnly)
        };
        // Equally alive: what a space can install says more than a review's
        // "Spaces only".
        if alive(&next) > alive(have)
            || (alive(&next) == alive(have) && spaces_only(have) && !spaces_only(&next))
        {
            have.server = next.server;
        }
        if next.files > have.files {
            have.files = next.files;
            have.index_percent = next.index_percent;
            have.grammar = next.grammar;
        }
    }
}

fn language_view(l: LanguageStatus, declined: &[String]) -> CodeLanguage {
    let was_declined = declined.iter().any(|d| d == l.language.as_str());
    CodeLanguage {
        language: l.language.0,
        files: u32::try_from(l.files).unwrap_or(u32::MAX),
        grammar: install_view(l.grammar, false),
        server: l.server.map(|s| CodeServer {
            name: s.name,
            hint: s.hint,
            install: install_view(s.install, was_declined),
            state: match s.state {
                ServerState::Off => CodeServerState::Off,
                ServerState::NotRunning => CodeServerState::NotRunning,
                ServerState::Warming => CodeServerState::Warming,
                ServerState::Ready => CodeServerState::Ready,
                ServerState::Failed => CodeServerState::Failed,
            },
        }),
        index_percent: l.index_percent.min(100),
    }
}

// ── applying edits ──────────────────────────────────────────────────────────

/// The text a symbol edit expects at `range` before it applies: an index
/// older than the file refuses the edit instead of splicing blind.
struct Check {
    range: theodore_ports::Range,
    text: String,
}

/// A symbol's range may start after modifiers on its line (`export`,
/// `export default`, `pub`): when the new `body` starts with those same
/// words, they are part of what it replaces, so they don't end up twice.
/// Returns the range to replace and the text it holds now.
async fn with_modifiers(
    root: &Path,
    path: &str,
    range: theodore_ports::Range,
    text: String,
    body: &str,
) -> (theodore_ports::Range, String) {
    let unchanged = (range, text.clone());
    let Ok(full) = crate::usecases::suggestions::safe_join(root, path).await else {
        return unchanged;
    };
    let Ok(file) = tokio::fs::read_to_string(&full).await else {
        return unchanged;
    };
    let line_start = theodore_ports::Position {
        line: range.start.line,
        col: 1,
    };
    let (Some(ls), Some(s)) = (offset(&file, line_start), offset(&file, range.start)) else {
        return unchanged;
    };
    let before = &file[ls..s];
    let mods = before.trim_start();
    if mods.is_empty() || !body.trim_start().starts_with(mods) {
        return unchanged;
    }
    let indent = before.len() - mods.len();
    let start = theodore_ports::Position {
        line: range.start.line,
        col: 1 + before[..indent].chars().count() as u32,
    };
    (
        theodore_ports::Range { start, ..range },
        format!("{mods}{text}"),
    )
}

/// Applies `edits` to files under `root`: every file is read and edited in
/// memory first (back to front), then all are written, then the port hears
/// of each. A path leaving the worktree, a stale range (`check`, or an
/// edit's own `expect`) or overlapping edits fail the whole call before
/// anything is written.
async fn apply_checked(
    port: &dyn CodeIntelPort,
    root: &Path,
    edits: &[TextEdit],
    check: Option<&Check>,
) -> AppResult<CodeEdits> {
    let mut by_path: std::collections::BTreeMap<&str, Vec<&TextEdit>> = Default::default();
    for e in edits {
        by_path.entry(e.path.as_str()).or_default().push(e);
    }
    let mut out: Vec<(String, PathBuf, String)> = Vec::new();
    for (path, es) in &by_path {
        relative(path)?;
        let full = crate::usecases::suggestions::safe_join(root, path).await?;
        let text = tokio::fs::read_to_string(&full)
            .await
            .map_err(|e| AppError::Invalid(format!("reading {path}: {e}")))?;
        let held = |range: theodore_ports::Range| {
            let (s, e) = (offset(&text, range.start), offset(&text, range.end));
            s.zip(e).and_then(|(s, e)| text.get(s..e))
        };
        if let Some(c) = check
            && held(c.range) != Some(c.text.as_str())
        {
            return Err(AppError::Invalid(format!(
                "{path} changed since it was indexed; read the symbol again and retry"
            )));
        }
        if es.iter().any(|e| {
            e.expect
                .as_deref()
                .is_some_and(|want| held(e.range) != Some(want))
        }) {
            return Err(AppError::Invalid(format!(
                "{path} changed since the language server read it; ask again"
            )));
        }
        let new = splice(&text, es).map_err(|e| AppError::Invalid(format!("{path}: {e}")))?;
        out.push((path.to_string(), full, new));
    }
    for (path, full, new) in &out {
        tokio::fs::write(full, new)
            .await
            .map_err(|e| AppError::Invalid(format!("writing {path}: {e}")))?;
    }
    for (path, _, _) in &out {
        port.file_changed(root, path).await;
    }
    Ok(CodeEdits {
        files: out.into_iter().map(|(p, _, _)| p).collect(),
        edits: edits.len(),
    })
}

/// The byte offset of a 1-based line and char column in `text`; the column
/// may be one past the line's last char (its end). `None` outside the text.
pub(crate) fn offset(text: &str, at: Position) -> Option<usize> {
    let line = usize::try_from(at.line).ok()?.checked_sub(1)?;
    let col = usize::try_from(at.col).ok()?.checked_sub(1)?;
    let start = if line == 0 {
        0
    } else {
        text.match_indices('\n').nth(line - 1)?.0 + 1
    };
    let rest = &text[start..];
    let content = &rest[..rest.find('\n').unwrap_or(rest.len())];
    if col == content.chars().count() {
        return Some(start + content.len());
    }
    content.char_indices().nth(col).map(|(i, _)| start + i)
}

/// `text` with `edits` applied, the last one first so earlier offsets
/// hold. Overlapping edits are refused.
pub(crate) fn splice(text: &str, edits: &[&TextEdit]) -> Result<String, String> {
    let mut spans: Vec<(usize, usize, &str)> = Vec::with_capacity(edits.len());
    for e in edits {
        let s = offset(text, e.range.start).ok_or_else(|| {
            format!(
                "{}:{} is outside the file",
                e.range.start.line, e.range.start.col
            )
        })?;
        let end = offset(text, e.range.end).ok_or_else(|| {
            format!(
                "{}:{} is outside the file",
                e.range.end.line, e.range.end.col
            )
        })?;
        if end < s {
            return Err("an edit ends before it starts".into());
        }
        spans.push((s, end, e.new_text.as_str()));
    }
    spans.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    for w in spans.windows(2) {
        // `w[0]` starts at or after `w[1]`.
        if w[1].1 > w[0].0 {
            return Err("edits overlap".into());
        }
    }
    let mut out = text.to_string();
    for (s, e, new) in spans {
        out.replace_range(s..e, new);
    }
    Ok(out)
}

/// The same directory, however it's spelled: the language servers (and git)
/// report a root by its real path, while a stored worktree may run through
/// a symlink (`/tmp` → `/private/tmp`). A path that's gone compares as is.
pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    real(a) == real(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use theodore_ports::Range;

    fn pos(line: u32, col: u32) -> Position {
        Position { line, col }
    }

    fn edit(start: (u32, u32), end: (u32, u32), new: &str) -> TextEdit {
        TextEdit {
            path: "f".into(),
            range: Range {
                start: pos(start.0, start.1),
                end: pos(end.0, end.1),
            },
            new_text: new.into(),
            expect: None,
        }
    }

    #[test]
    fn offsets_count_chars_not_bytes() {
        let text = "héllo\nwörld\n";
        assert_eq!(offset(text, pos(1, 1)), Some(0));
        assert_eq!(offset(text, pos(1, 3)), Some(3), "é is two bytes");
        assert_eq!(offset(text, pos(1, 6)), Some(6), "end of the line");
        assert_eq!(offset(text, pos(2, 2)), Some(8));
        assert_eq!(
            offset(text, pos(3, 1)),
            Some(text.len()),
            "after the last newline"
        );
        assert_eq!(offset(text, pos(1, 8)), None);
        assert_eq!(offset(text, pos(4, 1)), None);
        assert_eq!(offset(text, pos(0, 1)), None);
    }

    #[test]
    fn edits_apply_back_to_front() {
        let text = "fn a() {}\nfn b() {}\n";
        let e1 = edit((1, 4), (1, 5), "alpha");
        let e2 = edit((2, 4), (2, 5), "beta");
        assert_eq!(
            splice(text, &[&e1, &e2]).unwrap(),
            "fn alpha() {}\nfn beta() {}\n"
        );
        // An insert right where a replacement ends doesn't overlap it.
        let r = edit((1, 1), (1, 3), "pub fn");
        let i = edit((1, 3), (1, 3), "_");
        assert_eq!(
            splice(text, &[&r, &i]).unwrap(),
            "pub fn_ a() {}\nfn b() {}\n"
        );
    }

    #[test]
    fn overlapping_and_outside_edits_are_refused() {
        let text = "abcdef\n";
        let a = edit((1, 1), (1, 4), "x");
        let b = edit((1, 3), (1, 5), "y");
        assert_eq!(splice(text, &[&a, &b]).unwrap_err(), "edits overlap");
        let far = edit((9, 1), (9, 2), "z");
        assert!(splice(text, &[&far]).is_err());
    }

    #[test]
    fn agent_paths_stay_relative() {
        assert!(relative("src/lib.rs").is_ok());
        assert!(relative("./src/lib.rs").is_ok());
        for bad in ["", "/etc/passwd", "../x", "src/../../x"] {
            assert!(relative(bad).is_err(), "{bad}");
        }
    }
}
