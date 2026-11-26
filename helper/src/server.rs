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
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

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

/// A running Helper. Dropping it, or calling [`RunningHelper::stop`], shuts
/// it down and stops Chrome DevTools MCP.
pub struct RunningHelper {
    /// Where Agents connect, for example `http://127.0.0.1:47600/mcp`.
    pub mcp_url: String,
    pub addr: SocketAddr,
    cancel: CancellationToken,
    task: JoinHandle<Result<(), HelperError>>,
    _stop_on_drop: DropGuard,
}

impl RunningHelper {
    /// Shut the Helper down and wait for it to finish.
    pub async fn stop(self) {
        self.cancel.cancel();
        let _ = self.task.await;
    }

    /// Wait until the Helper stops on its own, for example because Chrome
    /// DevTools MCP exited.
    pub async fn stopped(&mut self) -> Result<(), HelperError> {
        match (&mut self.task).await {
            Ok(result) => result,
            Err(e) => Err(HelperError::Devtools(e.to_string())),
        }
    }
}

/// Attach to the browser and start serving Agents.
pub async fn start(config: HelperConfig) -> Result<RunningHelper, HelperError> {
    config.validate()?;
    // Take the port first, so a busy port fails before anything starts.
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    let addr = listener.local_addr()?;
    let backend = Arc::new(backend::connect(&config.devtools, &config.browser_url).await?);

    let cancel = CancellationToken::new();
    let helper = Helper {
        backend: backend.clone(),
    };
    let service: StreamableHttpService<Helper, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(helper.clone()),
        Default::default(),
        StreamableHttpServerConfig::default()
            // Host is checked against loopback by default; also refuse any
            // request a web page sends, since Agents don't send an Origin.
            .enforce_origin_validation()
            .with_cancellation_token(cancel.child_token()),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    let task = tokio::spawn({
        let cancel = cancel.clone();
        async move {
            let server = axum::serve(listener, router)
                .with_graceful_shutdown(cancel.clone().cancelled_owned());
            let watch = async {
                loop {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    if backend.peer().is_transport_closed() {
                        return;
                    }
                }
            };
            tokio::select! {
                served = server => served.map_err(HelperError::from),
                () = watch => {
                    tracing::error!("Chrome DevTools MCP exited; stopping the Helper");
                    cancel.cancel();
                    Err(HelperError::DevtoolsClosed)
                }
            }
        }
    });
    Ok(RunningHelper {
        mcp_url: format!("http://{addr}/mcp"),
        addr,
        _stop_on_drop: cancel.clone().drop_guard(),
        cancel,
        task,
    })
}
