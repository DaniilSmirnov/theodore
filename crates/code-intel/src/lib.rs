//! Code intelligence (docs/code-intel.md): [`CodeIntelPort`] over the syntax
//! index (`code-intel-syntax`), language servers (`code-intel-lsp`) and the
//! toolchain that finds and installs both (`code-intel-toolchain`).
//!
//! ```text
//! query(path) ─▶ language of path ─▶ policy allows LSP here, server Ready? ── yes ─▶ LSP   Source::Lsp
//!                                     │  (else note: "warming up", "off for         │ empty/error
//!                                     │   reviews (…)", "not installed")            ▼
//!                                     └──────────────────────────────────────▶ syntax index Source::Syntax
//!                                                                                 │ empty, or files it
//!                                                                                 ▼ does not cover yet
//!                                                                              text search  Source::Text
//!
//! open(ws) ─▶ git snapshot (languages from the manifest's globs)
//!          ─▶ indexer: focus_paths first ─▶ names they define ─▶ the rest
//!          │            └─ grammar per language, fetched in the background (no consent)
//!          └▶ per language: policy allows LSP, server already on disk? ─▶ spawn (warms up)
//!                           (downloads only in install())
//! ```
//!
//! Trust: [`LspPolicy`] has one switch per [`TrustMode`] — review runs
//! (off by default: most servers can run a PR's code) and spaces (on). The
//! one gate is `servers::allowed`; [`CodeIntel::set_options`] applies a
//! change live, stopping the sessions it no longer allows.

mod grammars;
mod paths;
mod servers;
mod workspace;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use theodore_code_intel_lsp::{LspPool, LspSession, SessionOptions};
use theodore_code_intel_syntax::{
    Indexer, IndexerConfig, SyntaxIndex, byte_offset, file_entry, git_snapshot, preview,
    text_search,
};
use theodore_code_intel_toolchain::Toolchain;
use theodore_ports::code_intel::{
    Answer, CodeHit, CodeIntelError, CodeIntelPort, CodeIntelResult, CodeSymbol, CodeSymbolKind,
    Diagnostic, InstallState, LanguageId, LanguageStatus, Location, LspPolicy, Position,
    ServerState, ServerStatus, Source, TextEdit, TrustMode, Workspace,
};

pub use grammars::GrammarSource;

use crate::paths::{canonical, normalize, read_inside};
use crate::servers::{allowed, denied_note};
use crate::workspace::{Gap, Ws, is_code, language_of, lock};

/// Most hits one answer carries.
const MAX_HITS: usize = 200;
/// How long a query about one file waits for it to be indexed.
const INDEX_WAIT: Duration = Duration::from_secs(2);
/// Names from the focus files used to promote files that mention them.
const MAX_FOCUS_NAMES: usize = 64;

/// Settings of [`CodeIntel`] (docs/code-intel.md §9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeIntelOptions {
    /// `<data dir>/code-intel`: `index.sqlite`, `grammars/`, `servers/`.
    pub data_dir: PathBuf,
    /// Language servers alive at once (`code_intel.max_servers`).
    pub max_servers: usize,
    /// An unused server is shut down after this (`code_intel.idle_minutes`).
    pub idle: Duration,
    /// Larger files are not indexed (`code_intel.max_file_kb`).
    pub max_file_kb: usize,
    /// Where language servers may run (`code_intel.lsp.review` /
    /// `code_intel.lsp.spaces`).
    pub lsp: LspPolicy,
}

impl CodeIntelOptions {
    /// The defaults of docs/code-intel.md §9.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            max_servers: 4,
            idle: Duration::from_secs(10 * 60),
            max_file_kb: 1024,
            lsp: LspPolicy::default(),
        }
    }
}

/// Replaces parts of [`CodeIntel`] (tests, tools). Defaults fit the app.
pub struct CodeIntelBuilder {
    options: CodeIntelOptions,
    toolchain: Option<Toolchain>,
    grammars: Option<Arc<dyn GrammarSource>>,
    session: SessionOptions,
    workers: Option<usize>,
}

impl CodeIntelBuilder {
    /// Default: `Toolchain::new(data_dir)` with the builtin manifest.
    pub fn toolchain(mut self, toolchain: Toolchain) -> Self {
        self.toolchain = Some(toolchain);
        self
    }

    /// Default: the toolchain (downloads grammars).
    pub fn grammar_source(mut self, source: Arc<dyn GrammarSource>) -> Self {
        self.grammars = Some(source);
        self
    }

    /// Timeouts of each language server session.
    pub fn session_options(mut self, options: SessionOptions) -> Self {
        self.session = options;
        self
    }

    /// Files parsed at once. Default: half the CPUs, 1–4.
    pub fn index_workers(mut self, workers: usize) -> Self {
        self.workers = Some(workers);
        self
    }

