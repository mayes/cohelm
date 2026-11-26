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

use axum::response::IntoResponse;
use rmcp::transport::streamable_http_server::session::SessionManager;

use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::{
    agents::{Agents, SessionId},
    backend::{self, Backend},
    bridge, browser,
    config::{HelperConfig, HelperError},
    pages,
    updates::{self, Update},
};
use tokio::sync::watch;

/// The name the Helper gives Agents, and checks for when a second Cohelm
/// finds its port taken.
pub const SERVER_NAME: &str = "cohelm";

/// How often the Helper checks which Agents' sessions have ended.
const PRUNE_EVERY: Duration = Duration::from_secs(2);

/// Serves Agents and passes their tool calls to the person's browser.
#[derive(Clone)]
struct Helper {
    backend: Arc<Backend>,
    agents: Arc<Agents>,
    /// How long a new Agent's call waits for the person, or None when
    /// nothing in Cohelm can ask them.
    approval_wait: Option<Duration>,
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        // Nothing reaches a tab until the person has allowed this Agent.
        let name = context.client_info().map(|c| c.name).unwrap_or_default();
        let session: Option<SessionId> = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.headers.get("mcp-session-id"))
            .and_then(|id| id.to_str().ok())
            .map(Into::into);
        if let Err(refusal) = self.agents.admit(&name, session, self.approval_wait).await {
            return Ok(CallToolResult::error(vec![ContentBlock::text(refusal.to_string())]).into());
        }
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
    // Cohelm's own pages, where the person answers Cohelm, and the
    // browser's settings are the person's alone.
    let opens_own_page = request
        .arguments
        .as_ref()
        .and_then(|args| args.get("url"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(browser_own_page);
    if opens_own_page {
        return Some(
            "Cohelm's own pages and the browser's settings are for the person, not Agents.",
        );
    }
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

/// Pages an Agent may not open: the browser's own and its extensions'.
/// The address is read as Chromium reads it, which drops tabs and newlines
/// anywhere and control characters and spaces at either end, and looks
/// inside wrappers such as `view-source:`.
fn browser_own_page(url: &str) -> bool {
    let read: String = url
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let mut url = read.trim_matches(|c: char| c <= ' ').to_ascii_lowercase();
    while let Some(inner) = ["view-source:", "filesystem:", "blob:"]
        .iter()
        .find_map(|wrapper| url.strip_prefix(wrapper))
    {
        url = inner.trim_start_matches(|c: char| c <= ' ').to_owned();
    }
    url != "about:blank"
        && [
            "chrome:",
            "chrome-extension:",
            "chrome-untrusted:",
            "chrome-search:",
            "devtools:",
            "about:",
        ]
        .iter()
        .any(|scheme| url.starts_with(scheme))
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
    control: Control,
    _stop_on_drop: DropGuard,
}

/// What the person can do about Agents, from Cohelm's pages or a test.
#[derive(Clone)]
pub(crate) struct Control {
    pub agents: Arc<Agents>,
    sessions: Arc<LocalSessionManager>,
    /// A newer Cohelm, once the Helper has seen one.
    pub update: Arc<watch::Sender<Option<Update>>>,
}

impl Control {
    /// End an Agent's sessions at once and forget its approval.
    pub async fn disconnect(&self, name: &str) {
        for session in self.agents.disconnect(name) {
            let _ = self.sessions.close_session(&session).await;
        }
    }

    /// Keep the connected list to sessions that are still open.
    async fn prune(&self) {
        let mut live = std::collections::HashSet::new();
        for session in self.agents.sessions() {
            if self.sessions.has_session(&session).await.unwrap_or(false) {
                live.insert(session);
            }
        }
        self.agents.prune(|s| live.contains(s));
    }
}

impl RunningHelper {
    /// The Agents the person has allowed, declined or is asked about. The
    /// person answers in Cohelm's prompt; tests answer here.
    pub fn agents(&self) -> &Agents {
        &self.control.agents
    }

    /// End an Agent's sessions at once and forget its approval, as the
    /// person does from Cohelm's Agents list.
    pub async fn disconnect(&self, name: &str) {
        self.control.disconnect(name).await;
    }

    /// A newer Cohelm, if the Helper has seen one.
    pub fn update(&self) -> Option<Update> {
        self.control.update.borrow().clone()
    }

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

    /// Open one of Cohelm's own pages, such as `agents.html`, in a new
    /// window, as the person does from the toolbar. For tests.
    pub async fn open_cohelm_page(&self, page: &str) -> Result<(), HelperError> {
        self.browser
            .ask(
                "Target.createTarget",
                serde_json::json!({
                    "url": format!("chrome-extension://{}/{page}", crate::EXTENSION_ID),
                    "newWindow": true,
                }),
            )
            .await?;
        Ok(())
    }

    /// Run `script` in the open Cohelm page whose address ends with `page`,
    /// as the person would act there, and return what it gives back. For
    /// tests of Cohelm's pages; None when no such page is open.
    pub async fn run_in_cohelm_page(
        &self,
        page: &str,
        script: &str,
    ) -> Result<Option<serde_json::Value>, HelperError> {
        let targets = self
            .browser
            .ask("Target.getTargets", serde_json::json!({}))
            .await?;
        let prefix = format!("chrome-extension://{}/", crate::EXTENSION_ID);
        let Some(target) = targets["targetInfos"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| {
                t["url"].as_str().is_some_and(|u| {
                    u.starts_with(&prefix)
                        && u.split(['?', '#'])
                            .next()
                            .is_some_and(|u| u.ends_with(page))
                })
            })
            .and_then(|t| t["targetId"].as_str())
        else {
            return Ok(None);
        };
        let session = self.browser.attach(target).await?;
        let result = self
            .browser
            .ask_in(
                &session,
                "Runtime.evaluate",
                serde_json::json!({ "expression": script, "awaitPromise": true, "returnByValue": true }),
            )
            .await;
        let _ = self
            .browser
            .ask(
                "Target.detachFromTarget",
                serde_json::json!({ "sessionId": session }),
            )
            .await;
        let result = result?;
        if let Some(thrown) = result.get("exceptionDetails") {
            return Err(HelperError::BrowserUnreachable(format!(
                "the page threw: {thrown}"
            )));
        }
        Ok(Some(result["result"]["value"].clone()))
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
    let control = Control {
        agents: Agents::load(config.agents_file.clone()),
        sessions: Arc::new(LocalSessionManager::default()),
        update: Arc::new(watch::Sender::new(None)),
    };
    if let Some(check) = config.updates.clone() {
        tokio::spawn(updates::run(
            check,
            control.update.clone(),
            cancel.child_token(),
        ));
    }
    let helper = Helper {
        backend: backend.clone(),
        agents: control.agents.clone(),
        approval_wait: config
            .browser
            .extension_dir
            .is_some()
            .then_some(config.approval_wait),
    };
    let service: StreamableHttpService<Helper, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(helper.clone()),
        control.sessions.clone(),
        StreamableHttpServerConfig::default()
            // Host is checked against loopback by default; also refuse any
            // request a web page sends, since Agents don't send an Origin.
            .enforce_origin_validation()
            .with_cancellation_token(cancel.child_token()),
    );
    let mcp_url = format!("http://{addr}/mcp");
    if config.browser.extension_dir.is_some() {
        tokio::spawn(pages::run(
            browser.clone(),
            control.clone(),
            mcp_url.clone(),
            cancel.child_token(),
        ));
    }
    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn(only_this_computer));
    tokio::spawn({
        let control = control.clone();
        let cancel = cancel.clone();
        async move {
            while cancel
                .run_until_cancelled(tokio::time::sleep(PRUNE_EVERY))
                .await
                .is_some()
            {
                control.prune().await;
            }
        }
    });
    let task = tokio::spawn({
        let cancel = cancel.clone();
        async move {
            let server = axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
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
        mcp_url,
        addr,
        _stop_on_drop: cancel.clone().drop_guard(),
        cancel,
        task,
        browser,
        control,
    })
}

/// Refuse a connection from another machine. The Helper listens on loopback
/// only, so this holds even if that ever changed by mistake.
async fn only_this_computer(
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<SocketAddr>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if !peer.ip().is_loopback() {
        tracing::warn!("refused a connection from {}", peer.ip());
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_cant_open_the_browsers_own_pages() {
        for url in [
            "chrome://settings",
            " CHROME-EXTENSION://dojelhkgldmdbffaafmofoilmbbacppo/about.html",
            "chrome-untrusted://print",
            "devtools://devtools/bundled/inspector.html",
            "about:settings",
            "chr\tome-extension://dojelhkgldmdbffaafmofoilmbbacppo/agents.html",
            "\u{1}chrome://settings",
            "view-source:chrome://settings",
            "view-source: blob:chrome-extension://x/y",
        ] {
            assert!(browser_own_page(url), "{url}");
        }
        for url in [
            "https://example.com",
            "about:blank",
            "http://chrome.example",
        ] {
            assert!(!browser_own_page(url), "{url}");
        }
    }
}
