//! One running language server and the documents it has open.
//!
//! ```text
//! start ─▶ spawn (stdio, cwd = root) ─▶ initialize ─▶ initialized    Warming
//!            │                                  │
//!            │      $/progress begin … end ─────┤ no work in flight for
//!            │      experimental/serverStatus ──┤ `ready_settle`, or quiescent,
//!            │                                  ▼ or `ready_max_wait` passed
//!            │                                Ready
//!            ├─ child exits on its own ───────▶ Failed   (pending requests error)
//!            └─ shutdown ─▶ exit ─▶ kill after `shutdown_grace` ─▶ NotRunning
//! ```
//!
//! Documents open lazily: a request on a file sends `didOpen` with its text
//! from disk first, and the least recently used one is closed beyond
//! `max_open_docs`. `file_changed` pushes the new text of an open file and
//! tells servers that watch files through the client.
//!
//! Server→client requests are answered so servers never stall waiting on
//! us: `workspace/configuration` from `init_options` (see
//! `convert::config_section`), `workspace/workspaceFolders` from the current
//! folders, and `client/registerCapability`, `window/workDoneProgress/create`
//! and the `*/refresh` requests with `null`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lsp_types::{
    DocumentChangeOperation, DocumentChanges, DocumentSymbol, DocumentSymbolResponse,
    GotoDefinitionResponse, OneOf, SymbolInformation, WorkspaceEdit, WorkspaceSymbolResponse,
};
use serde_json::{Value, json};
use theodore_ports::{
    CodeIntelError, CodeIntelResult, CodeSymbol, Diagnostic, Location, Position, ServerState,
    Source, TextEdit,
};
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;
use tokio::sync::{Notify, oneshot, watch};
use tokio::time::Instant;

use crate::LaunchConfig;
use crate::convert;
use crate::rpc::{self, Handler, Rpc, RpcError};
use crate::text::{self, PositionEncoding};

/// Timeouts and limits of one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOptions {
    /// How long one request may take.
    pub request_timeout: Duration,
    /// How long `initialize` may take.
    pub init_timeout: Duration,
    /// Quiet time with no progress in flight after which the server counts
    /// as warmed up.
    pub ready_settle: Duration,
    /// Ready after this long regardless of progress (a server that never
    /// ends its indexing token still answers).
    pub ready_max_wait: Duration,
    /// Documents kept open at once.
    pub max_open_docs: usize,
    /// Between `exit` and a kill.
    pub shutdown_grace: Duration,
    /// How long `diagnostics(path)` waits for a first `publishDiagnostics`
    /// of a file it just opened.
    pub diagnostics_wait: Duration,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            init_timeout: Duration::from_secs(60),
            ready_settle: Duration::from_millis(1500),
            ready_max_wait: Duration::from_secs(180),
            max_open_docs: 64,
            shutdown_grace: Duration::from_secs(5),
            diagnostics_wait: Duration::from_secs(2),
        }
    }
}

/// Indexing progress the readiness task watches.
#[derive(Debug)]
struct Progress {
    /// `$/progress` tokens between `begin` and `end`.
    active: HashSet<String>,
    /// The last `begin` / `end`, or `initialized`.
    last_change: Instant,
    /// The server sends `experimental/serverStatus`; trust it over timing.
    has_status: bool,
    quiescent: bool,
}

/// State the reader task's handler shares with the session.
struct Shared {
    server: String,
    init_options: Value,
    /// Canonical workspace roots, the first being the process's cwd.
    folders: Mutex<Vec<PathBuf>>,
    progress: Mutex<Progress>,
    progress_changed: Notify,
    /// The last `publishDiagnostics` (or pull report) per file.
    diagnostics: Mutex<HashMap<PathBuf, Vec<lsp_types::Diagnostic>>>,
    diagnostics_changed: Notify,
    /// The server registered `workspace/didChangeWatchedFiles`.
    watches_files: AtomicBool,
    /// `shutdown` was asked for: an exit now is not a crash.
    stopping: AtomicBool,
    state: watch::Sender<ServerState>,
}

impl Shared {
    fn fail(&self, why: &str) {
        if self.stopping.load(Ordering::SeqCst) {
            return;
        }
        let was = self.state.send_replace(ServerState::Failed);
        if was != ServerState::Failed {
            tracing::warn!(server = %self.server, "lsp: {why}");
        }
    }