    /// Opens `index.sqlite`. Must be called inside a tokio runtime.
    pub fn build(self) -> CodeIntelResult<CodeIntel> {
        let o = self.options;
        let toolchain = self
            .toolchain
            .unwrap_or_else(|| Toolchain::new(&o.data_dir));
        let grammars = self
            .grammars
            .unwrap_or_else(|| Arc::new(toolchain.clone()) as Arc<dyn GrammarSource>);
        let index = SyntaxIndex::open(&o.data_dir.join("index.sqlite"))?;
        let mut config = IndexerConfig {
            max_file_bytes: o.max_file_kb.saturating_mul(1024),
            ..IndexerConfig::default()
        };
        if let Some(w) = self.workers {
            config.workers = w.max(1);
        }
        let provider = grammars::provider(grammars.clone(), toolchain.manifest().clone());
        let pool = LspPool::with_options(o.max_servers, o.idle, self.session.clone());
        Ok(CodeIntel {
            inner: Arc::new(Inner {
                indexer: Indexer::new(index, provider, config),
                pool: Mutex::new(Arc::new(pool)),
                session: self.session,
                options: Mutex::new(o),
                toolchain,
                grammars,
                workspaces: Mutex::default(),
            }),
        })
    }
}

/// Code intelligence over open worktrees. Cheap to clone.
#[derive(Clone)]
pub struct CodeIntel {
    inner: Arc<Inner>,
}

struct Inner {
    toolchain: Toolchain,
    grammars: Arc<dyn GrammarSource>,
    indexer: Indexer,
    /// Replaced when `max_servers` or `idle` change.
    pool: Mutex<Arc<LspPool>>,
    session: SessionOptions,
    options: Mutex<CodeIntelOptions>,
    /// By canonical root.
    workspaces: Mutex<HashMap<PathBuf, Arc<Ws>>>,
}

/// Where an LSP question goes.
enum Route {
    Ready(Arc<LspSession>),
    /// No server answers; why, when it is worth telling.
    Not(Option<String>),
}

/// Notes of one answer, each once.
#[derive(Default)]
struct Notes(Vec<String>);

impl Notes {
    fn push(&mut self, note: impl Into<String>) {
        let note = note.into();
        if !note.is_empty() && !self.0.contains(&note) {
            self.0.push(note);
        }
    }

    fn extend(&mut self, notes: impl IntoIterator<Item = String>) {
        for n in notes {
            self.push(n);
        }
    }

    fn finish(self) -> Option<String> {
        (!self.0.is_empty()).then(|| self.0.join("; "))
    }
}

impl CodeIntel {
    /// With the app's toolchain and grammar downloads. Must be called inside
    /// a tokio runtime.
    pub fn new(options: CodeIntelOptions) -> CodeIntelResult<Self> {
        Self::builder(options).build()
    }

    pub fn builder(options: CodeIntelOptions) -> CodeIntelBuilder {
        CodeIntelBuilder {
            options,
            toolchain: None,
            grammars: None,
            session: SessionOptions::default(),
            workers: None,
        }
    }

    pub fn toolchain(&self) -> &Toolchain {
        &self.inner.toolchain
    }

    pub fn options(&self) -> CodeIntelOptions {
        lock(&self.inner.options).clone()
    }

    /// Apply changed settings. Live: `lsp` (sessions the new policy no
    /// longer allows stop; newly allowed ones start where a server is on
    /// disk), `max_servers` and `idle` (the pool restarts). `data_dir` and
    /// `max_file_kb` take effect on the next start.
    pub async fn set_options(&self, options: CodeIntelOptions) {
        let old = std::mem::replace(&mut *lock(&self.inner.options), options.clone());
        if old.data_dir != options.data_dir || old.max_file_kb != options.max_file_kb {
            tracing::info!(target: "code_intel", "data_dir / max_file_kb change applies after a restart");
        }
        let new_pool = old.max_servers != options.max_servers || old.idle != options.idle;
        if new_pool {
            let pool = Arc::new(LspPool::with_options(
                options.max_servers,
                options.idle,
                self.inner.session.clone(),
            ));
            let old_pool = std::mem::replace(&mut *lock(&self.inner.pool), pool);
            old_pool.shutdown_all().await;
        }
        let open: Vec<Arc<Ws>> = lock(&self.inner.workspaces).values().cloned().collect();
        for w in open.into_iter().filter(|w| w.lsp.is_none()) {
            let (was, now) = (allowed(old.lsp, w.trust), allowed(options.lsp, w.trust));
            if !now {
                if was {
                    self.pool().release_root(&w.root).await;
                }
                for plan in lock(&w.servers).values_mut() {
                    plan.resolved = None;
                }
            } else if !was || new_pool {
                self.start_servers(&w).await;
            }
        }
    }

    /// Close every workspace and stop every server.
    pub async fn shutdown(&self) {
        let all: Vec<Arc<Ws>> = lock(&self.inner.workspaces)
            .drain()
            .map(|(_, w)| w)
            .collect();
        for w in all {
            w.shut();
        }
        self.pool().shutdown_all().await;
    }

