use std::{net::SocketAddr, sync::Arc};

use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
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
    bridge, browser,
    config::{HelperConfig, HelperError},
};

/// The name the Helper gives Agents, and checks for when a second Cohelm
/// finds its port taken.
pub const SERVER_NAME: &str = "cohelm";

/// Serves Agents and passes their tool calls to the person's browser.
#[derive(Clone)]
struct Helper {
    backend: Arc<Backend>,
}

impl ServerHandler for Helper {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
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
        if let Some(reason) = refused(&request) {
            return Ok(CallToolResult::error(vec![ContentBlock::text(reason)]).into());
        }
        self.backend
            .call_tool_once(request)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))
    }
}

/// Steps the Helper never passes to the browser, whatever the Agent asks.
fn refused(request: &CallToolRequestParams) -> Option<&'static str> {
    let changes_identity = request.name == "emulate"
        && request
            .arguments
            .as_ref()
            .is_some_and(|args| args.get("userAgent").is_some_and(|v| !v.is_null()));
    changes_identity.then_some(
        "Cohelm keeps the browser's own user agent, so sites see plain Chrome. \
         Run emulate without userAgent.",
    )
}

/// A running Helper. Calling [`RunningHelper::stop`] closes the browser and
/// waits for the Helper to shut down. Dropping it starts the same shutdown
/// without waiting; the browser is stopped outright if the program ends
/// first.
pub struct RunningHelper {
    /// Where Agents connect, for example `http://127.0.0.1:47600/mcp`.
    pub mcp_url: String,
    pub addr: SocketAddr,
    cancel: CancellationToken,
    task: JoinHandle<Result<(), HelperError>>,
    browser: bridge::Asker,
    _stop_on_drop: DropGuard,
}

impl RunningHelper {
    /// Whether Cohelm's built-in extension is running in the browser.
    pub async fn extension_running(&self) -> Result<bool, HelperError> {
        self.probe().extension_running().await
    }

    /// A handle for asking about the browser while the Helper runs.
    pub fn probe(&self) -> BrowserProbe {
        BrowserProbe {
            browser: self.browser.clone(),
        }
    }

    /// Shut the Helper down and wait for it to finish.
    pub async fn stop(self) {
        self.cancel.cancel();
        let _ = self.task.await;
    }

    /// Wait until the Helper stops on its own: the person quit the browser,
    /// or Chrome DevTools MCP exited.
    pub async fn stopped(&mut self) -> Result<(), HelperError> {
        match (&mut self.task).await {
            Ok(result) => result,
            Err(e) => Err(HelperError::Devtools(e.to_string())),
        }
    }
}

/// Asks the browser about itself, apart from any Agent.
#[derive(Clone)]
pub struct BrowserProbe {
    browser: bridge::Asker,
}

impl BrowserProbe {
    /// Each target's type and the scheme of its address, for the log. No
    /// addresses, so the log holds nothing of where the person has been.
    pub async fn target_kinds(&self) -> Result<Vec<String>, HelperError> {
        let targets = self
            .browser
            .ask("Target.getTargets", serde_json::json!({}))
            .await?;
        Ok(targets["targetInfos"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| {
                let kind = t["type"].as_str().unwrap_or("?");
                let scheme = t["url"]
                    .as_str()
                    .and_then(|u| u.split_once(':'))
                    .map_or("", |(s, _)| s);
                format!("{kind} {scheme}")
            })
            .collect())
    }

    /// Whether Cohelm's built-in extension is running in the browser.
    pub async fn extension_running(&self) -> Result<bool, HelperError> {
        let targets = self
            .browser
            .ask("Target.getTargets", serde_json::json!({}))
            .await?;
        let page = format!("chrome-extension://{}/", crate::EXTENSION_ID);
        Ok(targets["targetInfos"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["url"].as_str())
            .any(|url| url.starts_with(&page)))
    }
}

/// Whether a Cohelm Helper is serving Agents at `addr`, waiting up to `wait`
/// for one that is still starting. Used when a second Cohelm finds the port
/// taken, so it never hands pages to some other program.
pub async fn cohelm_at(addr: SocketAddr, wait: Duration) -> bool {
    let url = format!("http://{addr}/mcp");
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let attempt = tokio::time::timeout_at(
            deadline,
            ().serve(rmcp::transport::StreamableHttpClientTransport::from_uri(
                url.clone(),
            )),
        )
        .await;
        match attempt {
            Ok(Ok(client)) => {
                let named = client
                    .peer_info()
                    .and_then(|info| info.server_info.as_ref().map(|s| s.name == SERVER_NAME))
                    .unwrap_or(false);
                let _ = client.cancel().await;
                return named;
            }
            Ok(Err(_)) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            _ => return false,
        }
    }
}

/// How long Chrome DevTools MCP gets to start and reach the browser. The
/// first run may download it.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);

/// Start the browser, attach to it and start serving Agents.
pub async fn start(config: HelperConfig) -> Result<RunningHelper, HelperError> {
    config.validate()?;
    // Take the port first, so a busy port fails before anything starts.
    let listener = match tokio::net::TcpListener::bind(config.listen).await {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            return Err(HelperError::AlreadyRunning(config.listen));
        }
        Err(e) => return Err(e.into()),
    };
    let addr = listener.local_addr()?;
    let mut bridge = bridge::start(browser::launch(&config.browser)?).await?;
    // Only this user can read the folder (it's created 0700). Chrome DevTools
    // MCP reads the file once on start, so it's removed as soon as the
    // browser answers.
    let private = tempfile::Builder::new()
        .prefix("cohelm-helper-")
        .tempdir()?;
    let devtools_config = backend::write_config(private.path(), &bridge.ws_url)?;
    let connected = tokio::time::timeout(
        CONNECT_TIMEOUT,
        backend::connect(&config.devtools, &devtools_config),
    )
    .await
    .unwrap_or_else(|_| {
        Err(HelperError::BrowserUnreachable(format!(
            "no answer within {} s",
            CONNECT_TIMEOUT.as_secs()
        )))
    });
    drop(private);
    let backend = match connected {
        Ok(backend) => Arc::new(backend),
        Err(e) => {
            bridge.close_browser().await;
            return Err(e);
        }
    };

    let browser = bridge.asker();
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
            let result = tokio::select! {
                served = server => served.map_err(HelperError::from),
                () = watch => {
                    tracing::error!("Chrome DevTools MCP exited; stopping the Helper");
                    Err(HelperError::DevtoolsClosed)
                }
                () = bridge.browser_exited() => {
                    tracing::info!("the browser closed; stopping the Helper");
                    Err(HelperError::BrowserClosed)
                }
            };
            cancel.cancel();
            bridge.close_browser().await;
            result
        }
    });
    Ok(RunningHelper {
        mcp_url: format!("http://{addr}/mcp"),
        addr,
        _stop_on_drop: cancel.clone().drop_guard(),
        cancel,
        task,
        browser,
    })
}