    fn on_progress(&self, params: &Value) {
        let token = match &params["token"] {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let mut p = lock(&self.progress);
        match params["value"]["kind"].as_str() {
            Some("begin") => {
                p.active.insert(token);
            }
            Some("end") => {
                p.active.remove(&token);
            }
            _ => return,
        }
        p.last_change = Instant::now();
        drop(p);
        self.progress_changed.notify_one();
    }

    fn on_diagnostics(&self, params: Value) {
        let Ok(p) = serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(params) else {
            return;
        };
        let Some(path) = text::uri_path(p.uri.as_str()) else {
            return;
        };
        lock(&self.diagnostics).insert(path, p.diagnostics);
        self.diagnostics_changed.notify_waiters();
    }

    fn folders_json(&self) -> Value {
        Value::Array(lock(&self.folders).iter().map(|f| folder_json(f)).collect())
    }
}

impl Handler for Shared {
    fn request(&self, method: &str, params: Value) -> Result<Value, (i64, String)> {
        match method {
            "workspace/configuration" => {
                let items = params["items"].as_array().cloned().unwrap_or_default();
                Ok(Value::Array(
                    items
                        .iter()
                        .map(|it| {
                            convert::config_section(
                                &self.init_options,
                                &self.server,
                                it["section"].as_str(),
                            )
                        })
                        .collect(),
                ))
            }
            "client/registerCapability" => {
                let regs = params["registrations"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                if regs
                    .iter()
                    .any(|r| r["method"] == "workspace/didChangeWatchedFiles")
                {
                    self.watches_files.store(true, Ordering::SeqCst);
                }
                Ok(Value::Null)
            }
            "workspace/workspaceFolders" => Ok(self.folders_json()),
            "workspace/applyEdit" => Ok(json!({ "applied": false })),
            "client/unregisterCapability"
            | "window/workDoneProgress/create"
            | "window/showMessageRequest"
            | "window/showDocument"
            | "workspace/semanticTokens/refresh"
            | "workspace/inlayHint/refresh"
            | "workspace/inlineValue/refresh"
            | "workspace/codeLens/refresh"
            | "workspace/diagnostic/refresh"
            | "workspace/foldingRange/refresh" => Ok(Value::Null),
            _ => Err((rpc::METHOD_NOT_FOUND, format!("unhandled: {method}"))),
        }
    }

    fn notification(&self, method: &str, params: Value) {
        match method {
            "textDocument/publishDiagnostics" => self.on_diagnostics(params),
            "$/progress" => self.on_progress(&params),
            "experimental/serverStatus" => {
                let mut p = lock(&self.progress);
                p.has_status = true;
                p.quiescent = params["quiescent"].as_bool().unwrap_or(false);
                p.last_change = Instant::now();
                drop(p);
                self.progress_changed.notify_one();
            }
            "window/logMessage" | "window/showMessage" => {
                tracing::debug!(server = %self.server, message = %params["message"], "lsp log");
            }
            _ => {}
        }
    }

    fn closed(&self) {
        self.fail("server output closed");
    }
}

/// What `initialize` returned.
struct InitInfo {
    encoding: PositionEncoding,
    /// Raw `ServerCapabilities`: read leniently, servers vary.
    caps: Value,
}

impl InitInfo {
    fn has(&self, pointer: &str) -> bool {
        self.caps
            .pointer(pointer)
            .is_some_and(|v| !v.is_null() && *v != Value::Bool(false))
    }
}

struct Doc {
    version: i32,
    text: String,
    used: u64,
}

#[derive(Default)]
struct Docs {
    open: HashMap<PathBuf, Doc>,
    tick: u64,
}

/// One language server process. Created by [`LspPool`](crate::LspPool), or
/// directly with [`LspSession::start`].
pub struct LspSession {
    config: LaunchConfig,
    options: SessionOptions,
    shared: Arc<Shared>,
    rpc: Rpc,
    init: OnceLock<InitInfo>,
    docs: tokio::sync::Mutex<Docs>,
    kill: Mutex<Option<oneshot::Sender<()>>>,
    exited: watch::Receiver<bool>,
    last_used: Mutex<Instant>,
    inflight: Arc<AtomicUsize>,
}

/// Marks a session busy while an operation runs.
struct InFlight(Arc<AtomicUsize>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl LspSession {
    /// Spawn the server in `root` and start `initialize` in the background.
    /// Returns as soon as the process runs; the state is `Warming` until the
    /// server is ready. Must be called inside a tokio runtime.
    pub fn start(
        root: &Path,
        config: LaunchConfig,
        options: SessionOptions,
    ) -> CodeIntelResult<Arc<Self>> {
        let server = config.server.clone();
        let err = |message: String| CodeIntelError::Server {
            server: server.clone(),
            message,
        };
        let root = std::fs::canonicalize(root)
            .map_err(|e| err(format!("root {}: {e}", root.display())))?;
        let (program, args) = config
            .argv
            .split_first()
            .ok_or_else(|| err("empty argv".into()))?;
        let mut child = Command::new(program)
            .args(args)
            .envs(config.env.iter().cloned())
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| err(format!("spawn {program}: {e}")))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        if let Some(stderr) = child.stderr.take() {
            let server = server.clone();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(server = %server, "lsp stderr: {line}");
                }
            });
        }

        let (state, _) = watch::channel(ServerState::Warming);
        let shared = Arc::new(Shared {
            server: server.clone(),
            init_options: config.init_options.clone(),
            folders: Mutex::new(vec![root.clone()]),
            progress: Mutex::new(Progress {
                active: HashSet::new(),
                last_change: Instant::now(),
                has_status: false,
                quiescent: false,
            }),
            progress_changed: Notify::new(),
            diagnostics: Mutex::default(),
            diagnostics_changed: Notify::new(),
            watches_files: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            state,
        });
        let rpc = Rpc::start(stdout, stdin, shared.clone());

        // The monitor owns the child: it notices an exit, and kills on
        // request or when the session (holding `kill`) is dropped.
        let (kill_tx, kill_rx) = oneshot::channel::<()>();
        let (exited_tx, exited) = watch::channel(false);
        {
            let shared = shared.clone();
            tokio::spawn(async move {
                tokio::select! {
                    status = child.wait() => {
                        tracing::debug!(server = %shared.server, ?status, "lsp: exited");
                        shared.fail(&format!("exited: {status:?}"));
                    }
                    _ = kill_rx => {
                        let _ = child.kill().await;
                    }
                }
                let _ = exited_tx.send(true);
            });
        }