    fn policy(&self) -> LspPolicy {
        lock(&self.inner.options).lsp
    }

    /// `w`'s own policy, else the default.
    fn policy_for(&self, w: &Ws) -> LspPolicy {
        w.lsp.unwrap_or_else(|| self.policy())
    }

    fn pool(&self) -> Arc<LspPool> {
        lock(&self.inner.pool).clone()
    }

    fn index(&self) -> &SyntaxIndex {
        self.inner.indexer.index()
    }

    fn ws(&self, root: &Path) -> CodeIntelResult<Arc<Ws>> {
        let map = lock(&self.inner.workspaces);
        map.get(root)
            .or_else(|| map.get(&canonical(root)))
            .cloned()
            .ok_or_else(|| CodeIntelError::NotOpen(root.to_path_buf()))
    }

    fn answer<T>(&self, w: &Ws, items: Vec<T>, source: Source, notes: Notes) -> Answer<T> {
        Answer {
            items,
            source,
            index_percent: w.index.progress().percent(),
            note: notes.finish(),
        }
    }

    // ── open / close ──────────────────────────────────────────────────────

    async fn open_ws(&self, ws: Workspace) -> CodeIntelResult<()> {
        let root = canonical(&ws.root);
        if !root.is_dir() {
            return Err(CodeIntelError::FileNotFound(ws.root.display().to_string()));
        }
        let focus: Vec<String> = ws
            .focus_paths
            .iter()
            .filter_map(|p| normalize(p).ok())
            .collect();
        if let Ok(existing) = self.ws(&root) {
            let same = existing.repo_key == ws.repo_key
                && existing.trust == ws.trust
                && existing.requested == ws.languages;
            if same {
                self.focus(&existing, focus);
                return Ok(());
            }
            self.close_ws(&root).await;
        }

        let only = (!ws.languages.is_empty()).then(|| ws.languages.iter().cloned().collect());
        let lang_of = language_of(self.inner.toolchain.manifest().clone(), only);
        let snapshot = {
            let (root, lang_of) = (root.clone(), lang_of.clone());
            blocking(move || git_snapshot(&root, &*lang_of).map_err(CodeIntelError::from)).await?
        };
        let w = Arc::new(Ws {
            index: self.inner.indexer.start(snapshot),
            pool_key: servers::pool_key(&ws.repo_key, ws.trust),
            root: root.clone(),
            repo_key: ws.repo_key,
            trust: ws.trust,
            lsp: ws.lsp,
            requested: ws.languages,
            language_of: lang_of,
            servers: Mutex::default(),
            tasks: Mutex::default(),
        });
        {
            let mut map = lock(&self.inner.workspaces);
            if map.contains_key(&root) {
                // A concurrent open of the same root won.
                w.shut();
                return Ok(());
            }
            map.insert(root.clone(), w.clone());
        }
        self.focus(&w, focus);
        tracing::info!(target: "code_intel", root = %root.display(), trust = ?w.trust, languages = ?w.languages(), "workspace open");
        self.start_servers(&w).await;
        Ok(())
    }

    /// Decide every language's server and start those the policy allows
    /// and that are on disk, so they warm up before the first question.
    async fn start_servers(&self, w: &Ws) {
        for lang in w.languages() {
            self.plan_server(w, &lang).await;
        }
        for server in w.servers_by_name().into_keys() {
            self.attach(w, &server).await;
        }
    }

    /// Index `paths` first, then files mentioning the names they define.
    fn focus(&self, w: &Arc<Ws>, paths: Vec<String>) {
        if paths.is_empty() {
            return;
        }
        w.index.prioritize_paths(&paths);
        let (me, ws) = (self.clone(), w.clone());
        w.track(tokio::spawn(async move {
            ws.index.index_now(&paths).await;
            let (snap, index) = (ws.index.snapshot(), me.index().clone());
            let names = blocking(move || {
                let mut names: Vec<String> = Vec::new();
                for p in &paths {
                    for s in index.file_symbols(&snap, p).unwrap_or_default() {
                        if !names.contains(&s.name) {
                            names.push(s.name);
                        }
                    }
                }
                names.truncate(MAX_FOCUS_NAMES);
                Ok(names)
            })
            .await
            .unwrap_or_default();
            ws.index.prioritize_mentions(&names).await;
        }));
    }

    /// (Re)decide `lang`'s server for `w`. Probes; downloads nothing.
    async fn plan_server(&self, w: &Ws, lang: &LanguageId) {
        let policy = self.policy_for(w);
        let Some(plan) = servers::plan(&self.inner.toolchain, lang, &w.root, policy, w.trust).await
        else {
            return;
        };
        lock(&w.servers).insert(lang.clone(), plan);
    }

