//! The `review-tools` / `space-tools` MCP server: a Streamable HTTP endpoint
//! on `127.0.0.1:<random>`, one bearer token per registered run (plan §8.3,
//! §9). The token decides the tool set (`RunContext::allows`), code tools
//! included (docs/code-intel.md §7).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rmcp::ErrorData;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::tool::{Extension, ToolRouter};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{RoleServer, ServerHandler, tool, tool_handler, tool_router};
use theodore_ports::McpEndpoint;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::backend::{
    BackendError, CODE_EDIT_TOOL_NAMES, CodeTools, CodeToolsBackend, NoCodeTools, NoSpaceTools,
    ReviewToolsBackend, RunContext, SpaceToolError, SpaceToolsBackend, ToolSet,
};
use crate::tools::{
    AddFindingParams, AddFindingResult, BaseFileResult, CodeAnswerResult, CodeEditResult,
    CodeStatusResult, DiagnosticItem, DiagnosticsParams, FindSymbolParams, GetBaseFileParams,
    GetGroupParams, GetSymbolParams, GroupResult, HitItem, InsertAfterSymbolParams, PositionParams,
    PrContextResult, ProposePatchParams, ProposePatchResult, ReadLogsParams, RenameSymbolParams,
    ReplaceSymbolBodyParams, SymbolBodyResult, SymbolItem,
};

/// Live run registrations: bearer token → run context. Shared between the auth
/// middleware (which resolves the token) and nothing else — tools only ever see
/// the resolved [`RunContext`], never the token map.
#[derive(Default)]
struct RunRegistry {
    runs: RwLock<HashMap<String, RunContext>>,
}

impl RunRegistry {
    fn resolve(&self, token: &str) -> Option<RunContext> {
        self.runs
            .read()
            .expect("run registry lock poisoned")
            .get(token)
            .cloned()
    }

    fn insert(&self, token: String, ctx: RunContext) {
        self.runs
            .write()
            .expect("run registry lock poisoned")
            .insert(token, ctx);
    }

    fn remove(&self, token: &str) {
        self.runs
            .write()
            .expect("run registry lock poisoned")
            .remove(token);
    }
}

/// Rejects requests without a valid `Authorization: Bearer <token>`; resolves
/// the token to its [`RunContext`] and attaches it to the request's HTTP
/// extensions, where tool handlers read it back via `Extension<http::request::Parts>`.
async fn auth_layer(
    axum::extract::State(registry): axum::extract::State<Arc<RunRegistry>>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let token = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let Some(token) = token else {
        return (StatusCode::UNAUTHORIZED, "missing bearer token").into_response();
    };
    let Some(ctx) = registry.resolve(token) else {
        return (StatusCode::UNAUTHORIZED, "unknown token").into_response();
    };
    req.extensions_mut().insert(ctx);
    next.run(req).await
}

fn run_context(parts: &http::request::Parts) -> Result<RunContext, ErrorData> {
    parts
        .extensions
        .get::<RunContext>()
        .cloned()
        .ok_or_else(|| ErrorData::internal_error("missing run context", None))
}

fn backend_error_result(err: BackendError) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(err.to_string())])
}

fn code_result<T: serde::Serialize>(
    result: Result<T, BackendError>,
) -> Result<CallToolResult, ErrorData> {
    match result {
        Ok(v) => Ok(CallToolResult::success(vec![ContentBlock::json(v)?])),
        Err(err) => Ok(backend_error_result(err)),
    }
}

pub(crate) fn space_result<T: serde::Serialize>(
    result: Result<T, SpaceToolError>,
) -> Result<CallToolResult, ErrorData> {
    match result {
        Ok(v) => Ok(CallToolResult::success(vec![ContentBlock::json(v)?])),
        Err(err) => Ok(CallToolResult::error(vec![ContentBlock::text(
            err.to_string(),
        )])),
    }
}

/// The space a run was registered for; the agent never names one.
pub(crate) fn run_space(parts: &http::request::Parts) -> Result<(RunContext, String), ErrorData> {
    let run = run_context(parts)?;
    let space = run
        .space
        .clone()
        .ok_or_else(|| ErrorData::invalid_params("this run has no space", None))?;
    Ok((run, space))
}

/// The run behind an MCP request, read back from the HTTP parts rmcp
/// attaches to every request context.
fn request_run(context: &RequestContext<RoleServer>) -> Option<RunContext> {
    context
        .extensions
        .get::<http::request::Parts>()?
        .extensions
        .get::<RunContext>()
        .cloned()
}

