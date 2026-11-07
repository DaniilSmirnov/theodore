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
//! Trust: [`LspPolicy`] has one switch per [`TrustMode`](theodore_ports::TrustMode) — review runs
//! (off by default: most servers can run a PR's code) and spaces (on). The
//! one gate is `servers::allowed`; [`CodeIntel::set_options`] applies a
//! change live, stopping the sessions it no longer allows.
//!
//! Upkeep (docs/code-intel.md §5):
//!
//! ```text
//! set_limits ─▶ pool restarts (max_servers, idle) · indexer size limit (a larger one retries skips)
//! open · close · every 10 min ─▶ evict_cache: index.sqlite under cache_bytes, LRU, open worktrees last
//! install(lang, progress) ─▶ grammar, server: downloading… (≤ 4/s) ─▶ installed | failed
//! remove(lang) ─▶ in an open worktree? refuse ─▶ stop sessions ─▶ delete ─▶ removed.json (no auto-fetch)
//! ```

mod grammars;
mod paths;
mod servers;
mod workspace;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use theodore_code_intel_lsp::{Located, LspPool, LspSession, SessionOptions};
use theodore_code_intel_syntax::{
    Indexer, IndexerConfig, SyntaxIndex, byte_offset, dirty_paths, file_entry, git_snapshot,
    preview, text_search,
};
use theodore_code_intel_toolchain::{OnBytes, Toolchain};
use theodore_ports::code_intel::{
    Answer, CodeHit, CodeIntelError, CodeIntelLimits, CodeIntelPort, CodeIntelResult, CodeSymbol,
    CodeSymbolKind, Diagnostic, Hover, InstallItem, InstallProgress, InstallProgressFn,
    InstallProgressState, InstallState, LanguageId, LanguageStatus, Location, LspPolicy, Position,
    ServerState, ServerStatus, Source, TextEdit, Workspace,
};

pub use grammars::GrammarSource;
pub use theodore_code_intel_syntax::Eviction;

use crate::grammars::Removed;

use crate::paths::{canonical, normalize, read_inside};
use crate::servers::{allowed, denied_note};
use crate::workspace::{Gap, Ws, is_code, language_of, lock};

/// Most hits one answer carries.
const MAX_HITS: usize = 200;
/// How long a query about one file waits for it to be indexed.
const INDEX_WAIT: Duration = Duration::from_secs(2);
/// Names from the focus files used to promote files that mention them.
const MAX_FOCUS_NAMES: usize = 64;
/// Files over the size limit read for an empty answer's note, at most.
const NOTE_SKIPPED_SCAN: usize = 20;
/// Files `git status` lists that a name question hashes again, at most.
const MAX_DIRTY: usize = 2000;

/// How often the index is checked against its size cap, besides on every
/// open and close.
const EVICT_EVERY: Duration = Duration::from_secs(10 * 60);
/// Lines of a definition the syntax layer's hover shows at most.
const SIGNATURE_LINES: usize = 4;
/// Install progress events per item, at most one per this.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

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
    /// `index.sqlite` is kept under this (`code_intel.cache_mb`): least
    /// recently used blobs are evicted, open worktrees' last.
    pub cache_bytes: u64,
    /// Where language servers may run (`code_intel.lsp.review` /
    /// `code_intel.lsp.spaces`).
    pub lsp: LspPolicy,
}

impl CodeIntelOptions {
    /// The defaults of docs/code-intel.md §9.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            lsp: LspPolicy::default(),
            max_servers: 0,
            idle: Duration::ZERO,
            max_file_kb: 0,
            cache_bytes: 0,
        }
        .with_limits(CodeIntelLimits::default())
    }

    /// The machine-wide limits of the global config.
    pub fn with_limits(self, l: CodeIntelLimits) -> Self {
        Self {
            max_servers: l.max_servers.max(1),
            idle: Duration::from_secs(u64::from(l.idle_minutes.max(1)) * 60),
            max_file_kb: l.max_file_kb.max(1),
            cache_bytes: l.cache_mb.max(MIN_CACHE_MB).saturating_mul(1024 * 1024),
            ..self
        }
    }
}