    /// Start (or join) `server` for `w` when the policy allows it and it is
    /// on disk.
    async fn attach(&self, w: &Ws, server: &str) -> Option<Arc<LspSession>> {
        let (plan, langs) = w.servers_by_name().remove(server)?;
        if !allowed(self.policy_for(w), w.trust) {
            return None;
        }
        let config = servers::launch(&self.inner.toolchain, &plan, w.trust, &langs)?;
        let result = self
            .pool()
            .get_or_spawn(&w.pool_key, &w.root, &config)
            .await;
        let error = result.as_ref().err().map(|e| e.to_string());
        if let Some(e) = &error {
            tracing::warn!(target: "code_intel", server, error = %e, "language server did not start");
        }
        for l in &langs {
            if let Some(p) = lock(&w.servers).get_mut(l) {
                p.error = error.clone();
            }
        }
        result.ok()
    }

    async fn close_ws(&self, root: &Path) {
        let w = {
            let mut map = lock(&self.inner.workspaces);
            map.remove(root).or_else(|| map.remove(&canonical(root)))
        };
        if let Some(w) = w {
            w.shut();
            self.pool().release_root(&w.root).await;
        }
    }

    // ── routing ───────────────────────────────────────────────────────────

    async fn route(&self, w: &Ws, lang: Option<&LanguageId>) -> Route {
        let Some(plan) = lang.and_then(|l| w.plan(l)) else {
            return Route::Not(None);
        };
        let name = plan.spec.name.clone();
        if !allowed(self.policy_for(w), w.trust) {
            return Route::Not(Some(denied_note(w.trust).into()));
        }
        if plan.resolved.is_none() {
            return Route::Not(Some(format!("{name} not installed")));
        }
        let session = match self.pool().get(&w.pool_key, &w.root, &name).await {
            Some(s) => s,
            // Idle-stopped or crashed once: the pool restarts it.
            None => match self.attach(w, &name).await {
                Some(s) => s,
                None => {
                    let why = w
                        .plan(lang.expect("plan found above"))
                        .and_then(|p| p.error)
                        .unwrap_or_else(|| "not running".into());
                    return Route::Not(Some(format!("{name}: {why}")));
                }
            },
        };
        match session.state() {
            ServerState::Ready => Route::Ready(session),
            ServerState::Warming => Route::Not(Some(format!("{name} warming up"))),
            ServerState::NotRunning | ServerState::Failed => {
                Route::Not(Some(format!("{name} not running")))
            }
        }
    }

    /// Why the syntax layer may miss things for `langs` (all when `None`).
    fn syntax_notes(&self, w: &Ws, only: Option<&LanguageId>) -> Vec<String> {
        let manifest = self.inner.toolchain.manifest();
        let progress = w.index.progress();
        let wanted = |l: &LanguageId| only.is_none_or(|o| o == l) && is_code(manifest, l);
        let mut notes: Vec<String> = progress
            .pending_languages
            .iter()
            .filter(|l| wanted(l))
            .map(|l| format!("{l}: grammar loading, text search"))
            .collect();
        notes.extend(
            progress
                .missing_languages
                .iter()
                .filter(|l| wanted(l))
                .map(|l| format!("{l}: no grammar yet, text search")),
        );
        notes
    }

    /// Wait (briefly) until `path` is indexed.
    async fn wait_indexed(&self, w: &Ws, path: &str) {
        let paths = [path.to_string()];
        let _ = tokio::time::timeout(INDEX_WAIT, w.index.index_now(&paths)).await;
    }

    /// Whole-word hits of `name` in files the syntax layer does not cover.
    async fn text(&self, gap: Gap, root: &Path, name: &str) -> Vec<CodeHit> {
        if gap.is_empty() || name.is_empty() {
            return Vec::new();
        }
        let (root, name) = (root.to_path_buf(), name.to_string());
        blocking(move || Ok(text_search(&root, &name, MAX_HITS, &|p| gap.includes(p))))
            .await
            .unwrap_or_default()
    }

    /// LSP locations as hits with previews from the files as they are now.
    async fn lsp_hits(&self, w: &Ws, locations: Vec<Location>, definitions: bool) -> Vec<CodeHit> {
        let root = w.root.clone();
        blocking(move || {
            let mut texts: HashMap<String, Option<String>> = HashMap::new();
            Ok(locations
                .into_iter()
                .take(MAX_HITS)
                .map(|location| {
                    let text = texts
                        .entry(location.path.clone())
                        .or_insert_with(|| read_inside(&root, &location.path).ok());
                    CodeHit {
                        preview: text
                            .as_deref()
                            .map(|t| preview(t, location.range.start.line))
                            .unwrap_or_default(),
                        location,
                        is_definition: definitions,
                        source: Source::Lsp,
                    }
                })
                .collect())
        })
        .await
        .unwrap_or_default()
    }

    // ── queries ───────────────────────────────────────────────────────────