/// One server for both tool sets: the run's token decides which tools it
/// lists and may call ([`RunContext::allows`]).
#[derive(Clone)]
pub(crate) struct ReviewToolsServer {
    backend: Arc<dyn ReviewToolsBackend>,
    pub(crate) spaces: Arc<dyn SpaceToolsBackend>,
    code: Arc<dyn CodeToolsBackend>,
    tool_router: ToolRouter<Self>,
}

#[tool_router(router = review_router)]
impl ReviewToolsServer {
    fn new(
        backend: Arc<dyn ReviewToolsBackend>,
        spaces: Arc<dyn SpaceToolsBackend>,
        code: Arc<dyn CodeToolsBackend>,
    ) -> Self {
        Self {
            backend,
            spaces,
            code,
            tool_router: Self::review_router()
                + Self::space_router()
                + Self::space_agent_router()
                + Self::code_router(),
        }
    }

    #[tool(
        description = "Title, body, commits, files, and the current grouping of the PR under review."
    )]
    async fn get_pr_context(
        &self,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let run = run_context(&parts)?;
        match self.backend.get_pr_context(&run).await {
            Ok(ctx) => {
                let result = PrContextResult::from(&ctx);
                Ok(CallToolResult::success(vec![ContentBlock::json(result)?]))
            }
            Err(err) => Ok(backend_error_result(err)),
        }
    }

    #[tool(
        description = "A group from the current grouping, with its hunks rendered as unified diff text."
    )]
    async fn get_group(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(params): Parameters<GetGroupParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let run = run_context(&parts)?;
        match self.backend.get_group(&run, &params.id).await {
            Ok(group) => {
                let result = GroupResult::from(&group);
                Ok(CallToolResult::success(vec![ContentBlock::json(result)?]))
            }
            Err(err) => Ok(backend_error_result(err)),
        }
    }

    #[tool(description = "File contents at the base revision (the worktree only has head).")]
    async fn get_base_file(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(params): Parameters<GetBaseFileParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let run = run_context(&parts)?;
        match self.backend.get_base_file(&run, &params.path).await {
            Ok(content) => {
                let result = BaseFileResult {
                    path: params.path,
                    exists: content.is_some(),
                    content: content.unwrap_or_default(),
                };
                Ok(CallToolResult::success(vec![ContentBlock::json(result)?]))
            }
            Err(err) => Ok(backend_error_result(err)),
        }
    }

    #[tool(
        description = "Report a review finding: file, line, severity, title, body. Returns a finding id."
    )]
    async fn add_finding(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(params): Parameters<AddFindingParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let run = run_context(&parts)?;
        match self.backend.add_finding(&run, params.into()).await {
            Ok(id) => {
                let result = AddFindingResult {
                    finding_id: id.to_string(),
                };
                Ok(CallToolResult::success(vec![ContentBlock::json(result)?]))
            }
            Err(err) => Ok(backend_error_result(err)),
        }
    }

    #[tool(description = "Attach a proposed fix to an existing finding.")]
    async fn propose_patch(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(params): Parameters<ProposePatchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let run = run_context(&parts)?;
        match self
            .backend
            .propose_patch(&run, params.into_new_patch())
            .await
        {
            Ok(()) => {
                let result = ProposePatchResult { ok: true };
                Ok(CallToolResult::success(vec![ContentBlock::json(result)?]))
            }
            Err(err) => Ok(backend_error_result(err)),
        }
    }
}

#[tool_router(router = space_router)]
impl ReviewToolsServer {
    #[tool(
        description = "Run the project's configured check (`spaces.check`) in this space now. Returns passed, the exit code and the last lines of output. Use it after making changes."
    )]
    async fn run_check(
        &self,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let (run, space) = run_space(&parts)?;
        space_result(self.spaces.run_check(&space, &run.run_id).await)
    }

    #[tool(
        description = "Start the space's app (`spaces.run`) unless it is already running. Returns whether it runs, its ports and http://localhost URLs."
    )]
    async fn start_app(
        &self,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let (_, space) = run_space(&parts)?;
        space_result(self.spaces.start_app(&space).await)
    }

    #[tool(description = "Stop the space's app (`spaces.run`). Returns its status.")]
    async fn stop_app(
        &self,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let (_, space) = run_space(&parts)?;
        space_result(self.spaces.stop_app(&space).await)
    }

    #[tool(
        description = "Whether the space's app (`spaces.run`) is running, its ports and http://localhost URLs, and how its last run ended."
    )]
    async fn app_status(
        &self,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let (_, space) = run_space(&parts)?;
        space_result(self.spaces.app_status(&space).await)
    }

    #[tool(
        description = "The last lines of output of one of the space's commands: setup, run (the app), check or archive."
    )]
    async fn read_logs(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(params): Parameters<ReadLogsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (_, space) = run_space(&parts)?;
        let tail = params.bounded_tail();
        space_result(
            self.spaces
                .read_logs(&space, params.kind.into(), tail)
                .await,
        )
    }

    #[tool(
        description = "The space: name, branch, base, worktree path, ports and URLs, environment variable names (not values), and which commands are configured and allowed."
    )]
    async fn space_info(
        &self,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let (_, space) = run_space(&parts)?;
        space_result(self.spaces.space_info(&space).await)
    }
}