/// A smaller `cache_mb` would evict the open worktrees' own blobs over and
/// over.
pub const MIN_CACHE_MB: u64 = 16;

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
        let removed = Arc::new(Removed::load(o.data_dir.join("removed.json")));
        let provider = grammars::provider(
            grammars.clone(),
            toolchain.manifest().clone(),
            removed.clone(),
        );
        let pool = LspPool::with_options(o.max_servers, o.idle, self.session.clone());
        let ci = CodeIntel {
            inner: Arc::new(Inner {
                indexer: Indexer::new(index, provider, config),
                pool: Mutex::new(Arc::new(pool)),
                session: self.session,
                options: Mutex::new(o),
                toolchain,
                grammars,
                removed,
                evicting: AtomicBool::new(false),
                workspaces: Mutex::default(),
                prefer: Mutex::default(),
            }),
        };
        let weak = Arc::downgrade(&ci.inner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(EVICT_EVERY).await;
                let Some(inner) = weak.upgrade() else { return };
                if let Err(e) = (CodeIntel { inner }).evict_cache().await {
                    tracing::warn!(target: "code_intel", error = %e, "index eviction failed");
                }
            }
        });
        Ok(ci)
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
    /// Languages removed, not fetched again on their own.
    removed: Arc<Removed>,
    /// One eviction at a time.
    evicting: AtomicBool,
    /// By canonical root.
    workspaces: Mutex<HashMap<PathBuf, Arc<Ws>>>,
    /// `Workspace::servers` of each open root: language → preferred server.
    prefer: Mutex<HashMap<PathBuf, BTreeMap<String, String>>>,
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

    /// A server's failure. One that only doesn't answer this kind of
    /// question (`Unsupported`, "method not found") is no news.
    fn error(&mut self, e: CodeIntelError) {
        if !matches!(e, CodeIntelError::Unsupported(_)) {
            self.push(e.to_string());
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
    /// disk), `max_servers` and `idle` (the pool restarts), `max_file_kb`
    /// (files parsed from now on; a larger limit retries the files skipped
    /// under the old one) and `cache_bytes` (a smaller cap evicts now).
    /// `data_dir` takes effect on the next start.
    pub async fn set_options(&self, options: CodeIntelOptions) {
        let old = std::mem::replace(&mut *lock(&self.inner.options), options.clone());
        if old.data_dir != options.data_dir {
            tracing::info!(target: "code_intel", "data_dir change applies after a restart");
        }
        if old.max_file_kb != options.max_file_kb {
            self.inner
                .indexer
                .set_max_file_bytes(options.max_file_kb.saturating_mul(1024));
            if options.max_file_kb > old.max_file_kb {
                for w in lock(&self.inner.workspaces).values() {
                    w.index.retry_too_large();
                }
            }
        }
        if options.cache_bytes < old.cache_bytes {
            self.spawn_evict();
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
        for w in open.into_iter().filter(|w| lock(&w.lsp).is_none()) {
            let (was, now) = (allowed(old.lsp, w.trust), allowed(options.lsp, w.trust));
            self.apply_switch(&w, was, now, new_pool).await;
        }
    }

    /// A worktree's policy went `was` → `now`: its servers follow.
    async fn apply_switch(&self, w: &Ws, was: bool, now: bool, restart: bool) {
        if !now {
            if was {
                self.pool().release_root(&w.root).await;
            }
            for plan in lock(&w.servers).values_mut() {
                plan.resolved = None;
            }
        } else if !was || restart {
            self.start_servers(w).await;
        }
    }

    /// [`set_options`](Self::set_options) with the global config's limits.
    pub async fn set_limits(&self, limits: CodeIntelLimits) {
        let options = self.options().with_limits(limits);
        self.set_options(options).await;
    }

    /// Bytes `index.sqlite` holds and the blobs in it.
    pub async fn cache_usage(&self) -> CodeIntelResult<(u64, usize)> {
        let index = self.index().clone();
        blocking(move || Ok((index.used_bytes()?, index.counts()?.0))).await
    }

    /// Keeps `index.sqlite` under `cache_bytes` (`SyntaxIndex::evict`):
    /// blobs of no open worktree go first; open worktrees forget the
    /// blobs of theirs that had to go too, and fill them in again on
    /// demand. One at a time: a call while one runs returns at once, empty.
    pub async fn evict_cache(&self) -> CodeIntelResult<Eviction> {
        if self.inner.evicting.swap(true, Ordering::SeqCst) {
            return Ok(Eviction::default());
        }
        struct Done<'a>(&'a AtomicBool);
        impl Drop for Done<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _done = Done(&self.inner.evicting);
        let cap = lock(&self.inner.options).cache_bytes;
        let open: Vec<Arc<Ws>> = lock(&self.inner.workspaces).values().cloned().collect();
        let in_use: HashSet<String> = open
            .iter()
            .flat_map(|w| {
                w.index
                    .snapshot()
                    .files()
                    .iter()
                    .map(|f| f.blob_sha.clone())
                    .collect::<Vec<_>>()
            })
            .collect();
        let index = self.index().clone();
        let ev = blocking(move || Ok(index.evict(cap, &in_use)?)).await?;
        if !ev.in_use.is_empty() {
            for w in &open {
                w.index.forget(&ev.in_use);
            }
        }
        Ok(ev)
    }

    fn spawn_evict(&self) {
        let me = self.clone();
        tokio::spawn(async move {
            if let Err(e) = me.evict_cache().await {
                tracing::warn!(target: "code_intel", error = %e, "index eviction failed");
            }
        });
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
        lock(&w.lsp).unwrap_or_else(|| self.policy())
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
                && existing.requested == ws.languages
                && lock(&self.inner.prefer).get(&root) == Some(&ws.servers);
            if same {
                self.focus(&existing, focus);
                return Ok(());
            }
            self.close_ws(&root).await;
        }

        let servers_wanted = ws.servers.clone();
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
            lsp: Mutex::new(ws.lsp),
            requested: ws.languages,
            language_of: lang_of,
            servers: Mutex::default(),
            dirty: Mutex::default(),
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
        lock(&self.inner.prefer).insert(root.clone(), servers_wanted);
        self.focus(&w, focus);
        tracing::info!(target: "code_intel", root = %root.display(), trust = ?w.trust, languages = ?w.languages(), "workspace open");
        self.start_servers(&w).await;
        self.spawn_evict();
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

    /// [`CodeIntelPort::warm`]. Stops at the cap: warming must not push out
    /// a server someone is using, only to be pushed out in turn.
    async fn warm_ws(&self, root: &Path) {
        let Ok(w) = self.ws(root) else { return };
        if !allowed(self.policy_for(&w), w.trust) {
            return;
        }
        let pool = self.pool();
        for server in w.servers_by_name().into_keys() {
            if pool.get(&w.pool_key, &w.root, &server).await.is_none() && pool.has_room().await {
                self.attach(&w, &server).await;
            }
        }
    }

    /// [`CodeIntelPort::set_lsp`].
    async fn set_ws_lsp(&self, root: &Path, on: bool) {
        let Ok(w) = self.ws(root) else { return };
        let was = allowed(self.policy_for(&w), w.trust);
        *lock(&w.lsp) = Some(LspPolicy {
            review: on,
            spaces: on,
        });
        self.apply_switch(&w, was, on, false).await;
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
        let prefer = lock(&self.inner.prefer)
            .get(&w.root)
            .and_then(|p| p.get(lang.as_str()).cloned());
        let Some(plan) = servers::plan(
            &self.inner.toolchain,
            lang,
            prefer.as_deref(),
            &w.root,
            policy,
            w.trust,
        )
        .await
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
            lock(&self.inner.prefer).remove(&w.root);
            w.shut();
            self.pool().release_root(&w.root).await;
            self.spawn_evict();
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
            return Route::Not(Some(servers::not_resolved_note(&plan)));
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
            ServerState::Off | ServerState::NotRunning | ServerState::Failed => {
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

    /// `path` as it is on disk now: hashed again, and reindexed when its
    /// content is not the snapshot's (or removed when gone). A file the
    /// agent wrote with its own Edit tool reaches here unannounced.
    async fn refresh_file(&self, w: &Ws, path: &str) {
        let had = w.index.snapshot().get(path).map(|f| f.blob_sha.clone());
        let (r, p, lang_of) = (w.root.clone(), path.to_string(), w.language_of.clone());
        match blocking(move || file_entry(&r, &p, &*lang_of).map_err(CodeIntelError::from)).await {
            Ok(Some(entry)) if had.as_ref() != Some(&entry.blob_sha) => w.index.update_file(entry),
            Ok(None) if had.is_some() => w.index.remove_file(path),
            Ok(_) => {}
            Err(e) => tracing::warn!(target: "code_intel", path, error = %e, "re-reading a file"),
        }
    }

    /// Files written since the snapshot and never reported (a new file of
    /// this turn, an edit by the agent's own tool): what `git status` lists
    /// now, and what it listed last time, is hashed again; changed ones go
    /// into the snapshot and are indexed first (briefly waited for). Ignored
    /// files stay out, as in the snapshot itself.
    async fn refresh_dirty(&self, w: &Ws) {
        let before: Vec<String> = lock(&w.dirty).iter().cloned().collect();
        let (root, lang_of) = (w.root.clone(), w.language_of.clone());
        let read = blocking(move || {
            let now = dirty_paths(&root).map_err(CodeIntelError::from)?;
            let mut paths: Vec<String> = now.clone();
            paths.extend(before.into_iter().filter(|p| !now.contains(p)));
            paths.truncate(MAX_DIRTY);
            let entries = paths
                .into_iter()
                .map(|p| {
                    let e = file_entry(&root, &p, &*lang_of).ok().flatten();
                    (p, e)
                })
                .collect::<Vec<_>>();
            Ok((now, entries))
        })
        .await;
        let (now, entries) = match read {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(target: "code_intel", error = %e, "git status");
                return;
            }
        };
        *lock(&w.dirty) = now.into_iter().collect();
        let snap = w.index.snapshot();
        let mut files: BTreeMap<String, theodore_code_intel_syntax::FileEntry> = snap
            .files()
            .iter()
            .map(|f| (f.path.clone(), f.clone()))
            .collect();
        let mut changed = Vec::new();
        for (path, entry) in entries {
            let had = files.get(&path).map(|f| f.blob_sha.clone());
            match entry {
                Some(e) if had.as_ref() != Some(&e.blob_sha) => {
                    changed.push(path.clone());
                    files.insert(path, e);
                }
                None if had.is_some() => {
                    files.remove(&path);
                }
                _ => {}
            }
        }
        if files.len() == snap.files().len() && changed.is_empty() {
            return;
        }
        w.index
            .update_snapshot(theodore_code_intel_syntax::Snapshot::new(
                snap.root().to_path_buf(),
                files.into_values().collect(),
            ));
        let _ = tokio::time::timeout(INDEX_WAIT, w.index.index_now(&changed)).await;
    }

    /// Wait (briefly) until `path`, as it is on disk now, is indexed.
    async fn wait_indexed(&self, w: &Ws, path: &str) {
        self.refresh_file(w, path).await;
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
        self.refresh_dirty(&w).await;
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
                    Err(e) => notes.error(e),
                },
                Route::Not(n) => notes.extend(n),
            }
        }
        // A Markdown heading or a TOML key is not a definition of code; the
        // file's own symbols still list them.
        let manifest = self.inner.toolchain.manifest();
        let code = |s: &CodeSymbol| w.language(&s.path).is_none_or(|l| is_code(manifest, &l));
        let (lsp, mut prose): (Vec<CodeSymbol>, Vec<CodeSymbol>) = lsp.into_iter().partition(code);
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
        let (mut items, more_prose): (Vec<CodeSymbol>, Vec<CodeSymbol>) =
            items.into_iter().partition(code);
        prose.extend(more_prose);
        items.truncate(MAX_HITS);
        if items.is_empty() {
            if !prose.is_empty() {
                let mut paths: Vec<&str> = prose.iter().map(|s| s.path.as_str()).collect();
                paths.dedup();
                let more = if paths.len() > NOTE_EXTERNAL {
                    ", …"
                } else {
                    ""
                };
                paths.truncate(NOTE_EXTERNAL);
                notes.push(format!(
                    "in prose or data files, not code: {}{more}",
                    paths.join(", ")
                ));
            }
            notes.extend(self.skipped_mentions(&w, name).await);
        }
        Ok(self.answer(&w, items, source, notes))
    }

    /// "not searched, over code_intel.max_file_kb: a.ts, …": the code files
    /// left out for their size that mention `name`, so an empty answer
    /// doesn't read as "defined nowhere".
    async fn skipped_mentions(&self, w: &Ws, name: &str) -> Option<String> {
        let manifest = self.inner.toolchain.manifest();
        let big: Vec<String> = w
            .index
            .progress()
            .too_large
            .into_iter()
            .filter(|p| w.language(p).is_some_and(|l| is_code(manifest, &l)))
            .take(NOTE_SKIPPED_SCAN)
            .collect();
        if big.is_empty() || name.is_empty() {
            return None;
        }
        let (root, name) = (w.root.clone(), name.to_string());
        let found = blocking(move || {
            Ok(big
                .into_iter()
                .filter(|p| read_inside(&root, p).is_ok_and(|t| t.contains(&name)))
                .collect::<Vec<_>>())
        })
        .await
        .unwrap_or_default();
        if found.is_empty() {
            return None;
        }
        let shown: Vec<&str> = found
            .iter()
            .take(NOTE_EXTERNAL)
            .map(String::as_str)
            .collect();
        let more = if found.len() > shown.len() {
            ", …"
        } else {
            ""
        };
        Some(format!(
            "not searched, over code_intel.max_file_kb: {}{more}",
            shown.join(", ")
        ))
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
                Err(e) => notes.error(e),
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
                // Only outside the worktree (std, a dependency): the server's
                // answer still stands; a same-named local symbol would be wrong.
                Ok(found) if !found.items.is_empty() || found.external_count > 0 => {
                    notes.extend(external_note(&found, true));
                    let hits = self.lsp_hits(&w, found.items, true).await;
                    return Ok(self.answer(&w, hits, Source::Lsp, notes));
                }
                Ok(_) => notes.push(format!("{}: no definition; matched by name", s.server())),
                Err(e) => notes.error(e),
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
                Ok(found) if !found.items.is_empty() => {
                    notes.extend(external_note(&found, false));
                    let hits = self.lsp_hits(&w, found.items, false).await;
                    return Ok(self.answer(&w, hits, Source::Lsp, notes));
                }
                Ok(_) => notes.push(format!("{}: no references; matched by name", s.server())),
                Err(e) => notes.error(e),
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
                            Err(e) => notes.error(e),
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
                                Err(e) => notes.error(e),
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

    /// Hover text at `at`, for the diff view: the language server's when
    /// one answers, else the signature of the best definition with the
    /// name under the cursor (`Source::Syntax`). `None`: neither has one.
    pub async fn hover_at(
        &self,
        root: &Path,
        path: &str,
        at: Position,
    ) -> CodeIntelResult<Option<Hover>> {
        let w = self.ws(root)?;
        let path = normalize(path)?;
        if let Route::Ready(s) = self.route(&w, w.language(&path).as_ref()).await {
            match s.hover(&w.root, &path, at).await {
                Ok(Some(text)) => {
                    return Ok(Some(Hover {
                        text,
                        source: Source::Lsp,
                    }));
                }
                Ok(None) => {}
                Err(e) => tracing::debug!(target: "code_intel", error = %e, "hover: server"),
            }
        }
        self.wait_indexed(&w, &path).await;
        let (snap, index, p) = (w.index.snapshot(), self.index().clone(), path.clone());
        let found = blocking(move || {
            let Some(hit) = index.definition(&snap, &p, at, 1)?.into_iter().next() else {
                return Ok(None);
            };
            let text = read_inside(snap.root(), &hit.location.path).ok();
            Ok(text.map(|t| (signature(&t, hit.location.range.start.line), hit.location)))
        })
        .await?;
        let Some((sig, loc)) = found.filter(|(sig, _)| !sig.is_empty()) else {
            return Ok(None);
        };
        let fence = w
            .language(&loc.path)
            .map(|l| l.as_str().to_string())
            .unwrap_or_default();
        Ok(Some(Hover {
            text: format!(
                "```{fence}\n{sig}\n```\n\n_Matched by name, no language server: {}:{}_",
                loc.path, loc.range.start.line
            ),
            source: Source::Syntax,
        }))
    }

    // ── install / remove / changes / status ───────────────────────────────

    async fn install_language(
        &self,
        language: &LanguageId,
        progress: Option<InstallProgressFn>,
    ) -> CodeIntelResult<()> {
        let tc = self.inner.toolchain.clone();
        let spec =
            tc.manifest().language(language).cloned().ok_or_else(|| {
                CodeIntelError::Unsupported(format!("unknown language {language}"))
            })?;
        let mut first_error: Option<CodeIntelError> = None;
        // Asked for again: it may be fetched on its own again, too.
        let mut again = vec![language.clone()];
        again.extend(spec.tags_extends.clone());
        if let Some(g) = &spec.grammar {
            again.extend(g.languages.iter().map(|l| l.id.clone()));
        }
        self.inner.removed.forget(&again);

        // The grammar, and the one its tags query extends.
        let grammar = match &spec.grammar {
            Some(g) => {
                let report =
                    Reporter::new(progress.clone(), language, InstallItem::Grammar, &g.package);
                let on_bytes = report.on_bytes();
                let result = self
                    .inner
                    .grammars
                    .grammar_with(language, on_bytes.as_deref())
                    .await;
                report.finish(&result);
                result
            }
            None => self.inner.grammars.grammar(language).await,
        };
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

        // The preferred server, downloaded only if it is not on the machine
        // and can run here. A project that names one wins (the first open
        // worktree of the language that does).
        let prefer: Option<String> = lock(&self.inner.prefer)
            .values()
            .find_map(|p| p.get(language.as_str()).cloned());
        let mut fetched: Option<String> = None;
        if let Some((server, state)) = tc.server_to_install(language, prefer.as_deref()).await {
            let fetch = !matches!(
                state,
                InstallState::System { .. }
                    | InstallState::Installed { .. }
                    | InstallState::NeedsRuntime { .. }
                    | InstallState::TrustedOnly
            );
            if fetch && server.download_for(tc.platform()).is_some() {
                self.mark_downloading(&server.name);
                fetched = Some(server.name.clone());
                let report = Reporter::new(
                    progress.clone(),
                    language,
                    InstallItem::Server,
                    &server.name,
                );
                let on_bytes = report.on_bytes();
                let result = tc.install_server_with(&server, on_bytes.as_deref()).await;
                report.finish(&result);
                if let Err(e) = result {
                    first_error.get_or_insert(e);
                }
            }
        }

        // Every open workspace with the language picks both up.
        let open: Vec<Arc<Ws>> = lock(&self.inner.workspaces).values().cloned().collect();
        for w in open {
            // `mark_downloading` marked every language of that server, in
            // every worktree: each has to read where it stands now.
            let sharing: Vec<LanguageId> = lock(&w.servers)
                .iter()
                .filter(|(l, p)| *l != language && Some(&p.spec.name) == fetched.as_ref())
                .map(|(l, _)| l.clone())
                .collect();
            for l in &sharing {
                self.plan_server(&w, l).await;
            }
            if w.languages().contains(language) {
                let progress = w.index.progress();
                if grammar_ok && progress.missing_languages.contains(language) {
                    w.index.refresh_grammar(language);
                }
                self.plan_server(&w, language).await;
                if let Some(plan) = w.plan(language) {
                    self.attach(&w, &plan.spec.name).await;
                }
            } else if let Some(server) = fetched.as_deref().filter(|_| !sharing.is_empty()) {
                self.attach(&w, server).await;
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// See [`CodeIntelPort::remove`].
    async fn remove_language(&self, language: &LanguageId) -> CodeIntelResult<()> {
        let tc = self.inner.toolchain.clone();
        let spec =
            tc.manifest().language(language).cloned().ok_or_else(|| {
                CodeIntelError::Unsupported(format!("unknown language {language}"))
            })?;
        // Every language the grammar package parses goes with it.
        let mut parsed: Vec<LanguageId> = spec
            .grammar
            .as_ref()
            .map(|g| g.languages.iter().map(|l| l.id.clone()).collect())
            .unwrap_or_default();
        if !parsed.contains(language) {
            parsed.push(language.clone());
        }
        let open: Vec<Arc<Ws>> = lock(&self.inner.workspaces).values().cloned().collect();
        let users: Vec<PathBuf> = open
            .iter()
            .filter(|w| {
                let langs = w.languages();
                parsed.iter().any(|l| langs.contains(l))
            })
            .map(|w| w.root.clone())
            .collect();
        if !users.is_empty() {
            return Err(CodeIntelError::InUse {
                language: language.to_string(),
                roots: users,
            });
        }

        // Servers: every one of the language Theodore downloads. Their
        // sessions stop wherever they run (a server can serve several
        // languages).
        let names: Vec<String> = spec
            .servers
            .iter()
            .filter(|s| s.download.is_some())
            .map(|s| s.name.clone())
            .collect();
        for name in &names {
            self.pool().stop_server(name).await;
        }
        let grammars = self.inner.grammars.clone();
        let (lang, tc2, names2) = (language.clone(), tc.clone(), names.clone());
        let removed_langs = blocking(move || {
            for name in &names2 {
                tc2.remove_server(name)?;
            }
            grammars.remove(&lang)
        })
        .await?;
        let mut sticky = parsed;
        for l in removed_langs {
            if !sticky.contains(&l) {
                sticky.push(l);
            }
        }
        self.inner.removed.insert(&sticky);

        // Open worktrees whose plans named a removed server: Missing now.
        for w in &open {
            let langs: Vec<LanguageId> = lock(&w.servers)
                .iter()
                .filter(|(_, p)| names.contains(&p.spec.name))
                .map(|(l, _)| l.clone())
                .collect();
            for l in langs {
                self.plan_server(w, &l).await;
            }
        }
        tracing::info!(target: "code_intel", %language, servers = ?names, "language removed");
        Ok(())
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
        self.refresh_file(&w, &path).await;
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
                    hint: p.spec.hint.clone(),
                    state: if allowed(policy, w.trust) {
                        running
                            .get(&p.spec.name)
                            .copied()
                            .unwrap_or(ServerState::NotRunning)
                    } else {
                        ServerState::Off
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

/// Turns one install item's download into [`InstallProgress`] for the
/// port's listener: `Downloading` at most every [`PROGRESS_EVERY`] (and
/// the last chunk), then `Installed` or `Failed` once.
struct Reporter {
    sink: Option<InstallProgressFn>,
    language: LanguageId,
    what: InstallItem,
    name: String,
    last: Mutex<Option<Instant>>,
}

impl Reporter {
    fn new(
        sink: Option<InstallProgressFn>,
        language: &LanguageId,
        what: InstallItem,
        name: &str,
    ) -> Arc<Self> {
        Arc::new(Self {
            sink,
            language: language.clone(),
            what,
            name: name.to_string(),
            last: Mutex::new(None),
        })
    }

    fn send(&self, state: InstallProgressState) {
        if let Some(sink) = &self.sink {
            sink(InstallProgress {
                language: self.language.clone(),
                what: self.what,
                name: self.name.clone(),
                state,
            });
        }
    }

    /// The toolchain's byte listener; `None` without a sink.
    fn on_bytes(self: &Arc<Self>) -> Option<Box<OnBytes>> {
        self.sink.as_ref()?;
        let me = self.clone();
        Some(Box::new(move |received: u64, total: Option<u64>| {
            let done = total == Some(received) && received > 0;
            {
                let mut last = lock(&me.last);
                if !done && last.is_some_and(|t| t.elapsed() < PROGRESS_EVERY) {
                    return;
                }
                *last = Some(Instant::now());
            }
            me.send(InstallProgressState::Downloading { received, total });
        }))
    }

    fn finish<T>(&self, result: &CodeIntelResult<T>) {
        self.send(match result {
            Ok(_) => InstallProgressState::Installed,
            Err(e) => InstallProgressState::Failed {
                error: e.to_string(),
            },
        });
    }
}

/// A definition's signature: its first line in `text` (1-based `line`) and
/// the next ones until the body opens (`{`, a trailing `:` or `;`, `=`),
/// a blank line, or [`SIGNATURE_LINES`].
fn signature(text: &str, line: u32) -> String {
    let mut out: Vec<&str> = Vec::new();
    for l in text
        .lines()
        .skip(line.saturating_sub(1) as usize)
        .take(SIGNATURE_LINES)
    {
        let l = l.trim_end();
        if l.trim().is_empty() {
            break;
        }
        if let Some(i) = l.find('{') {
            let head = l[..i].trim_end();
            if !head.trim().is_empty() {
                out.push(head);
            }
            break;
        }
        out.push(l);
        if l.ends_with(':') || l.ends_with(';') || l.ends_with('=') || l.ends_with("=>") {
            break;
        }
    }
    // Keep the indentation relative to the first line.
    let indent = out.first().map_or(0, |l| l.len() - l.trim_start().len());
    out.iter()
        .map(|l| l.get(indent..).unwrap_or(l.trim_start()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Paths an external-results note names at most.
const NOTE_EXTERNAL: usize = 3;

/// "2 results outside the worktree (std / dependencies): /…/vec.rs, …":
/// their paths are not repo-relative, so they stay out of `items`.
/// `name`: list the first few paths (definitions); references only count.
fn external_note(found: &Located, name: bool) -> Option<String> {
    let n = found.external_count;
    if n == 0 {
        return None;
    }
    let what = if n == 1 { "result" } else { "results" };
    let mut note = format!("{n} {what} outside the worktree (std / dependencies)");
    if name && !found.external.is_empty() {
        let shown: Vec<&str> = found
            .external
            .iter()
            .take(NOTE_EXTERNAL)
            .map(String::as_str)
            .collect();
        note.push_str(": ");
        note.push_str(&shown.join(", "));
        if n > shown.len() {
            note.push_str(", …");
        }
    }
    Some(note)
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

    async fn hover(&self, root: &Path, path: &str, at: Position) -> CodeIntelResult<Option<Hover>> {
        self.hover_at(root, path, at).await
    }

    async fn install(
        &self,
        language: &LanguageId,
        progress: Option<InstallProgressFn>,
    ) -> CodeIntelResult<()> {
        self.install_language(language, progress).await
    }

    async fn remove(&self, language: &LanguageId) -> CodeIntelResult<()> {
        self.remove_language(language).await
    }

    async fn set_limits(&self, limits: CodeIntelLimits) {
        CodeIntel::set_limits(self, limits).await
    }

    async fn file_changed(&self, root: &Path, path: &str) {
        self.changed(root, path).await
    }

    async fn warm(&self, root: &Path) {
        self.warm_ws(root).await
    }

    async fn set_lsp(&self, root: &Path, on: bool) {
        self.set_ws_lsp(root, on).await
    }

    async fn status(&self, root: &Path) -> CodeIntelResult<Vec<LanguageStatus>> {
        self.status_of(root).await
    }
}