    async fn find_symbol_in(
        &self,
        root: &Path,
        name: &str,
        kind: Option<CodeSymbolKind>,
    ) -> CodeIntelResult<Answer<CodeSymbol>> {
        let w = self.ws(root)?;
        let mut notes = Notes::default();
        let mut lsp: Vec<CodeSymbol> = Vec::new();
        let mut answered: HashSet<LanguageId> = HashSet::new();
        for (_, (_, langs)) in w.servers_by_name() {
            match self.route(&w, langs.first()).await {
                Route::Ready(s) => match s.workspace_symbols(&w.root, name).await {
                    Ok(syms) => {
                        lsp.extend(
                            syms.into_iter()
                                .filter(|s| s.name == name && kind.is_none_or(|k| k == s.kind)),
                        );
                        answered.extend(langs);
                    }
                    Err(e) => notes.push(e.to_string()),
                },
                Route::Not(n) => notes.extend(n),
            }
        }
        // Syntax answers for the languages no server answered for; all of
        // them when the servers found nothing.
        if lsp.is_empty() {
            answered.clear();
        }
        let (snap, index, n) = (w.index.snapshot(), self.index().clone(), name.to_string());
        let syntax = blocking(move || Ok(index.find_definitions(&snap, &n, kind)?)).await?;
        let syntax: Vec<CodeSymbol> = syntax
            .into_iter()
            .filter(|s| w.language(&s.path).is_none_or(|l| !answered.contains(&l)))
            .collect();
        notes.extend(self.syntax_notes(&w, None));
        let gap = w.gap(self.inner.toolchain.manifest(), self.index(), &answered);
        let text: Vec<CodeSymbol> = self
            .text(gap, &w.root, name)
            .await
            .into_iter()
            .filter(|h| h.is_definition)
            .map(|h| CodeSymbol {
                name: name.to_string(),
                kind: CodeSymbolKind::Other,
                range: h.location.range,
                name_range: h.location.range,
                path: h.location.path,
                container: None,
                source: Source::Text,
            })
            .collect();
        let source = best(!lsp.is_empty(), !syntax.is_empty());
        let mut items = lsp;
        items.extend(syntax);
        items.extend(text);
        items.truncate(MAX_HITS);
        Ok(self.answer(&w, items, source, notes))
    }

    async fn file_symbols_in(
        &self,
        root: &Path,
        path: &str,
    ) -> CodeIntelResult<Answer<CodeSymbol>> {
        let w = self.ws(root)?;
        let path = normalize(path)?;
        let lang = w.language(&path);
        let mut notes = Notes::default();
        match self.route(&w, lang.as_ref()).await {
            Route::Ready(s) => match s.document_symbols(&w.root, &path).await {
                Ok(syms) if !syms.is_empty() => {
                    return Ok(self.answer(&w, syms, Source::Lsp, notes));
                }
                Ok(_) => {}
                Err(e) => notes.push(e.to_string()),
            },
            Route::Not(n) => notes.extend(n),
        }
        self.wait_indexed(&w, &path).await;
        let (snap, index, p) = (w.index.snapshot(), self.index().clone(), path.clone());
        let syms = blocking(move || Ok(index.file_symbols(&snap, &p)?)).await?;
        notes.extend(self.syntax_notes(&w, lang.as_ref()));
        Ok(self.answer(&w, syms, Source::Syntax, notes))
    }

    async fn symbol_body_in(
        &self,
        root: &Path,
        path: &str,
        name: &str,
    ) -> CodeIntelResult<(CodeSymbol, String)> {
        let w = self.ws(root)?;
        let path = normalize(path)?;
        self.wait_indexed(&w, &path).await;
        let (snap, index, p, n) = (
            w.index.snapshot(),
            self.index().clone(),
            path.clone(),
            name.to_string(),
        );
        let syntax_err = match blocking(move || Ok(index.symbol_body(&snap, &p, &n)?)).await {
            Ok(found) => return Ok(found),
            Err(e) => e,
        };
        let Route::Ready(s) = self.route(&w, w.language(&path).as_ref()).await else {
            return Err(syntax_err);
        };
        let Some(sym) = s
            .document_symbols(&w.root, &path)
            .await
            .ok()
            .and_then(|syms| syms.into_iter().find(|s| s.name == name))
        else {
            return Err(syntax_err);
        };
        let (root, p) = (w.root.clone(), path.clone());
        let text = blocking(move || read_inside(&root, &p)).await?;
        match (
            byte_offset(&text, sym.range.start),
            byte_offset(&text, sym.range.end),
        ) {
            (Some(a), Some(b)) if a <= b => {
                let body = text[a..b].to_string();
                Ok((sym, body))
            }
            _ => Err(CodeIntelError::Other(format!(
                "{path} changed since the server read it"
            ))),
        }
    }