/// Code intelligence (docs/code-intel.md §7): listed only for runs whose
/// project has it on ([`RunContext::code`]); the edits only on a space
/// turn that may write.
#[tool_router(router = code_router)]
impl ReviewToolsServer {
    #[tool(
        description = "Definitions named `name` (exact, case-sensitive), optionally of one kind. Returns file:line with the symbol's kind and container, and `source` (lsp: exact, syntax: by name, text: a search). Prefer this to grepping for code."
    )]
    async fn find_symbol(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<FindSymbolParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let run = run_context(&parts)?;
        let answer = self
            .code
            .find_symbol(&run, &p.name, p.kind.map(Into::into))
            .await;
        code_result(answer.map(|a| CodeAnswerResult::from_answer(a, SymbolItem::from)))
    }

    #[tool(
        description = "The source of one symbol (signature and body) in a file, instead of reading the whole file."
    )]
    async fn get_symbol(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<GetSymbolParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let run = run_context(&parts)?;
        let found = self.code.get_symbol(&run, &p.path, &p.name).await;
        code_result(found.map(|(s, body)| SymbolBodyResult::new(s, &body)))
    }

    #[tool(
        description = "Uses of the symbol at path:line:col (1-based; col in characters): call sites with a line of context, same file first."
    )]
    async fn references(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<PositionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let run = run_context(&parts)?;
        let answer = self.code.references(&run, &p.path, p.at()).await;
        code_result(answer.map(|a| CodeAnswerResult::from_answer(a, HitItem::from)))
    }

    #[tool(
        description = "Where the symbol at path:line:col (1-based; col in characters) is defined."
    )]
    async fn definition(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<PositionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let run = run_context(&parts)?;
        let answer = self.code.definition(&run, &p.path, p.at()).await;
        code_result(answer.map(|a| CodeAnswerResult::from_answer(a, HitItem::from)))
    }

    #[tool(
        description = "Errors and warnings from the language server: of one file, or of every file it reported on. Empty with source syntax when no server runs."
    )]
    async fn diagnostics(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<DiagnosticsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let run = run_context(&parts)?;
        let answer = self.code.diagnostics(&run, p.path.as_deref()).await;
        code_result(answer.map(|a| CodeAnswerResult::from_answer(a, DiagnosticItem::from)))
    }

    #[tool(
        description = "Code intelligence per language: grammar, language server (installed, warming, ready) and how much of the worktree is indexed."
    )]
    async fn code_status(
        &self,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let run = run_context(&parts)?;
        let status = self.code.code_status(&run).await;
        code_result(status.map(|langs| CodeStatusResult {
            languages: langs.into_iter().map(Into::into).collect(),
        }))
    }

    #[tool(
        description = "Replace a symbol's whole definition (signature and body) in a file with `body`, without matching old text by hand. Returns the files written."
    )]
    async fn replace_symbol_body(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<ReplaceSymbolBodyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let run = run_context(&parts)?;
        let done = self
            .code
            .replace_symbol_body(&run, &p.path, &p.name, &p.body)
            .await;
        code_result(done.map(CodeEditResult::from))
    }

    #[tool(
        description = "Insert `text` on the line after a symbol's definition (a new function after another, a new method in an impl). Returns the files written."
    )]
    async fn insert_after_symbol(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<InsertAfterSymbolParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let run = run_context(&parts)?;
        let done = self
            .code
            .insert_after_symbol(&run, &p.path, &p.name, &p.text)
            .await;
        code_result(done.map(CodeEditResult::from))
    }

    #[tool(
        description = "Rename the symbol at path:line:col everywhere it is used, through the language server. Returns the files written."
    )]
    async fn rename_symbol(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<RenameSymbolParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let run = run_context(&parts)?;
        let at = theodore_ports::Position {
            line: p.line,
            col: p.col,
        };
        let done = self
            .code
            .rename_symbol(&run, &p.path, at, &p.new_name)
            .await;
        code_result(done.map(CodeEditResult::from))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ReviewToolsServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("theodore-review-tools", "0.1.0"))
    }

    /// Only the tools the run's token allows.
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let run = request_run(&context)
            .ok_or_else(|| ErrorData::internal_error("missing run context", None))?;
        let tools = self
            .tool_router
            .list_all()
            .into_iter()
            .filter(|t| run.allows(&t.name))
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    /// Refuses a tool outside the run's set as if it didn't exist.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let run = request_run(&context)
            .ok_or_else(|| ErrorData::internal_error("missing run context", None))?;
        if !run.allows(&request.name) {
            // A space turn that may not write hears why, instead of a
            // tool that doesn't exist.
            if run.tools == ToolSet::Space
                && run.code == CodeTools::Read
                && CODE_EDIT_TOOL_NAMES.contains(&request.name.as_ref())
            {
                return Ok(CallToolResponse::Complete(CallToolResult::error(vec![
                    ContentBlock::text(
                        "code edits are off: this turn is in Ask mode. Ask the user to switch \
                         the space to Edit",
                    ),
                ])));
            }
            return Err(ErrorData::invalid_params("tool not found", None));
        }
        let tcc = ToolCallContext::new(self, request, context);
        self.tool_router.call(tcc).await
    }
}

