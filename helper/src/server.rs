use std::{net::SocketAddr, sync::Arc};

use rmcp::{
    ErrorData as McpError, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, ListToolsResult, PaginatedRequestParams,
        ServerCapabilities, ServerConfig,
    },
    service::{RequestContext, RoleServer},
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{
    backend::{self, Backend},
    config::{HelperConfig, HelperError},
};

/// Serves Agents and passes their tool calls to the person's browser.
#[derive(Clone)]
struct Helper {
    backend: Arc<Backend>,
}

impl ServerHandler for Helper {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.backend
            .list_tools(request)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        self.backend
            .call_tool_once(request)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))
    }
}

/// A running Helper. Drop or call [`RunningHelper::stop`] to shut it down.
pub struct RunningHelper {
    /// Where Agents connect, for example `http://127.0.0.1:47600/mcp`.
    pub mcp_url: String,
    pub addr: SocketAddr,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl RunningHelper {
    pub async fn stop(self) {
        self.cancel.cancel();
        let _ = self.task.await;
    }
}

/// Attach to the browser and start serving Agents.
pub async fn start(config: HelperConfig) -> Result<RunningHelper, HelperError> {
    config.validate()?;
    let backend = Arc::new(backend::connect(&config.devtools, &config.browser_url).await?);

    let cancel = CancellationToken::new();
    let helper = Helper { backend };
    let service: StreamableHttpService<Helper, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(helper.clone()),
        Default::default(),
        StreamableHttpServerConfig::default().with_cancellation_token(cancel.child_token()),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    let addr = listener.local_addr()?;
    let task = tokio::spawn({
        let cancel = cancel.clone();
        async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move { cancel.cancelled_owned().await })
                .await;
        }
    });
    Ok(RunningHelper {
        mcp_url: format!("http://{addr}/mcp"),
        addr,
        cancel,
        task,
    })
}