    async fn definition_in(
        &self,
        root: &Path,
        path: &str,
        at: Position,
    ) -> CodeIntelResult<Answer<CodeHit>> {
        let w = self.ws(root)?;
        let path = normalize(path)?;
        let lang = w.language(&path);
        let mut notes = Notes::default();
        match self.route(&w, lang.as_ref()).await {
            Route::Ready(s) => match s.definition(&w.root, &path, at).await {
                Ok(locs) if !locs.is_empty() => {
                    let hits = self.lsp_hits(&w, locs, true).await;
                    return Ok(self.answer(&w, hits, Source::Lsp, notes));
                }
                Ok(_) => notes.push(format!("{}: no definition; matched by name", s.server())),
                Err(e) => notes.push(e.to_string()),
            },
            Route::Not(n) => notes.extend(n),
        }
        self.wait_indexed(&w, &path).await;
        let (snap, index, p) = (w.index.snapshot(), self.index().clone(), path.clone());
        let (hits, word) = blocking(move || {
            let hits = index.definition(&snap, &p, at, MAX_HITS)?;
            let word = if hits.is_empty() {
                index.symbol_at(&snap, &p, at)?.map(|s| s.name)
            } else {
                None
            };
            Ok((hits, word))
        })
        .await?;
        notes.extend(self.syntax_notes(&w, None));
        if !hits.is_empty() {
            return Ok(self.answer(&w, hits, Source::Syntax, notes));
        }
        let gap = w.gap(
            self.inner.toolchain.manifest(),
            self.index(),
            &HashSet::new(),
        );
        let text = match word {
            Some(word) => self.text(gap, &w.root, &word).await,
            None => Vec::new(),
        };
        let defs: Vec<CodeHit> = text.into_iter().filter(|h| h.is_definition).collect();
        Ok(self.answer(&w, defs, Source::Text, notes))
    }

    async fn references_in(
        &self,
        root: &Path,
        path: &str,
        at: Position,
    ) -> CodeIntelResult<Answer<CodeHit>> {
        let w = self.ws(root)?;
        let path = normalize(path)?;
        let lang = w.language(&path);
        let mut notes = Notes::default();
        match self.route(&w, lang.as_ref()).await {
            Route::Ready(s) => match s.references(&w.root, &path, at, true).await {
                Ok(locs) if !locs.is_empty() => {
                    let hits = self.lsp_hits(&w, locs, false).await;
                    return Ok(self.answer(&w, hits, Source::Lsp, notes));
                }
                Ok(_) => notes.push(format!("{}: no references; matched by name", s.server())),
                Err(e) => notes.push(e.to_string()),
            },
            Route::Not(n) => notes.extend(n),
        }
        self.wait_indexed(&w, &path).await;
        let (snap, index, p) = (w.index.snapshot(), self.index().clone(), path.clone());
        let (hits, word) = blocking(move || {
            let Some(sym) = index.symbol_at(&snap, &p, at)? else {
                return Ok((Vec::new(), None));
            };
            let hits = index.references(&snap, &p, &sym.name, MAX_HITS)?;
            Ok((hits, Some(sym.name)))
        })
        .await?;
        notes.extend(self.syntax_notes(&w, None));
        let source = best(false, !hits.is_empty());
        let mut items = hits;
        if let Some(word) = word {
            let gap = w.gap(
                self.inner.toolchain.manifest(),
                self.index(),
                &HashSet::new(),
            );
            for h in self.text(gap, &w.root, &word).await {
                if !items.iter().any(|i| i.location == h.location) {
                    items.push(h);
                }
            }
        }
        items.truncate(MAX_HITS);
        Ok(self.answer(&w, items, source, notes))
    }

    async fn diagnostics_in(
        &self,
        root: &Path,
        path: Option<&str>,
    ) -> CodeIntelResult<Answer<Diagnostic>> {
        let w = self.ws(root)?;
        let mut notes = Notes::default();
        let mut items = Vec::new();
        let mut asked = false;
        match path {
            Some(p) => {
                let p = normalize(p)?;
                let lang = w.language(&p);
                match self.route(&w, lang.as_ref()).await {
                    Route::Ready(s) => {
                        asked = true;
                        match session_diagnostics(&s, &w.root, Some(&p)).await {
                            Ok(d) => items = d,
                            Err(e) => notes.push(e.to_string()),
                        }
                    }
                    Route::Not(n) => notes.extend(n),
                }
            }
            None => {
                for (_, (_, langs)) in w.servers_by_name() {
                    match self.route(&w, langs.first()).await {
                        Route::Ready(s) => {
                            asked = true;
                            match session_diagnostics(&s, &w.root, None).await {
                                Ok(d) => items.extend(d),
                                Err(e) => notes.push(e.to_string()),
                            }
                        }
                        Route::Not(n) => notes.extend(n),
                    }
                }
            }
        }
        if !asked {
            notes.push("diagnostics need a running language server");
        }
        Ok(self.answer(&w, items, Source::Lsp, notes))
    }

    async fn rename_in(
        &self,
        root: &Path,
        path: &str,
        at: Position,
        new_name: &str,
    ) -> CodeIntelResult<Vec<TextEdit>> {
        let w = self.ws(root)?;
        let path = normalize(path)?;
        match self.route(&w, w.language(&path).as_ref()).await {
            Route::Ready(s) => s.rename(&w.root, &path, at, new_name).await,
            Route::Not(n) => Err(CodeIntelError::Unsupported(format!(
                "rename needs a language server{}",
                n.map(|n| format!(" ({n})")).unwrap_or_default()
            ))),
        }
    }

