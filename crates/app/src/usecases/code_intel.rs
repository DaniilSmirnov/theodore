//! Code intelligence above the port (docs/code-intel.md): which worktrees
//! are open on `CodeIntelPort`, under which trust, the install flow, the
//! MCP tools' calls (queries, and edits applied here), and Settings →
//! Languages.
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
    CodeInstall, CodeInstallPolicy, CodeIntelLspRequest, CodeIntelStatus, CodeLanguage, CodeLsp,
    CodeLspScope, CodeLspToggle, CodeServer, CodeServerState, ConfigLayer, Ephemeral, Project,
    Setting, Space, SpaceMode, SpaceStatus, models,
};
use theodore_domain::{PrId, RepoRef};
use theodore_ports::{
    Answer, CodeHit, CodeIntelError, CodeIntelPort, CodeSymbol, CodeSymbolKind, Diagnostic,
    InstallState, LanguageId, LanguageStatus, NewAction, Position, ServerState, SyncActionCode,
    TextEdit, TrustMode, Workspace,
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
/// Settings → Languages' toggles, below a global or local config file.
pub(crate) const LSP_REVIEW_KEY: &str = "code_intel.lsp.review";
pub(crate) const LSP_SPACES_KEY: &str = "code_intel.lsp.spaces";

/// Where language servers may run (`code_intel.lsp`), resolved: your
/// global / local file, else the Settings toggle, else the default. The
/// repo's file never sets it (`config::repo_may_not_set`).
/// Handed to the port per workspace (`Workspace::lsp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspPolicy {
    /// In review runs: a PR's code may run.
    pub review: bool,
    pub spaces: bool,
}

/// One worktree open on the port.
#[derive(Debug, Clone, PartialEq)]
struct Opened {
    trust: TrustMode,
    project: Option<String>,
    /// Language servers may serve it (`LspPolicy`).
    lsp: bool,
}

/// In-memory: what is open, and which languages were suggested this
/// session.
#[derive(Default)]
pub(crate) struct CodeIntelState {
    open: std::sync::Mutex<HashMap<PathBuf, Opened>>,
    suggested: std::sync::Mutex<HashSet<String>>,
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
pub(crate) fn code_prompt(code: CodeTools) -> Option<&'static str> {
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