/// A registered agent run: its MCP endpoint (URL + bearer token).
pub type RegisteredEndpoint = McpEndpoint;

/// A running `review-tools` MCP server. Dropping this does not stop the
/// server; call [`McpHandle::shutdown`] for a graceful stop.
pub struct McpHandle {
    addr: SocketAddr,
    registry: Arc<RunRegistry>,
    shutdown: CancellationToken,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl McpHandle {
    /// Local address the server is listening on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Registers a new agent run and returns the endpoint it should be given
    /// (URL + bearer token). Call [`Self::unregister_run`] when the run ends.
    pub fn register_run(&self, ctx: RunContext) -> McpEndpoint {
        let token = Uuid::new_v4().to_string();
        let name = ctx.server_name().to_string();
        self.registry.insert(token.clone(), ctx);
        McpEndpoint {
            url: format!("http://{}/mcp", self.addr),
            token,
            name,
        }
    }

    /// Revokes a token; further requests carrying it get 401.
    pub fn unregister_run(&self, token: &str) {
        self.registry.remove(token);
    }

    /// Stops accepting requests and waits for the server task to exit.
    pub async fn shutdown(mut self) {
        self.shutdown.cancel();
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
    }
}

/// Serves the `review-tools` / `space-tools` MCP server over Streamable
/// HTTP on `127.0.0.1:<random port>`. Each agent run gets its own token via
/// [`McpHandle::register_run`]; requests must carry `Authorization: Bearer <token>`.
pub struct McpServer;

impl McpServer {
    /// Without spaces: the space tools fail ([`NoSpaceTools`]).
    pub async fn start(backend: Arc<dyn ReviewToolsBackend>) -> std::io::Result<McpHandle> {
        Self::start_with_spaces(backend, Arc::new(NoSpaceTools)).await
    }

    /// Without code intelligence: no run lists the code tools.
    pub async fn start_with_spaces(
        backend: Arc<dyn ReviewToolsBackend>,
        spaces: Arc<dyn SpaceToolsBackend>,
    ) -> std::io::Result<McpHandle> {
        Self::start_with_code(backend, spaces, Arc::new(NoCodeTools)).await
    }

    pub async fn start_with_code(
        backend: Arc<dyn ReviewToolsBackend>,
        spaces: Arc<dyn SpaceToolsBackend>,
        code: Arc<dyn CodeToolsBackend>,
    ) -> std::io::Result<McpHandle> {
        let registry = Arc::new(RunRegistry::default());
        let shutdown = CancellationToken::new();

        let http_config = StreamableHttpServerConfig::default()
            .with_cancellation_token(shutdown.clone())
            .with_sse_keep_alive(None);
        let session_manager = Arc::new(LocalSessionManager::default());
        let service: StreamableHttpService<ReviewToolsServer, LocalSessionManager> =
            StreamableHttpService::new(
                {
                    let backend = backend.clone();
                    move || {
                        Ok(ReviewToolsServer::new(
                            backend.clone(),
                            spaces.clone(),
                            code.clone(),
                        ))
                    }
                },
                session_manager,
                http_config,
            );

        let app = Router::new()
            .nest_service("/mcp", service)
            .layer(middleware::from_fn_with_state(registry.clone(), auth_layer));

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        let shutdown_for_task = shutdown.clone();
        let join = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move { shutdown_for_task.cancelled().await })
                .await;
        });

        Ok(McpHandle {
            addr,
            registry,
            shutdown,
            join: Some(join),
        })
    }
}