    /// Hover text (markdown) at `at`, for the diff view. Language servers
    /// only: `None` when none answers for the file.
    pub async fn hover(
        &self,
        root: &Path,
        path: &str,
        at: Position,
    ) -> CodeIntelResult<Option<String>> {
        let w = self.ws(root)?;
        let path = normalize(path)?;
        match self.route(&w, w.language(&path).as_ref()).await {
            Route::Ready(s) => s.hover(&w.root, &path, at).await,
            Route::Not(_) => Ok(None),
        }
    }

    // ── install / changes / status ────────────────────────────────────────

    async fn install_language(&self, language: &LanguageId) -> CodeIntelResult<()> {
        let tc = self.inner.toolchain.clone();
        let spec =
            tc.manifest().language(language).cloned().ok_or_else(|| {
                CodeIntelError::Unsupported(format!("unknown language {language}"))
            })?;
        let mut first_error: Option<CodeIntelError> = None;

        // The grammar, and the one its tags query extends.
        let grammar = self.inner.grammars.grammar(language).await;
        if let Some(base) = &spec.tags_extends
            && let Err(e) = self.inner.grammars.grammar(base).await
        {
            tracing::warn!(target: "code_intel", %language, %base, error = %e, "base grammar");
        }
        let grammar_ok = grammar.is_ok();
        if let Err(e) = grammar
            && spec.grammar.is_some()
        {
            first_error = Some(e);
        }

        // The preferred server, downloaded only if it is not on the machine.
        // Resolved outside any worktree: a worktree's own binaries are
        // never what gets installed.
        let neutral = tc.root().to_path_buf();
        if let Some((server, state)) = tc
            .server_for(language, None, &neutral, TrustMode::Review)
            .await
        {
            let on_disk = matches!(
                state,
                InstallState::System { .. } | InstallState::Installed { .. }
            );
            if !on_disk && server.download_for(tc.platform()).is_some() {
                self.mark_downloading(&server.name);
                if let Err(e) = tc.install_server(&server).await {
                    first_error.get_or_insert(e);
                }
            }
        }

        // Every open workspace with the language picks both up.
        let open: Vec<Arc<Ws>> = lock(&self.inner.workspaces).values().cloned().collect();
        for w in open {
            if !w.languages().contains(language) {
                continue;
            }
            let progress = w.index.progress();
            if grammar_ok && progress.missing_languages.contains(language) {
                w.index.refresh_grammar(language);
            }
            self.plan_server(&w, language).await;
            if let Some(plan) = w.plan(language) {
                self.attach(&w, &plan.spec.name).await;
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn mark_downloading(&self, server: &str) {
        for w in lock(&self.inner.workspaces).values() {
            for plan in lock(&w.servers).values_mut() {
                if plan.spec.name == server {
                    plan.install = InstallState::Downloading;
                }
            }
        }
    }

    async fn changed(&self, root: &Path, path: &str) {
        let Ok(w) = self.ws(root) else {
            return;
        };
        let Ok(path) = normalize(path) else {
            tracing::debug!(target: "code_intel", path, "file_changed: path outside the workspace");
            return;
        };
        let (r, p, lang_of) = (w.root.clone(), path.clone(), w.language_of.clone());
        match blocking(move || file_entry(&r, &p, &*lang_of).map_err(CodeIntelError::from)).await {
            Ok(Some(entry)) => w.index.update_file(entry),
            Ok(None) => w.index.remove_file(&path),
            Err(e) => tracing::warn!(target: "code_intel", path, error = %e, "file_changed"),
        }
        // Tell a running server; do not start one for this.
        let Some(plan) = w.language(&path).and_then(|l| w.plan(&l)) else {
            return;
        };
        if !allowed(self.policy_for(&w), w.trust) {
            return;
        }
        if let Some(s) = self.pool().get(&w.pool_key, &w.root, &plan.spec.name).await
            && let Err(e) = s.file_changed(&w.root, &path).await
        {
            tracing::debug!(target: "code_intel", path, error = %e, "file_changed: server");
        }
    }

    async fn status_of(&self, root: &Path) -> CodeIntelResult<Vec<LanguageStatus>> {
        let w = self.ws(root)?;
        let policy = self.policy_for(&w);
        let snap = w.index.snapshot();
        let progress = w.index.progress();
        let running: BTreeMap<String, ServerState> =
            self.pool().status(&w.root).await.into_iter().collect();
        let mut files: BTreeMap<LanguageId, Vec<String>> = BTreeMap::new();
        for l in w.languages() {
            files.entry(l).or_default();
        }
        for f in snap.files() {
            if let Some(l) = &f.language {
                files.entry(l.clone()).or_default().push(f.blob_sha.clone());
            }
        }
        let index = self.index().clone();
        let missing = progress.missing_languages.clone();
        let indexed: BTreeMap<LanguageId, u8> = {
            let files = files.clone();
            blocking(move || {
                Ok(files
                    .into_iter()
                    .map(|(l, shas)| {
                        let pct = if missing.contains(&l) {
                            0
                        } else if shas.is_empty() {
                            100
                        } else {
                            let done = shas
                                .iter()
                                .filter(|s| index.is_indexed(s).unwrap_or(false))
                                .count();
                            (done * 100 / shas.len()) as u8
                        };
                        (l, pct)
                    })
                    .collect())
            })
            .await?
        };
        let mut out: Vec<LanguageStatus> = files
            .into_iter()
            .map(|(language, shas)| {
                let server = w.plan(&language).map(|p| ServerStatus {
                    state: if allowed(policy, w.trust) {
                        running
                            .get(&p.spec.name)
                            .copied()
                            .unwrap_or(ServerState::NotRunning)
                    } else {
                        ServerState::NotRunning
                    },
                    name: p.spec.name,
                    install: p.install,
                });
                LanguageStatus {
                    grammar: self.inner.grammars.state(&language),
                    index_percent: indexed.get(&language).copied().unwrap_or(0),
                    files: shas.len(),
                    server,
                    language,
                }
            })
            .collect();
        out.sort_by(|a, b| {
            b.files
                .cmp(&a.files)
                .then_with(|| a.language.cmp(&b.language))
        });
        Ok(out)
    }
}

/// The best layer that contributed.
fn best(lsp: bool, syntax: bool) -> Source {
    if lsp {
        Source::Lsp
    } else if syntax {
        Source::Syntax
    } else {
        Source::Text
    }
}

/// `LspSession::diagnostics` driven on a blocking thread. Its future fails
/// rustc's higher-ranked `Send` check (it holds `iter().map(|(p, _)| …)`
/// across an await; rust-lang/rust#102211), so it cannot live inside an
/// `async_trait` future; `block_on` there needs no `Send`. Collecting
/// that iterator in `code-intel-lsp` would make this a plain call.
async fn session_diagnostics(
    s: &Arc<LspSession>,
    root: &Path,
    path: Option<&str>,
) -> CodeIntelResult<Vec<Diagnostic>> {
    let (s, root, path) = (s.clone(), root.to_path_buf(), path.map(str::to_string));
    let rt = tokio::runtime::Handle::current();
    blocking(move || rt.block_on(s.diagnostics(&root, path.as_deref()))).await
}

/// Runs blocking work (SQLite, git, file reads) off the async threads.
async fn blocking<T, F>(f: F) -> CodeIntelResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> CodeIntelResult<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| CodeIntelError::Other(format!("code intel task: {e}")))?
}

#[async_trait]
impl CodeIntelPort for CodeIntel {
    async fn open(&self, ws: Workspace) -> CodeIntelResult<()> {
        self.open_ws(ws).await
    }

