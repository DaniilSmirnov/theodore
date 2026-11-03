//! The code tools alone (`code-tools`) for one fixed run, over any MCP
//! transport: stdio for `theodore code-tools-mcp`. The same tool
//! definitions, descriptions, result shapes and caps as the app's servers
//! (`server`), because it is that server: the run the HTTP server reads
//! from a bearer token is given here once, up front.
//!
//! ```text
//! client ─▶ tools/list · tools/call ─▶ + run (ToolSet::Code) ─▶ ReviewToolsServer
//!                                                              └─ RunContext::allows: code tools only
//! ```

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};

use crate::backend::{
    CodeTools, CodeToolsBackend, NoReviewTools, NoSpaceTools, RunContext, ToolSet,
};
use crate::server::ReviewToolsServer;

#[derive(Debug, thiserror::Error)]
#[error("serving code-tools: {0}")]
pub struct ServeError(String);

/// A `code-tools` run on `agent`: read-only, or with the edits.
pub fn code_run(agent: theodore_domain::AgentId, edits: bool) -> RunContext {
    RunContext {
        run_id: theodore_domain::RunId::new("code-tools"),
        pr: theodore_domain::PrId::new("code-tools"),
        agent,
        space: None,
        tools: ToolSet::Code,
        code: if edits {
            CodeTools::Edit
        } else {
            CodeTools::Read
        },
    }
}

/// Serves `code` as `run` over `transport` until it closes.
pub async fn serve_code_tools<T, E, A>(
    code: Arc<dyn CodeToolsBackend>,
    run: RunContext,
    transport: T,
) -> Result<(), ServeError>
where
    T: rmcp::transport::IntoTransport<RoleServer, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let handler = OneRun {
        inner: ReviewToolsServer::new(Arc::new(NoReviewTools), Arc::new(NoSpaceTools), code),
        run,
    };
    let running = handler
        .serve(transport)
        .await
        .map_err(|e| ServeError(e.to_string()))?;
    running
        .waiting()
        .await
        .map_err(|e| ServeError(e.to_string()))?;
    Ok(())
}

/// The app's server, every request carrying the one run.
#[derive(Clone)]
struct OneRun {
    inner: ReviewToolsServer,
    run: RunContext,
}

impl OneRun {
    /// Where the HTTP server's auth layer puts the run: the request's
    /// `http::request::Parts`, which the tools read it from.
    fn with_run(&self, mut context: RequestContext<RoleServer>) -> RequestContext<RoleServer> {
        let (mut parts, ()) = http::Request::new(()).into_parts();
        parts.extensions.insert(self.run.clone());
        context.extensions.insert(parts);
        context
    }
}

impl ServerHandler for OneRun {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("theodore-code-tools", "0.1.0"))
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        self.inner.list_tools(request, self.with_run(context)).await
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.inner.call_tool(request, self.with_run(context)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{
        CODE_EDIT_TOOL_NAMES, CODE_TOOL_NAMES, REVIEW_TOOL_NAMES, SPACE_TOOL_NAMES,
    };

    #[test]
    fn a_code_run_sees_the_code_tools_only() {
        let read = code_run(theodore_domain::AgentId::Claude, false);
        let edit = code_run(theodore_domain::AgentId::Claude, true);
        assert_eq!(read.server_name(), theodore_ports::CODE_TOOLS);
        for t in CODE_TOOL_NAMES {
            assert!(read.allows(t) && edit.allows(t), "{t}");
        }
        for t in CODE_EDIT_TOOL_NAMES {
            assert!(!read.allows(t) && edit.allows(t), "{t}");
        }
        for t in REVIEW_TOOL_NAMES.iter().chain(SPACE_TOOL_NAMES) {
            assert!(!edit.allows(t), "{t}");
        }
        assert!(!edit.allows(theodore_ports::APPROVE_TOOL));
        assert!(edit.code_edits() && !read.code_edits());
    }
}