    /// `cfg.lsp` with the Settings toggles and defaults under it.
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
        let lsp = self.lsp_policy(&cfg).await.review;
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
        };
        if self
            .open_code_workspace(ws, lsp, cfg.install, project)
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
        // An untrusted space is served like a review.
        let policy = self.lsp_policy(code).await;
        let lsp = policy.spaces && (trusted || policy.review);
        let ws = Workspace {
            repo_key: repo_key(&project.repo),
            root: PathBuf::from(&space.worktree),
            trust: if trusted {
                TrustMode::Trusted
            } else {
                TrustMode::Review
            },
            languages: languages(code),
            focus_paths: Vec::new(),
            lsp: None,
        };
        self.open_code_workspace(ws, lsp, code.install, Some(project.id.clone()))
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
            SpaceMode::Ask => CodeTools::Read,
            SpaceMode::Edit | SpaceMode::Run => CodeTools::Edit,
        }
    }

    /// `open` on the port unless `ws.root` is open already under the same
    /// trust and LSP policy (a change reopens it), then the install flow in
    /// the background. `false` when the port refused.
    async fn open_code_workspace(
        &self,
        ws: Workspace,
        lsp: bool,
        install: InstallPolicy,
        project: Option<String>,
    ) -> bool {
        let Some(port) = self.code_port() else {
            return false;
        };
        let want = Opened {
            trust: ws.trust,
            project: project.clone(),
            lsp,
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
            Some(o) if o.trust == want.trust && o.lsp == want.lsp => return true,
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

    /// A space turn moved its checkpoint `before` → `after`: the files the
    /// agent wrote with its own tools are reindexed, in the background.
    pub(crate) fn spawn_space_code_changed(
        &self,
        root: PathBuf,
        before: theodore_domain::Sha,
        after: theodore_domain::Sha,
    ) {
        let Some(port) = self.code_port() else {
            return;
        };
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
                InstallPolicy::Always => {
                    if let Err(e) = port.install(&l.language).await {
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

    // ── Settings → Languages ────────────────────────────────────────────

    /// Each language of `project`: from a worktree of it already open,
    /// else (a local folder) the folder itself, read-only.
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
        let mut root = self
            .0
            .code_intel
            .open
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find(|(_, o)| o.project.as_deref() == Some(project))
            .map(|(r, _)| r.clone());
        if root.is_none()
            && let Some(folder) = &p.path
        {
            let ws = Workspace {
                repo_key: repo_key(&p.repo),
                root: PathBuf::from(folder),
                trust: TrustMode::Review,
                languages: languages(&cfg),
                focus_paths: Vec::new(),
                lsp: None,
            };
            if self
                .open_code_workspace(ws, policy.review, cfg.install, Some(p.id.clone()))
                .await
            {
                root = Some(PathBuf::from(folder));
            }
        }
        let mut languages = Vec::new();
        if let Some(root) = root {
            let declined = self.declined_languages().await;
            languages = port
                .status(&root)
                .await
                .map_err(code_err)?
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

    /// Settings → Languages: one LSP toggle, for every project. A global
    /// or local file setting the key still wins.
    pub async fn code_intel_set_lsp(&self, req: CodeIntelLspRequest) -> AppResult<()> {
        let key = match req.scope {
            CodeLspScope::Review => LSP_REVIEW_KEY,
            CodeLspScope::Spaces => LSP_SPACES_KEY,
        };
        self.write_user_setting(key, serde_json::json!(req.on))
            .await
    }

    /// "Install": downloads `language`'s grammar and server, and forgets
    /// an earlier No.
    pub async fn code_intel_install(&self, language: &str) -> AppResult<()> {
        let port = self.code_port().ok_or_else(off)?;
        let language = language.trim();
        if language.is_empty() {
            return Err(AppError::Invalid("no language".into()));
        }
        let mut declined = self.declined_languages().await;
        if declined.iter().any(|l| l == language) {
            declined.retain(|l| l != language);
            self.write_declined(declined).await?;
        }
        port.install(&LanguageId::new(language))
            .await
            .map_err(code_err)
    }

    /// "No": `language`'s server isn't suggested again.
    pub async fn code_intel_decline(&self, language: &str) -> AppResult<()> {
        let language = language.trim();
        if language.is_empty() {
            return Err(AppError::Invalid("no language".into()));
        }
        let mut declined = self.declined_languages().await;
        if !declined.iter().any(|l| l == language) {
            declined.push(language.to_string());
            declined.sort();
            self.write_declined(declined).await?;
        }
        Ok(())
    }

    async fn write_declined(&self, languages: Vec<String>) -> AppResult<()> {
        self.write_user_setting(DECLINED_KEY, serde_json::json!(languages))
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
    async fn code_root(&self, scope: &CodeScope) -> AppResult<(Arc<dyn CodeIntelPort>, PathBuf)> {
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
                Ok((port, entry.worktree_path.clone()))
            }
            CodeScope::Space(id) => {
                let (space, project) = self.code_space(id).await?;
                if !self.open_space_code(&space, &project).await {
                    return Err(off());
                }
                Ok((port, PathBuf::from(&space.worktree)))
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
        let (port, root) = self.code_root(scope).await?;
        port.find_symbol(&root, name, kind).await.map_err(code_err)
    }

    /// The symbol and its source text.
    pub async fn code_get_symbol(
        &self,
        scope: &CodeScope,
        path: &str,
        name: &str,
    ) -> AppResult<(CodeSymbol, String)> {
        let (port, root) = self.code_root(scope).await?;
        port.symbol_body(&root, relative(path)?, name)
            .await
            .map_err(code_err)
    }

    pub async fn code_definition(
        &self,
        scope: &CodeScope,
        path: &str,
        at: Position,
    ) -> AppResult<Answer<CodeHit>> {
        let (port, root) = self.code_root(scope).await?;
        port.definition(&root, relative(path)?, at)
            .await
            .map_err(code_err)
    }

    pub async fn code_references(
        &self,
        scope: &CodeScope,
        path: &str,
        at: Position,
    ) -> AppResult<Answer<CodeHit>> {
        let (port, root) = self.code_root(scope).await?;
        port.references(&root, relative(path)?, at)
            .await
            .map_err(code_err)
    }

    pub async fn code_diagnostics(
        &self,
        scope: &CodeScope,
        path: Option<&str>,
    ) -> AppResult<Answer<Diagnostic>> {
        let (port, root) = self.code_root(scope).await?;
        let path = path.map(relative).transpose()?;
        port.diagnostics(&root, path).await.map_err(code_err)
    }

    pub async fn code_status(&self, scope: &CodeScope) -> AppResult<Vec<LanguageStatus>> {
        let (port, root) = self.code_root(scope).await?;
        port.status(&root).await.map_err(code_err)
    }

    /// The worktree of a space turn that may edit: refused in Ask mode
    /// (the space's `mode` is the current turn's) and on a review run.
    async fn edit_root(&self, scope: &CodeScope) -> AppResult<(Arc<dyn CodeIntelPort>, PathBuf)> {
        let CodeScope::Space(id) = scope else {
            return Err(AppError::Invalid(
                "code edits are for a space's agent turn only; a review run is read-only".into(),
            ));
        };
        let (space, _) = self.code_space(id).await?;
        if space.mode == SpaceMode::Ask {
            return Err(AppError::Invalid(
                "this turn is in Ask mode: code edits are off. Ask the user to switch to Edit"
                    .into(),
            ));
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
        let (port, root) = self.edit_root(scope).await?;
        let path = relative(path)?;
        let (symbol, text) = port
            .symbol_body(&root, path, name)
            .await
            .map_err(code_err)?;
        let edit = TextEdit {
            path: path.to_string(),
            range: symbol.range,
            new_text: body.to_string(),
        };
        apply_edits(&*port, &root, &[edit], Some(&text)).await
    }

    /// `insert_after_symbol`: `text` on the line after `name`'s definition.
    pub async fn code_insert_after_symbol(
        &self,
        scope: &CodeScope,
        path: &str,
        name: &str,
        text: &str,
    ) -> AppResult<CodeEdits> {
        let (port, root) = self.edit_root(scope).await?;
        let path = relative(path)?;
        let (symbol, body) = port
            .symbol_body(&root, path, name)
            .await
            .map_err(code_err)?;
        let at = symbol.range.end;
        let edit = TextEdit {
            path: path.to_string(),
            range: theodore_ports::Range { start: at, end: at },
            new_text: format!("\n{}", text.trim_end_matches('\n')),
        };
        let check = Check {
            range: symbol.range,
            text: body,
        };
        apply_checked(&*port, &root, &[edit], Some(&check)).await
    }

    /// `rename_symbol`: the server's rename edits, in every file.
    pub async fn code_rename_symbol(
        &self,
        scope: &CodeScope,
        path: &str,
        at: Position,
        new_name: &str,
    ) -> AppResult<CodeEdits> {
        let (port, root) = self.edit_root(scope).await?;
        let new_name = new_name.trim();
        if new_name.is_empty() {
            return Err(AppError::Invalid("the new name is empty".into()));
        }
        let edits = port
            .rename(&root, relative(path)?, at, new_name)
            .await
            .map_err(code_err)?;
        if edits.is_empty() {
            return Err(AppError::Invalid(
                "nothing to rename here (no language server answered)".into(),
            ));
        }
        apply_edits(&*port, &root, &edits, None).await
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
        InstallState::Declined => CodeInstall::Declined,
        InstallState::Failed { error } => CodeInstall::Failed { error },
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
            install: install_view(s.install, was_declined),
            state: match s.state {
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

/// `apply_checked` where the only edit's own range must still hold
/// `expect`.
async fn apply_edits(
    port: &dyn CodeIntelPort,
    root: &Path,
    edits: &[TextEdit],
    expect: Option<&str>,
) -> AppResult<CodeEdits> {
    let check = match (expect, edits) {
        (Some(text), [one]) => Some(Check {
            range: one.range,
            text: text.to_string(),
        }),
        _ => None,
    };
    apply_checked(port, root, edits, check.as_ref()).await
}

/// Applies `edits` to files under `root`: every file is read and edited in
/// memory first (back to front), then all are written, then the port hears
/// of each. A path leaving the worktree, a stale range or overlapping
/// edits fail the whole call before anything is written.
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
        if let Some(c) = check {
            let (s, e) = (offset(&text, c.range.start), offset(&text, c.range.end));
            let current = s.zip(e).and_then(|(s, e)| text.get(s..e));
            if current != Some(c.text.as_str()) {
                return Err(AppError::Invalid(format!(
                    "{path} changed since it was indexed; read the symbol again and retry"
                )));
            }
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