    async fn close(&self, root: &Path) {
        self.close_ws(root).await
    }

    async fn find_symbol(
        &self,
        root: &Path,
        name: &str,
        kind: Option<CodeSymbolKind>,
    ) -> CodeIntelResult<Answer<CodeSymbol>> {
        self.find_symbol_in(root, name, kind).await
    }

    async fn file_symbols(&self, root: &Path, path: &str) -> CodeIntelResult<Answer<CodeSymbol>> {
        self.file_symbols_in(root, path).await
    }

    async fn symbol_body(
        &self,
        root: &Path,
        path: &str,
        name: &str,
    ) -> CodeIntelResult<(CodeSymbol, String)> {
        self.symbol_body_in(root, path, name).await
    }

    async fn definition(
        &self,
        root: &Path,
        path: &str,
        at: Position,
    ) -> CodeIntelResult<Answer<CodeHit>> {
        self.definition_in(root, path, at).await
    }

    async fn references(
        &self,
        root: &Path,
        path: &str,
        at: Position,
    ) -> CodeIntelResult<Answer<CodeHit>> {
        self.references_in(root, path, at).await
    }

    async fn diagnostics(
        &self,
        root: &Path,
        path: Option<&str>,
    ) -> CodeIntelResult<Answer<Diagnostic>> {
        self.diagnostics_in(root, path).await
    }

    async fn rename(
        &self,
        root: &Path,
        path: &str,
        at: Position,
        new_name: &str,
    ) -> CodeIntelResult<Vec<TextEdit>> {
        self.rename_in(root, path, at, new_name).await
    }

    async fn install(&self, language: &LanguageId) -> CodeIntelResult<()> {
        self.install_language(language).await
    }

    async fn file_changed(&self, root: &Path, path: &str) {
        self.changed(root, path).await
    }

    async fn status(&self, root: &Path) -> CodeIntelResult<Vec<LanguageStatus>> {
        self.status_of(root).await
    }
}