        let session = Arc::new(Self {
            config,
            options,
            shared,
            rpc,
            init: OnceLock::new(),
            docs: tokio::sync::Mutex::default(),
            kill: Mutex::new(Some(kill_tx)),
            exited,
            last_used: Mutex::new(Instant::now()),
            inflight: Arc::default(),
        });
        tokio::spawn(Arc::clone(&session).initialize(root));
        Ok(session)
    }

    async fn initialize(self: Arc<Self>, root: PathBuf) {
        let params = initialize_params(&root, &self.config);
        let result = self
            .rpc
            .request("initialize", params, self.options.init_timeout)
            .await;
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                self.shared.fail(&format!("initialize: {e}"));
                self.kill();
                return;
            }
        };
        let caps = result.get("capabilities").cloned().unwrap_or(Value::Null);
        let encoding = PositionEncoding::from_lsp(caps["positionEncoding"].as_str());
        let _ = self.init.set(InitInfo { encoding, caps });
        self.rpc.notify("initialized", json!({}));
        lock(&self.shared.progress).last_change = Instant::now();
        // Wake `wait_initialized` (the value stays `Warming`).
        self.shared
            .state
            .send_if_modified(|s| *s == ServerState::Warming);
        tokio::spawn(readiness(self.shared.clone(), self.options.clone()));
    }

    /// The server's name.
    pub fn server(&self) -> &str {
        &self.config.server
    }

    pub fn config(&self) -> &LaunchConfig {
        &self.config
    }

    pub fn state(&self) -> ServerState {
        *self.shared.state.borrow()
    }

    /// Every state change, starting from the current one.
    pub fn subscribe(&self) -> watch::Receiver<ServerState> {
        self.shared.state.subscribe()
    }

    /// Wait until the server is `Ready`, `Failed` or stopped, up to
    /// `timeout`; returns the state then.
    pub async fn wait_ready(&self, timeout: Duration) -> ServerState {
        let mut rx = self.subscribe();
        let _ = tokio::time::timeout(timeout, rx.wait_for(|s| *s != ServerState::Warming)).await;
        self.state()
    }

    /// The negotiated position encoding; `None` before `initialize` answers.
    pub fn encoding(&self) -> Option<PositionEncoding> {
        self.init.get().map(|i| i.encoding)
    }

    /// The workspace roots it serves, canonical.
    pub fn roots(&self) -> Vec<PathBuf> {
        lock(&self.shared.folders).clone()
    }

    /// An operation is running.
    pub fn is_busy(&self) -> bool {
        self.inflight.load(Ordering::SeqCst) > 0
    }

    /// When an operation last started or ended.
    pub fn last_used(&self) -> Instant {
        *lock(&self.last_used)
    }

    /// The server accepts more workspace folders. `None` before `initialize`
    /// answers.
    pub fn supports_folders(&self) -> Option<bool> {
        let info = self.init.get()?;
        Some(
            self.config.workspace_folders
                && (info.has("/workspace/workspaceFolders/supported")
                    || info.has("/workspace/workspaceFolders/changeNotifications")),
        )
    }

    fn err(&self, message: impl std::fmt::Display) -> CodeIntelError {
        CodeIntelError::Server {
            server: self.config.server.clone(),
            message: message.to_string(),
        }
    }

    fn touch(&self) {
        *lock(&self.last_used) = Instant::now();
    }

    fn kill(&self) {
        if let Some(tx) = lock(&self.kill).take() {
            let _ = tx.send(());
        }
    }

    /// Mark busy, wait for `initialize`, refuse a dead server.
    async fn begin(&self) -> CodeIntelResult<InFlight> {
        self.inflight.fetch_add(1, Ordering::SeqCst);
        let guard = InFlight(self.inflight.clone());
        self.touch();
        let mut rx = self.subscribe();
        let init = &self.init;
        let _ = tokio::time::timeout(
            self.options.init_timeout,
            rx.wait_for(|s| init.get().is_some() || *s != ServerState::Warming),
        )
        .await;
        match self.state() {
            ServerState::Failed => Err(self.err("server failed")),
            ServerState::NotRunning => Err(self.err("server stopped")),
            _ if self.init.get().is_none() => Err(self.err("initialize timed out")),
            _ => Ok(guard),
        }
    }

    fn info(&self) -> &InitInfo {
        self.init.get().expect("begin() waited for initialize")
    }

    async fn call<R: lsp_types::request::Request>(
        &self,
        params: R::Params,
    ) -> CodeIntelResult<R::Result> {
        let params = serde_json::to_value(params).map_err(|e| self.err(e))?;
        let raw = self
            .rpc
            .request(R::METHOD, params, self.options.request_timeout)
            .await
            .map_err(|e| self.err(e))?;
        self.touch();
        serde_json::from_value(raw).map_err(|e| self.err(RpcError::Decode(e.to_string())))
    }

    /// The canonical folder `root` names, if this session serves it.
    fn folder(&self, root: &Path) -> CodeIntelResult<PathBuf> {
        let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        lock(&self.shared.folders)
            .iter()
            .find(|f| **f == canon)
            .cloned()
            .ok_or_else(|| CodeIntelError::NotOpen(root.to_path_buf()))
    }

    /// Open `abs` on the server if it is not yet, and return its text.
    async fn open_doc(&self, abs: &Path, rel: &str) -> CodeIntelResult<String> {
        let mut docs = self.docs.lock().await;
        docs.tick += 1;
        let tick = docs.tick;
        if let Some(doc) = docs.open.get_mut(abs) {
            doc.used = tick;
            return Ok(doc.text.clone());
        }
        let language_id = self.config.language_id_for(abs).ok_or_else(|| {
            CodeIntelError::Unsupported(format!("{}: no language for {rel}", self.config.server))
        })?;
        let text = match tokio::fs::read_to_string(abs).await {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(CodeIntelError::FileNotFound(rel.to_string()));
            }
            Err(e) => return Err(CodeIntelError::Other(format!("read {rel}: {e}"))),
        };
        self.rpc.notify(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": text::file_uri(abs),
                "languageId": language_id,
                "version": 0,
                "text": text,
            }}),
        );
        docs.open.insert(
            abs.to_path_buf(),
            Doc {
                version: 0,
                text: text.clone(),
                used: tick,
            },
        );
        while docs.open.len() > self.options.max_open_docs.max(1) {
            let Some(oldest) = docs
                .open
                .iter()
                .min_by_key(|(_, d)| d.used)
                .map(|(p, _)| p.clone())
            else {
                break;
            };
            docs.open.remove(&oldest);
            self.rpc.notify(
                "textDocument/didClose",
                json!({"textDocument": {"uri": text::file_uri(&oldest)}}),
            );
        }
        Ok(text)
    }

    /// The current text of each file: the open buffer, else the disk.
    async fn texts(&self, paths: impl IntoIterator<Item = PathBuf>) -> HashMap<PathBuf, String> {
        let mut out = HashMap::new();
        for p in paths {
            if out.contains_key(&p) {
                continue;
            }
            let open = self.docs.lock().await.open.get(&p).map(|d| d.text.clone());
            let text = match open {
                Some(t) => t,
                None => tokio::fs::read_to_string(&p).await.unwrap_or_default(),
            };
            out.insert(p, text);
        }
        out
    }

    /// Convert LSP locations to contract ones, dropping any outside `root`.
    async fn locations(
        &self,
        root: &Path,
        raw: Vec<(lsp_types::Uri, lsp_types::Range)>,
    ) -> Vec<Location> {
        self.locations_keep_index(root, raw)
            .await
            .into_iter()
            .flatten()
            .collect()
    }

    fn position_params(
        &self,
        abs: &Path,
        text: &str,
        at: Position,
    ) -> lsp_types::TextDocumentPositionParams {
        lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier {
                uri: text::lsp_uri(abs),
            },
            position: text::to_lsp(text, at, self.info().encoding),
        }
    }

    /// Where the symbol at `at` is defined.
    pub async fn definition(
        &self,
        root: &Path,
        path: &str,
        at: Position,
    ) -> CodeIntelResult<Vec<Location>> {
        let _busy = self.begin().await?;
        let root = self.folder(root)?;
        let abs = text::resolve(&root, path)?;
        let text = self.open_doc(&abs, path).await?;
        let params = lsp_types::GotoDefinitionParams {
            text_document_position_params: self.position_params(&abs, &text, at),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let raw = match self
            .call::<lsp_types::request::GotoDefinition>(params)
            .await?
        {
            None => vec![],
            Some(GotoDefinitionResponse::Scalar(l)) => vec![(l.uri, l.range)],
            Some(GotoDefinitionResponse::Array(ls)) => {
                ls.into_iter().map(|l| (l.uri, l.range)).collect()
            }
            Some(GotoDefinitionResponse::Link(links)) => links
                .into_iter()
                .map(|l| (l.target_uri, l.target_selection_range))
                .collect(),
        };
        Ok(self.locations(&root, raw).await)
    }

    /// Uses of the symbol at `at`, with its declaration if `include_decl`.
    pub async fn references(
        &self,
        root: &Path,
        path: &str,
        at: Position,
        include_decl: bool,
    ) -> CodeIntelResult<Vec<Location>> {
        let _busy = self.begin().await?;
        let root = self.folder(root)?;
        let abs = text::resolve(&root, path)?;
        let text = self.open_doc(&abs, path).await?;
        let params = lsp_types::ReferenceParams {
            text_document_position: self.position_params(&abs, &text, at),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: lsp_types::ReferenceContext {
                include_declaration: include_decl,
            },
        };
        let raw = self
            .call::<lsp_types::request::References>(params)
            .await?
            .unwrap_or_default()
            .into_iter()
            .map(|l| (l.uri, l.range))
            .collect();
        Ok(self.locations(&root, raw).await)
    }

    /// The symbols of one file, nested ones flattened with their container.
    pub async fn document_symbols(
        &self,
        root: &Path,
        path: &str,
    ) -> CodeIntelResult<Vec<CodeSymbol>> {
        let _busy = self.begin().await?;
        let root = self.folder(root)?;
        let abs = text::resolve(&root, path)?;
        let text = self.open_doc(&abs, path).await?;
        let params = lsp_types::DocumentSymbolParams {
            text_document: lsp_types::TextDocumentIdentifier {
                uri: text::lsp_uri(&abs),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let enc = self.info().encoding;
        match self
            .call::<lsp_types::request::DocumentSymbolRequest>(params)
            .await?
        {
            None => Ok(vec![]),
            Some(DocumentSymbolResponse::Nested(syms)) => {
                let mut out = Vec::new();
                flatten(&text, enc, path, None, syms, &mut out);
                Ok(out)
            }
            Some(DocumentSymbolResponse::Flat(syms)) => Ok(self.symbol_infos(&root, syms).await),
        }
    }

    /// `workspace/symbol` for `query`, limited to `root`. Servers match
    /// fuzzily; exact-name filtering is the caller's.
    pub async fn workspace_symbols(
        &self,
        root: &Path,
        query: &str,
    ) -> CodeIntelResult<Vec<CodeSymbol>> {
        let _busy = self.begin().await?;
        let root = self.folder(root)?;
        let params = lsp_types::WorkspaceSymbolParams {
            query: query.to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let infos = match self
            .call::<lsp_types::request::WorkspaceSymbolRequest>(params)
            .await?
        {
            None => vec![],
            Some(WorkspaceSymbolResponse::Flat(syms)) => syms,
            // A location without a range needs `workspaceSymbol/resolve`,
            // which we don't advertise; such entries are skipped.
            #[allow(deprecated)]
            Some(WorkspaceSymbolResponse::Nested(syms)) => syms
                .into_iter()
                .filter_map(|s| match s.location {
                    OneOf::Left(location) => Some(SymbolInformation {
                        name: s.name,
                        kind: s.kind,
                        tags: s.tags,
                        deprecated: None,
                        location,
                        container_name: s.container_name,
                    }),
                    OneOf::Right(_) => None,
                })
                .collect(),
        };
        Ok(self.symbol_infos(&root, infos).await)
    }

    async fn symbol_infos(&self, root: &Path, syms: Vec<SymbolInformation>) -> Vec<CodeSymbol> {
        let raw = syms
            .iter()
            .map(|s| (s.location.uri.clone(), s.location.range))
            .collect();
        let locations = self.locations_keep_index(root, raw).await;
        syms.into_iter()
            .zip(locations)
            .filter_map(|(s, loc)| {
                let loc = loc?;
                Some(CodeSymbol {
                    name: s.name,
                    kind: convert::symbol_kind(s.kind),
                    range: loc.range,
                    name_range: loc.range,
                    path: loc.path,
                    container: s.container_name.filter(|c| !c.is_empty()),
                    source: Source::Lsp,
                })
            })
            .collect()
    }

    /// Like `locations`, one entry per input (`None` outside `root`).
    async fn locations_keep_index(
        &self,
        root: &Path,
        raw: Vec<(lsp_types::Uri, lsp_types::Range)>,
    ) -> Vec<Option<Location>> {
        let paths: Vec<Option<PathBuf>> = raw
            .iter()
            .map(|(u, _)| text::uri_path(u.as_str()))
            .collect();
        let texts = self.texts(paths.iter().flatten().cloned()).await;
        let enc = self.info().encoding;
        paths
            .into_iter()
            .zip(raw)
            .map(|(path, (_, range))| {
                let path = path?;
                let rel = text::relative(root, &path)?;
                let text = texts.get(&path).map(String::as_str).unwrap_or("");
                Some(Location {
                    path: rel,
                    range: text::range_from_lsp(text, range, enc),
                })
            })
            .collect()
    }

    /// Diagnostics of one file (pulled when the server supports it, else
    /// the last published ones), or of every file under `root` the server
    /// has reported on.
    pub async fn diagnostics(
        &self,
        root: &Path,
        path: Option<&str>,
    ) -> CodeIntelResult<Vec<Diagnostic>> {
        let _busy = self.begin().await?;
        let root = self.folder(root)?;
        let files: Vec<PathBuf> = match path {
            Some(rel) => {
                let abs = text::resolve(&root, rel)?;
                let notified = self.shared.diagnostics_changed.notified();
                let was_open = self.docs.lock().await.open.contains_key(&abs);
                self.open_doc(&abs, rel).await?;
                if self.info().has("/diagnosticProvider") {
                    self.pull_diagnostics(&abs).await?;
                } else if !was_open && !lock(&self.shared.diagnostics).contains_key(&abs) {
                    // Just opened: give the server a moment to publish.
                    let _ = tokio::time::timeout(self.options.diagnostics_wait, async {
                        tokio::pin!(notified);
                        loop {
                            notified.as_mut().await;
                            if lock(&self.shared.diagnostics).contains_key(&abs) {
                                break;
                            }
                            notified.set(self.shared.diagnostics_changed.notified());
                        }
                    })
                    .await;
                }
                vec![abs]
            }
            None => lock(&self.shared.diagnostics)
                .keys()
                .filter(|p| text::relative(&root, p).is_some())
                .cloned()
                .collect(),
        };
        let raw: Vec<(PathBuf, Vec<lsp_types::Diagnostic>)> = {
            let cache = lock(&self.shared.diagnostics);
            files
                .into_iter()
                .filter_map(|f| cache.get(&f).cloned().map(|d| (f, d)))
                .collect()
        };
        let texts = self.texts(raw.iter().map(|(p, _)| p.clone())).await;
        let enc = self.info().encoding;
        let mut out = Vec::new();
        for (file, diags) in raw {
            let Some(rel) = text::relative(&root, &file) else {
                continue;
            };
            let text = texts.get(&file).map(String::as_str).unwrap_or("");
            for d in diags {
                out.push(Diagnostic {
                    location: Location {
                        path: rel.clone(),
                        range: text::range_from_lsp(text, d.range, enc),
                    },
                    severity: convert::severity(d.severity),
                    message: d.message,
                    code: convert::code(d.code),
                    server: self.config.server.clone(),
                });
            }
        }
        out.sort_by(|a, b| a.location.cmp(&b.location));
        Ok(out)
    }

    /// `textDocument/diagnostic` into the cache; an "unchanged" report keeps
    /// the cached one.
    async fn pull_diagnostics(&self, abs: &Path) -> CodeIntelResult<()> {
        let params = lsp_types::DocumentDiagnosticParams {
            text_document: lsp_types::TextDocumentIdentifier {
                uri: text::lsp_uri(abs),
            },
            identifier: None,
            previous_result_id: None,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let report = self
            .call::<lsp_types::request::DocumentDiagnosticRequest>(params)
            .await?;
        if let lsp_types::DocumentDiagnosticReportResult::Report(
            lsp_types::DocumentDiagnosticReport::Full(full),
        ) = report
        {
            lock(&self.shared.diagnostics).insert(
                abs.to_path_buf(),
                full.full_document_diagnostic_report.items,
            );
        }
        Ok(())
    }

    /// The edits renaming the symbol at `at` to `new_name` would make,
    /// grouped by file, each file's edits last-first so they apply in
    /// order. Fails when the rename would also create, move or delete
    /// files, or touch files outside `root`.
    pub async fn rename(
        &self,
        root: &Path,
        path: &str,
        at: Position,
        new_name: &str,
    ) -> CodeIntelResult<Vec<TextEdit>> {
        let _busy = self.begin().await?;
        let root = self.folder(root)?;
        let abs = text::resolve(&root, path)?;
        let text = self.open_doc(&abs, path).await?;
        let params = lsp_types::RenameParams {
            text_document_position: self.position_params(&abs, &text, at),
            new_name: new_name.to_string(),
            work_done_progress_params: Default::default(),
        };
        let Some(edit) = self.call::<lsp_types::request::Rename>(params).await? else {
            return Ok(vec![]);
        };
        let raw = workspace_edits(edit)?;
        let paths: Vec<PathBuf> = raw
            .iter()
            .map(|(u, _)| {
                text::uri_path(u.as_str())
                    .ok_or_else(|| CodeIntelError::OutsideWorkspace(u.as_str().to_string()))
            })
            .collect::<CodeIntelResult<_>>()?;
        let texts = self.texts(paths.iter().cloned()).await;
        let enc = self.info().encoding;
        let mut out = Vec::new();
        for (file, (_, edits)) in paths.into_iter().zip(raw) {
            let rel = text::relative(&root, &file)
                .ok_or_else(|| CodeIntelError::OutsideWorkspace(file.display().to_string()))?;
            let text = texts.get(&file).map(String::as_str).unwrap_or("");
            let mut edits: Vec<TextEdit> = edits
                .into_iter()
                .map(|e| TextEdit {
                    path: rel.clone(),
                    range: text::range_from_lsp(text, e.range, enc),
                    new_text: e.new_text,
                })
                .collect();
            edits.sort_by_key(|e| std::cmp::Reverse(e.range.start));
            out.extend(edits);
        }
        Ok(out)
    }

    /// Hover text (markdown) at `at`.
    pub async fn hover(
        &self,
        root: &Path,
        path: &str,
        at: Position,
    ) -> CodeIntelResult<Option<String>> {
        let _busy = self.begin().await?;
        let root = self.folder(root)?;
        let abs = text::resolve(&root, path)?;
        let text = self.open_doc(&abs, path).await?;
        let params = lsp_types::HoverParams {
            text_document_position_params: self.position_params(&abs, &text, at),
            work_done_progress_params: Default::default(),
        };
        Ok(self
            .call::<lsp_types::request::HoverRequest>(params)
            .await?
            .and_then(|h| convert::hover_text(h.contents)))
    }

    /// `path` changed on disk: push the new text if it is open (close it if
    /// it is gone), and tell a server that watches files through us.
    pub async fn file_changed(&self, root: &Path, path: &str) -> CodeIntelResult<()> {
        let _busy = self.begin().await?;
        let root = self.folder(root)?;
        let abs = text::resolve(&root, path)?;
        let now = tokio::fs::read_to_string(&abs).await.ok();
        let syncs = !matches!(
            self.info().caps.pointer("/textDocumentSync"),
            Some(Value::Number(n)) if n.as_u64() == Some(0)
        ) && !matches!(
            self.info().caps.pointer("/textDocumentSync/change"),
            Some(Value::Number(n)) if n.as_u64() == Some(0)
        );
        {
            let mut docs = self.docs.lock().await;
            match (docs.open.get_mut(&abs), &now) {
                (Some(doc), Some(text)) if doc.text != *text => {
                    doc.version += 1;
                    doc.text = text.clone();
                    if syncs {
                        self.rpc.notify(
                            "textDocument/didChange",
                            json!({
                                "textDocument": {"uri": text::file_uri(&abs), "version": doc.version},
                                "contentChanges": [{"text": text}],
                            }),
                        );
                    }
                }
                (Some(_), None) => {
                    docs.open.remove(&abs);
                    self.rpc.notify(
                        "textDocument/didClose",
                        json!({"textDocument": {"uri": text::file_uri(&abs)}}),
                    );
                }
                _ => {}
            }
        }
        if self.shared.watches_files.load(Ordering::SeqCst) {
            // 2 = changed, 3 = deleted.
            let kind = if now.is_some() { 2 } else { 3 };
            self.rpc.notify(
                "workspace/didChangeWatchedFiles",
                json!({"changes": [{"uri": text::file_uri(&abs), "type": kind}]}),
            );
        }
        Ok(())
    }

    /// Serve one more worktree (`workspace/didChangeWorkspaceFolders`).
    /// `Unsupported` when the server takes a single root.
    pub async fn add_folder(&self, root: &Path) -> CodeIntelResult<()> {
        let _busy = self.begin().await?;
        if self.supports_folders() != Some(true) {
            return Err(CodeIntelError::Unsupported(format!(
                "{} takes one workspace folder",
                self.config.server
            )));
        }
        let root = std::fs::canonicalize(root)
            .map_err(|e| CodeIntelError::Other(format!("{}: {e}", root.display())))?;
        {
            let mut folders = lock(&self.shared.folders);
            if folders.contains(&root) {
                return Ok(());
            }
            folders.push(root.clone());
        }
        self.rpc.notify(
            "workspace/didChangeWorkspaceFolders",
            json!({"event": {"added": [folder_json(&root)], "removed": []}}),
        );
        Ok(())
    }

    /// Stop serving a worktree; its open documents are closed.
    pub async fn remove_folder(&self, root: &Path) {
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        {
            let mut folders = lock(&self.shared.folders);
            let before = folders.len();
            folders.retain(|f| *f != root);
            if folders.len() == before {
                return;
            }
        }
        let mut docs = self.docs.lock().await;
        let gone: Vec<PathBuf> = docs
            .open
            .keys()
            .filter(|p| p.starts_with(&root))
            .cloned()
            .collect();
        for p in gone {
            docs.open.remove(&p);
            self.rpc.notify(
                "textDocument/didClose",
                json!({"textDocument": {"uri": text::file_uri(&p)}}),
            );
        }
        drop(docs);
        lock(&self.shared.diagnostics).retain(|p, _| !p.starts_with(&root));
        self.rpc.notify(
            "workspace/didChangeWorkspaceFolders",
            json!({"event": {"added": [], "removed": [folder_json(&root)]}}),
        );
    }

    /// `shutdown`, `exit`, and a kill if the process is still there after
    /// `shutdown_grace`. Idempotent. The state ends `NotRunning`.
    pub async fn shutdown(&self) {
        if self.shared.stopping.swap(true, Ordering::SeqCst) {
            return self.wait_exit(self.options.shutdown_grace).await;
        }
        let grace = self.options.shutdown_grace;
        let deadline = Instant::now() + grace;
        if !*self.exited.borrow() && !self.rpc.is_closed() {
            let _ = self.rpc.request("shutdown", Value::Null, grace).await;
            self.rpc.notify("exit", Value::Null);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        self.wait_exit(left).await;
        self.kill();
        self.wait_exit(Duration::from_secs(1)).await;
        self.shared.state.send_replace(ServerState::NotRunning);
    }

    async fn wait_exit(&self, timeout: Duration) {
        let mut rx = self.exited.clone();
        let _ = tokio::time::timeout(timeout, rx.wait_for(|e| *e)).await;
    }

    /// The child process has exited.
    pub fn has_exited(&self) -> bool {
        *self.exited.borrow()
    }
}

/// Ready once `initialize` is done and indexing has gone quiet.
async fn readiness(shared: Arc<Shared>, options: SessionOptions) {
    let deadline = Instant::now() + options.ready_max_wait;
    loop {
        if *shared.state.borrow() != ServerState::Warming {
            return;
        }
        let now = Instant::now();
        let (idle, quiet_at, has_status, quiescent) = {
            let p = lock(&shared.progress);
            (
                p.active.is_empty(),
                p.last_change + options.ready_settle,
                p.has_status,
                p.quiescent,
            )
        };
        let ready = now >= deadline
            || (has_status && quiescent && idle)
            || (!has_status && idle && now >= quiet_at);
        if ready {
            shared.state.send_if_modified(|s| {
                let warming = *s == ServerState::Warming;
                if warming {
                    *s = ServerState::Ready;
                }
                warming
            });
            return;
        }
        let wake = if idle && !has_status {
            quiet_at.min(deadline)
        } else {
            deadline
        };
        tokio::select! {
            _ = shared.progress_changed.notified() => {}
            _ = tokio::time::sleep_until(wake) => {}
        }
    }
}

fn initialize_params(root: &Path, config: &LaunchConfig) -> Value {
    let mut params = json!({
        "processId": std::process::id(),
        "clientInfo": {"name": "theodore", "version": env!("CARGO_PKG_VERSION")},
        "rootUri": text::file_uri(root),
        "rootPath": root.display().to_string(),
        "workspaceFolders": [folder_json(root)],
        "capabilities": {
            "general": {"positionEncodings": ["utf-8", "utf-16"]},
            "workspace": {
                "configuration": true,
                "workspaceFolders": true,
                "didChangeWatchedFiles": {"dynamicRegistration": true},
                "symbol": {"dynamicRegistration": false},
                "workspaceEdit": {"documentChanges": true, "resourceOperations": []},
            },
            "textDocument": {
                "synchronization": {"dynamicRegistration": false, "didSave": false},
                "definition": {"linkSupport": true},
                "references": {},
                "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                "publishDiagnostics": {"relatedInformation": false, "versionSupport": false},
                "diagnostic": {"dynamicRegistration": false, "relatedDocumentSupport": false},
                "rename": {"prepareSupport": false},
                "hover": {"contentFormat": ["markdown", "plaintext"]},
            },
            "window": {"workDoneProgress": true},
            "experimental": {"serverStatusNotification": true},
        },
    });
    if !config.init_options.is_null() {
        params["initializationOptions"] = config.init_options.clone();
    }
    params
}

fn folder_json(root: &Path) -> Value {
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    json!({"uri": text::file_uri(root), "name": name})
}

/// Depth-first, parents before children.
fn flatten(
    text: &str,
    enc: PositionEncoding,
    path: &str,
    container: Option<&str>,
    syms: Vec<DocumentSymbol>,
    out: &mut Vec<CodeSymbol>,
) {
    for s in syms {
        out.push(CodeSymbol {
            name: s.name.clone(),
            kind: convert::symbol_kind(s.kind),
            range: text::range_from_lsp(text, s.range, enc),
            name_range: text::range_from_lsp(text, s.selection_range, enc),
            path: path.to_string(),
            container: container.map(str::to_string),
            source: Source::Lsp,
        });
        if let Some(children) = s.children {
            flatten(text, enc, path, Some(&s.name), children, out);
        }
    }
}

/// A `WorkspaceEdit` as text edits per file, in the server's file order.
fn workspace_edits(
    edit: WorkspaceEdit,
) -> CodeIntelResult<Vec<(lsp_types::Uri, Vec<lsp_types::TextEdit>)>> {
    let plain = |e: OneOf<lsp_types::TextEdit, lsp_types::AnnotatedTextEdit>| match e {
        OneOf::Left(e) => e,
        OneOf::Right(a) => a.text_edit,
    };
    if let Some(changes) = edit.document_changes {
        let docs = match changes {
            DocumentChanges::Edits(docs) => docs,
            DocumentChanges::Operations(ops) => ops
                .into_iter()
                .map(|op| match op {
                    DocumentChangeOperation::Edit(e) => Ok(e),
                    DocumentChangeOperation::Op(op) => Err(CodeIntelError::Unsupported(format!(
                        "rename also changes files: {op:?}"
                    ))),
                })
                .collect::<CodeIntelResult<_>>()?,
        };
        let mut out: Vec<(lsp_types::Uri, Vec<lsp_types::TextEdit>)> = Vec::new();
        for doc in docs {
            let edits = doc.edits.into_iter().map(plain);
            match out.iter_mut().find(|(u, _)| *u == doc.text_document.uri) {
                Some((_, v)) => v.extend(edits),
                None => out.push((doc.text_document.uri, edits.collect())),
            }
        }
        return Ok(out);
    }
    let mut out: Vec<_> = edit.changes.unwrap_or_default().into_iter().collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> Arc<Shared> {
        let (state, _) = watch::channel(ServerState::Warming);
        Arc::new(Shared {
            server: "s".into(),
            init_options: Value::Null,
            folders: Mutex::default(),
            progress: Mutex::new(Progress {
                active: HashSet::new(),
                last_change: Instant::now(),
                has_status: false,
                quiescent: false,
            }),
            progress_changed: Notify::new(),
            diagnostics: Mutex::default(),
            diagnostics_changed: Notify::new(),
            watches_files: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            state,
        })
    }

    fn options() -> SessionOptions {
        SessionOptions {
            ready_settle: Duration::from_secs(1),
            ready_max_wait: Duration::from_secs(60),
            ..SessionOptions::default()
        }
    }

    fn progress(kind: &str, token: &str) -> Value {
        json!({"token": token, "value": {"kind": kind}})
    }

    #[tokio::test(start_paused = true)]
    async fn ready_after_every_token_ends_and_the_settle_passes() {
        let s = shared();
        s.on_progress(&progress("begin", "Roots Scanned"));
        let task = tokio::spawn(readiness(s.clone(), options()));
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(*s.state.borrow(), ServerState::Warming);
        // A gap between phases shorter than the settle keeps it warming.
        s.on_progress(&progress("end", "Roots Scanned"));
        tokio::time::sleep(Duration::from_millis(500)).await;
        s.on_progress(&progress("begin", "Indexing"));
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(*s.state.borrow(), ServerState::Warming);
        s.on_progress(&progress("end", "Indexing"));
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(*s.state.borrow(), ServerState::Ready);
        task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn server_status_overrides_the_settle_and_max_wait_caps_it() {
        let s = shared();
        let task = tokio::spawn(readiness(s.clone(), options()));
        s.notification("experimental/serverStatus", json!({"quiescent": false}));
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(*s.state.borrow(), ServerState::Warming);
        s.notification("experimental/serverStatus", json!({"quiescent": true}));
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(*s.state.borrow(), ServerState::Ready);
        task.await.unwrap();

        let s = shared();
        s.on_progress(&progress("begin", "never"));
        let task = tokio::spawn(readiness(s.clone(), options()));
        tokio::time::sleep(Duration::from_secs(59)).await;
        assert_eq!(*s.state.borrow(), ServerState::Warming);
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(*s.state.borrow(), ServerState::Ready);
        task.await.unwrap();
    }

    #[test]
    fn an_exit_after_shutdown_is_not_a_crash() {
        let s = shared();
        s.stopping.store(true, Ordering::SeqCst);
        s.fail("exited");
        assert_eq!(*s.state.borrow(), ServerState::Warming);
    }

    #[test]
    fn rename_edits_reject_file_operations() {
        let edit: WorkspaceEdit = serde_json::from_value(json!({"documentChanges": [
            {"kind": "rename", "oldUri": "file:///a.rs", "newUri": "file:///b.rs"}
        ]}))
        .unwrap();
        assert!(matches!(
            workspace_edits(edit),
            Err(CodeIntelError::Unsupported(_))
        ));
    }
}
