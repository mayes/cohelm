//! Lets Chrome DevTools MCP reach the browser's private pipe.
//!
//! Chrome DevTools MCP only speaks to a browser over a WebSocket, so the
//! Helper relays one: it listens on loopback at a path holding a fresh random
//! secret, refuses any request a web page sends, and passes messages to and
//! from the pipe. The secret reaches Chrome DevTools MCP in a config file only
//! this user can read, not on its command line (ADR 0005).
//!
//! One client is relayed at a time. The relay gives each of the client's
//! commands an id of its own, so a reply only ever reaches the client that
//! asked, even across a reconnect. When a client goes, the Helper turns off
//! its auto-attach and lets go of its tabs, so the person's new tabs never
//! wait on a client that's no longer there.

use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader, Write},
    net::SocketAddr,
    time::Duration,
};

use axum::{
    Router,
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{browser::LaunchedBrowser, config::HelperError};

/// The Helper's own DevTools commands use ids from here to [`MAX_ID`], and the
/// ids it gives the client's commands stay below it, so their replies are told
/// apart.
const OWN_IDS: u64 = 1 << 30;
/// Chromium reads a command's id as a 32-bit signed number. A larger id gets
/// an error with no id at all, so its reply could never be matched.
const MAX_ID: u64 = i32::MAX as u64;
/// How long the Helper waits for the browser to answer one of its own
/// commands.
const ASK_TIMEOUT: Duration = Duration::from_secs(10);
/// Large enough for a full-page screenshot.
const MAX_MESSAGE: usize = 256 << 20;
/// How long the browser gets to close by itself before it's stopped.
const CLOSE_GRACE: Duration = Duration::from_secs(5);
/// Messages waiting for a client. One that falls this far behind is dropped,
/// rather than the Helper holding the browser's output in memory.
const CLIENT_BACKLOG: usize = 4096;
/// Commands waiting for the browser to read them.
const PIPE_BACKLOG: usize = 4096;
/// Events waiting for the Helper's own watchers.
const WATCH_BACKLOG: usize = 256;

/// A running bridge.
pub(crate) struct Bridge {
    /// For Chrome DevTools MCP's `wsEndpoint`. Holds the secret.
    pub ws_url: String,
    close: Option<oneshot::Sender<()>>,
    /// Taken once the browser has exited.
    exited: Option<oneshot::Receiver<()>>,
    events: mpsc::UnboundedSender<Event>,
    watchers: broadcast::Sender<Value>,
    cancel: CancellationToken,
}

impl Bridge {
    /// Ask the browser to close, and stop it if it hasn't after a few
    /// seconds.
    pub async fn close_browser(mut self) {
        if let Some(close) = self.close.take() {
            let _ = close.send(());
        }
        self.browser_exited().await;
        self.cancel.cancel();
    }

    /// A handle for sending the Helper's own commands to the browser.
    pub fn asker(&self) -> Asker {
        Asker {
            events: self.events.clone(),
            watchers: self.watchers.clone(),
        }
    }

    /// Resolves when the browser has exited, for example because the person
    /// quit it. Safe to call again afterwards.
    pub async fn browser_exited(&mut self) {
        if let Some(exited) = self.exited.as_mut() {
            let _ = exited.await;
            self.exited = None;
        }
    }
}

/// Sends the browser DevTools commands of the Helper's own, apart from any
/// client's.
#[derive(Clone)]
pub(crate) struct Asker {
    events: mpsc::UnboundedSender<Event>,
    watchers: broadcast::Sender<Value>,
}

impl Asker {
    /// Send a command and wait for its result.
    pub async fn ask(&self, method: &str, params: Value) -> Result<Value, HelperError> {
        self.send(Command::Browser, method, params).await
    }

    /// Send a command to a target the Helper attached to itself.
    pub async fn ask_in(
        &self,
        session: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, HelperError> {
        self.send(Command::Session(session.to_owned()), method, params)
            .await
    }

    /// Attach to a target for the Helper's own use, apart from any client,
    /// and return the session. Its events come through [`Asker::watch`].
    pub async fn attach(&self, target: &str) -> Result<String, HelperError> {
        let attached = self
            .send(
                Command::Attach,
                "Target.attachToTarget",
                json!({ "targetId": target, "flatten": true }),
            )
            .await?;
        attached["sessionId"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| HelperError::BrowserUnreachable("attached without a session".into()))
    }

    /// The browser's target events, and every event from the Helper's own
    /// sessions. One who falls behind misses some.
    pub fn watch(&self) -> broadcast::Receiver<Value> {
        self.watchers.subscribe()
    }

    async fn send(
        &self,
        command: Command,
        method: &str,
        params: Value,
    ) -> Result<Value, HelperError> {
        let (answer, answered) = oneshot::channel();
        self.events
            .send(Event::Ask(command, method.to_owned(), params, answer))
            .map_err(|_| HelperError::BrowserClosed)?;
        tokio::time::timeout(ASK_TIMEOUT, answered)
            .await
            .map_err(|_| HelperError::BrowserUnreachable(format!("no answer to {method}")))?
            .map_err(|_| HelperError::BrowserClosed)?
            .map_err(HelperError::BrowserUnreachable)
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

enum Event {
    FromBrowser(String),
    BrowserGone,
    Connected(u64, mpsc::Sender<String>),
    FromClient(u64, String),
    Disconnected(u64),
    Close,
    /// A command of the Helper's own, answered with its result or error.
    Ask(
        Command,
        String,
        Value,
        oneshot::Sender<Result<Value, String>>,
    ),
}

/// Where one of the Helper's own commands goes.
enum Command {
    Browser,
    Session(String),
    /// Attaching to a target for the Helper.
    Attach,
}

#[derive(Clone)]
struct Gate {
    secret: String,
    events: mpsc::UnboundedSender<Event>,
    next_client: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

pub(crate) async fn start(browser: LaunchedBrowser) -> Result<Bridge, HelperError> {
    let LaunchedBrowser {
        mut child,
        to_browser,
        from_browser,
    } = browser;
    let secret = new_secret()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr: SocketAddr = listener.local_addr()?;
    let cancel = CancellationToken::new();
    let (events, inbox) = mpsc::unbounded_channel();
    let watchers = broadcast::channel(WATCH_BACKLOG).0;

    // Blocking threads read and write the pipe, the same on every platform.
    let reader_events = events.clone();
    std::thread::spawn(move || read_pipe(from_browser, reader_events));
    let (to_pipe, pipe_outbox) = mpsc::channel::<String>(PIPE_BACKLOG);
    std::thread::spawn(move || write_pipe(to_browser, pipe_outbox));

    let (close, close_requested) = oneshot::channel::<()>();
    let (exited_tx, exited) = oneshot::channel();
    tokio::spawn(Relay::new(to_pipe, watchers.clone()).run(inbox));
    tokio::spawn({
        let events = events.clone();
        async move {
            tokio::select! {
                _ = child.wait() => {}
                asked = close_requested => {
                    if asked.is_ok() {
                        // Let the browser close itself, saving its session;
                        // stop it if it takes too long.
                        let _ = events.send(Event::Close);
                        if tokio::time::timeout(CLOSE_GRACE, child.wait()).await.is_err() {
                            tracing::warn!("the browser didn't close in time; stopping it");
                            let _ = child.kill().await;
                        }
                    } else {
                        // The Bridge was dropped: kill_on_drop stops the browser.
                        let _ = child.kill().await;
                    }
                }
            }
            let _ = exited_tx.send(());
        }
    });

    let gate = Gate {
        secret: secret.clone(),
        events: events.clone(),
        next_client: Default::default(),
    };
    let router = Router::new()
        .route("/devtools/browser/{secret}", get(upgrade))
        .with_state(gate);
    let shutdown = cancel.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await;
    });

    Ok(Bridge {
        ws_url: format!("ws://{addr}/devtools/browser/{secret}"),
        close: Some(close),
        exited: Some(exited),
        events,
        watchers,
        cancel,
    })
}

fn new_secret() -> Result<String, HelperError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| HelperError::Io(std::io::Error::other(e)))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn read_pipe(from_browser: std::io::PipeReader, events: mpsc::UnboundedSender<Event>) {
    let mut reader = BufReader::new(from_browser);
    let mut message = Vec::new();
    loop {
        message.clear();
        match reader.read_until(0, &mut message) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if message.last() == Some(&0) {
                    message.pop();
                }
                let text = String::from_utf8_lossy(&message).into_owned();
                if events.send(Event::FromBrowser(text)).is_err() {
                    return;
                }
            }
        }
    }
    let _ = events.send(Event::BrowserGone);
}

fn write_pipe(mut to_browser: std::io::PipeWriter, mut outbox: mpsc::Receiver<String>) {
    while let Some(message) = outbox.blocking_recv() {
        let written = to_browser
            .write_all(message.as_bytes())
            .and_then(|()| to_browser.write_all(&[0]))
            .and_then(|()| to_browser.flush());
        if written.is_err() {
            break;
        }
    }
}

/// The client the relay serves now.
struct Client {
    id: u64,
    tx: mpsc::Sender<String>,
    /// Every session the browser attached for this client.
    sessions: HashSet<String>,
    /// The top-level ones, with their targets, which the relay lets go of
    /// when the client goes.
    roots: HashMap<String, String>,
}

struct Relay {
    to_pipe: mpsc::Sender<String>,
    client: Option<Client>,
    /// The relay's id for each command still waiting on the browser, with
    /// the client that sent it and that client's own id.
    pending: HashMap<u64, (u64, Value)>,
    next_id: u64,
    own_id: u64,
    /// The Helper's own commands still waiting on the browser.
    asked: HashMap<u64, oneshot::Sender<Result<Value, String>>>,
    /// The Helper's own attaches still waiting on the browser, by command id.
    attaching: HashSet<u64>,
    /// What the browser sent while the Helper was attaching, in order, held
    /// until its reply says which news of an attach is the Helper's.
    held: Vec<String>,
    /// Sessions the Helper attached for itself. The client never hears of
    /// them.
    own_sessions: HashSet<String>,
    watchers: broadcast::Sender<Value>,
}

impl Relay {
    fn new(to_pipe: mpsc::Sender<String>, watchers: broadcast::Sender<Value>) -> Self {
        Self {
            to_pipe,
            client: None,
            pending: HashMap::new(),
            next_id: 0,
            own_id: OWN_IDS - 1,
            asked: HashMap::new(),
            attaching: HashSet::new(),
            held: Vec::new(),
            own_sessions: HashSet::new(),
            watchers,
        }
    }

    async fn run(mut self, mut inbox: mpsc::UnboundedReceiver<Event>) {
        while let Some(event) = inbox.recv().await {
            match event {
                Event::FromBrowser(text) => self.on_browser_message(text).await,
                Event::FromClient(id, text) => self.on_client_message(id, text).await,
                Event::Connected(id, tx) => {
                    if self.client.is_some() {
                        tracing::info!("a new DevTools client replaces the old one");
                        self.release().await;
                    }
                    self.client = Some(Client {
                        id,
                        tx,
                        sessions: HashSet::new(),
                        roots: HashMap::new(),
                    });
                }
                Event::Disconnected(id) => {
                    if self.client.as_ref().is_some_and(|c| c.id == id) {
                        self.release().await;
                    }
                }
                Event::Close => {
                    self.send_own("Browser.close", json!({}), None).await;
                }
                Event::Ask(command, method, params, answer) => {
                    // Forget commands whose asker gave up waiting.
                    self.asked.retain(|_, waiting| !waiting.is_closed());
                    let id = match command {
                        Command::Browser => self.send_own(&method, params, None).await,
                        Command::Session(session) => {
                            self.send_own(&method, params, Some(&session)).await
                        }
                        Command::Attach => {
                            let id = self.send_own(&method, params, None).await;
                            self.attaching.insert(id);
                            id
                        }
                    };
                    self.asked.insert(id, answer);
                }
                Event::BrowserGone => break,
            }
        }
        // Dropping the client's sender ends its WebSocket.
    }

    async fn on_client_message(&mut self, client_id: u64, text: String) {
        if self.client.as_ref().is_none_or(|c| c.id != client_id) {
            return;
        }
        let Ok(mut message) = serde_json::from_str::<Value>(&text) else {
            return;
        };
        if let Some(own) = message.get_mut("id") {
            let relay_id = self.next_relay_id();
            self.pending.insert(
                relay_id,
                (client_id, std::mem::replace(own, relay_id.into())),
            );
        }
        let _ = self.to_pipe.send(message.to_string()).await;
    }

    async fn on_browser_message(&mut self, text: String) {
        let Ok(message) = serde_json::from_str::<Value>(&text) else {
            return;
        };
        if let Some(id) = message.get("id").and_then(Value::as_u64) {
            let attached = self.attaching.remove(&id);
            if attached
                && let Some(session) = message.pointer("/result/sessionId").and_then(Value::as_str)
            {
                self.own_sessions.insert(session.to_owned());
            }
            if let Some(answer) = self.asked.remove(&id) {
                let result = match message.get("error") {
                    Some(error) => Err(error.to_string()),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = answer.send(result);
            } else if id < OWN_IDS {
                self.hold_or_route(text, message).await;
            }
            if attached {
                self.release_held().await;
            }
            return;
        }
        if let Some(error) = message.get("error") {
            // The browser couldn't read a command well enough to say which.
            tracing::warn!("the browser refused a command: {error}");
            return;
        }
        self.hold_or_route(text, message).await;
    }

    /// While the Helper is attaching to a target, the browser's news of that
    /// attach can't yet be told from the client's, so everything bound for
    /// the client waits, in order, until the Helper's reply comes.
    async fn hold_or_route(&mut self, text: String, message: Value) {
        let own_event = message
            .get("sessionId")
            .and_then(Value::as_str)
            .is_some_and(|p| self.own_sessions.contains(p));
        if !own_event && !self.attaching.is_empty() {
            self.held.push(text);
            return;
        }
        self.route(text, message).await;
    }

    async fn release_held(&mut self) {
        if !self.attaching.is_empty() {
            return;
        }
        for text in std::mem::take(&mut self.held) {
            if let Ok(message) = serde_json::from_str::<Value>(&text) {
                self.route(text, message).await;
            }
        }
    }

    /// Pass a reply or event to whoever it's for: the Helper, the client, or
    /// no one.
    async fn route(&mut self, text: String, mut message: Value) {
        if let Some(id) = message.get("id").and_then(Value::as_u64) {
            // A reply: back to the client that asked, under its own id, and
            // to no one else.
            let Some((asker, own)) = self.pending.remove(&id) else {
                return;
            };
            if self.client.as_ref().is_some_and(|c| c.id == asker) {
                message["id"] = own;
                self.send_to_client(message.to_string()).await;
            }
            return;
        }
        let parent = message
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let method = message.get("method").and_then(Value::as_str);
        let session = message
            .pointer("/params/sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned);

        // The Helper's own sessions, and what it watches of the browser.
        if parent
            .as_ref()
            .is_some_and(|p| self.own_sessions.contains(p))
        {
            let _ = self.watchers.send(message);
            return;
        }
        let info = message.pointer("/params/targetInfo");
        let cohelm_page = info.is_some_and(crate::pages::cohelm_target);
        match (method, &session) {
            (Some("Target.attachedToTarget"), Some(attached))
                if self.own_sessions.contains(attached) =>
            {
                return;
            }
            (Some("Target.detachedFromTarget"), Some(gone)) if self.own_sessions.remove(gone) => {
                let _ = self.watchers.send(message);
                return;
            }
            // Cohelm's own pages are the person's: the client is never
            // attached to them, so an Agent can't answer for the person.
            (Some("Target.attachedToTarget"), Some(attached))
                if parent.is_none() && cohelm_page =>
            {
                let attached = attached.clone();
                self.let_go(&attached).await;
                return;
            }
            (Some(m), _) if parent.is_none() && m.starts_with("Target.target") => {
                let _ = self.watchers.send(message.clone());
                if m == "Target.targetInfoChanged" && cohelm_page {
                    // A tab that became one of Cohelm's pages.
                    let target = info.and_then(|i| i["targetId"].as_str());
                    let theirs = self.client.as_ref().and_then(|c| {
                        c.roots
                            .iter()
                            .find(|(_, t)| Some(t.as_str()) == target)
                            .map(|(s, _)| s.clone())
                    });
                    if let Some(theirs) = theirs {
                        self.let_go(&theirs).await;
                    }
                }
            }
            _ => {}
        }

        match (method, session, &mut self.client) {
            (Some("Target.attachedToTarget"), Some(session), Some(client)) => {
                let ours = parent.as_ref().is_none_or(|p| client.sessions.contains(p));
                if !ours {
                    return;
                }
                if parent.is_none() {
                    let target = info
                        .and_then(|i| i["targetId"].as_str())
                        .unwrap_or_default();
                    client.roots.insert(session.clone(), target.to_owned());
                }
                client.sessions.insert(session);
            }
            (Some("Target.attachedToTarget"), Some(session), None) => {
                // Attached as the last client left: let the tab go on.
                if parent.is_none() {
                    let session = session.clone();
                    self.let_go(&session).await;
                }
                return;
            }
            (Some("Target.detachedFromTarget"), Some(session), Some(client)) => {
                client.sessions.remove(&session);
                client.roots.remove(&session);
                if parent
                    .as_ref()
                    .is_some_and(|p| !client.sessions.contains(p))
                {
                    return;
                }
            }
            (_, _, Some(client)) => {
                // Events inside a session go only to the client that owns it.
                if parent
                    .as_ref()
                    .is_some_and(|p| !client.sessions.contains(p))
                {
                    return;
                }
            }
            (_, _, None) => return,
        }
        self.send_to_client(text).await;
    }

    async fn send_to_client(&mut self, text: String) {
        let Some(client) = &self.client else {
            return;
        };
        if let Err(mpsc::error::TrySendError::Full(_)) = client.tx.try_send(text) {
            tracing::warn!("the DevTools client fell behind; disconnecting it");
            self.release().await;
        }
    }

    /// Undo what a departed client left switched on in the browser.
    async fn release(&mut self) {
        let Some(client) = self.client.take() else {
            return;
        };
        self.pending.retain(|_, (asker, _)| *asker != client.id);
        self.send_own(
            "Target.setAutoAttach",
            json!({ "autoAttach": false, "waitForDebuggerOnStart": false, "flatten": true }),
            None,
        )
        .await;
        self.send_own(
            "Target.setDiscoverTargets",
            json!({ "discover": false }),
            None,
        )
        .await;
        for session in client.roots.keys() {
            self.let_go(session).await;
        }
    }

    /// Resume a tab that may be waiting for a debugger, and detach from it.
    async fn let_go(&mut self, session: &str) {
        self.send_own("Runtime.runIfWaitingForDebugger", json!({}), Some(session))
            .await;
        self.send_own(
            "Target.detachFromTarget",
            json!({ "sessionId": session }),
            None,
        )
        .await;
    }

    /// The next id for a client's command: 1 up to just below [`OWN_IDS`],
    /// then round again.
    fn next_relay_id(&mut self) -> u64 {
        self.next_id = self.next_id % (OWN_IDS - 1) + 1;
        self.next_id
    }

    /// The next id for one of the Helper's own commands: [`OWN_IDS`] up to
    /// [`MAX_ID`], then round again.
    fn next_own_id(&mut self) -> u64 {
        self.own_id = if self.own_id >= MAX_ID {
            OWN_IDS
        } else {
            self.own_id + 1
        };
        self.own_id
    }

    async fn send_own(&mut self, method: &str, params: Value, session: Option<&str>) -> u64 {
        let id = self.next_own_id();
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = session.into();
        }
        let _ = self.to_pipe.send(message.to_string()).await;
        id
    }
}

async fn upgrade(
    State(gate): State<Gate>,
    Path(secret): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // A web page always sends an Origin; Chrome DevTools MCP never does.
    if headers.contains_key(header::ORIGIN) || !same_secret(&secret, &gate.secret) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let id = gate
        .next_client
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        + 1;
    ws.max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| serve_client(socket, id, gate.events))
}

fn same_secret(given: &str, expected: &str) -> bool {
    // Compare every byte, so the time taken says nothing about the secret.
    given.len() == expected.len()
        && given
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

async fn serve_client(mut socket: WebSocket, id: u64, events: mpsc::UnboundedSender<Event>) {
    let (tx, mut outbox) = mpsc::channel(CLIENT_BACKLOG);
    if events.send(Event::Connected(id, tx)).is_err() {
        return;
    }
    loop {
        tokio::select! {
            outgoing = outbox.recv() => match outgoing {
                Some(text) => {
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                // Replaced by a newer client, or the browser is gone.
                None => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if events.send(Event::FromClient(id, text.to_string())).is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Binary(bytes))) => {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    if events.send(Event::FromClient(id, text)).is_err() {
                        break;
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
        }
    }
    let _ = socket.send(Message::Close(None)).await;
    let _ = events.send(Event::Disconnected(id));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_compare_whole() {
        assert!(same_secret("abc123", "abc123"));
        assert!(!same_secret("abc124", "abc123"));
        assert!(!same_secret("abc12", "abc123"));
        assert!(!same_secret("", "abc123"));
    }

    #[test]
    fn ids_stay_within_what_chromium_reads() {
        let (to_pipe, _outbox) = mpsc::channel(1);
        let mut relay = Relay::new(to_pipe, broadcast::channel(1).0);
        assert_eq!(relay.next_relay_id(), 1);
        assert_eq!(relay.next_own_id(), OWN_IDS);
        relay.next_id = OWN_IDS - 1;
        assert_eq!(
            relay.next_relay_id(),
            1,
            "client ids go round below OWN_IDS"
        );
        relay.own_id = MAX_ID;
        assert_eq!(relay.next_own_id(), OWN_IDS, "own ids go round at MAX_ID");
    }

    #[tokio::test]
    async fn the_helpers_own_sessions_stay_apart_from_the_clients() {
        let (to_pipe, mut pipe) = mpsc::channel(16);
        let watchers = broadcast::channel(16).0;
        let mut watch = watchers.subscribe();
        let (events, inbox) = mpsc::unbounded_channel();
        tokio::spawn(Relay::new(to_pipe, watchers).run(inbox));
        let (client, mut heard) = mpsc::channel(16);
        events.send(Event::Connected(1, client)).unwrap();
        let from_browser = |m: Value| events.send(Event::FromBrowser(m.to_string())).unwrap();
        let attach = |target: &str| {
            let (answer, answered) = oneshot::channel();
            events
                .send(Event::Ask(
                    Command::Attach,
                    "Target.attachToTarget".into(),
                    json!({ "targetId": target, "flatten": true }),
                    answer,
                ))
                .unwrap();
            answered
        };
        let sent_id = |text: String| serde_json::from_str::<Value>(&text).unwrap()["id"].clone();
        let attached = |session: &str, target: &str| {
            json!({ "method": "Target.attachedToTarget", "params": {
                "sessionId": session, "targetInfo": { "targetId": target }, "waitingForDebugger": false } })
        };

        // The Helper's attach: the client never hears of it.
        let answered = attach("T1");
        let id = sent_id(pipe.recv().await.unwrap());
        from_browser(attached("own", "T1"));
        from_browser(json!({ "id": id, "result": { "sessionId": "own" } }));
        assert_eq!(answered.await.unwrap().unwrap()["sessionId"], "own");
        from_browser(
            json!({ "sessionId": "own", "method": "Runtime.bindingCalled", "params": {} }),
        );
        let seen = tokio::time::timeout(Duration::from_secs(1), watch.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(seen["method"], "Runtime.bindingCalled");

        // The client's attach to a target the Helper is attaching to still
        // reaches the client.
        let answered = attach("T2");
        let id = sent_id(pipe.recv().await.unwrap());
        from_browser(attached("theirs", "T2"));
        from_browser(attached("own2", "T2"));
        from_browser(json!({ "id": id, "result": { "sessionId": "own2" } }));
        answered.await.unwrap().unwrap();
        let got: Value = serde_json::from_str(
            &tokio::time::timeout(Duration::from_secs(1), heard.recv())
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(got["params"]["sessionId"], "theirs");
        from_browser(json!({ "sessionId": "own2", "method": "Runtime.consoleAPICalled" }));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            heard.try_recv().is_err(),
            "the client hears nothing of the Helper's sessions"
        );

        // Two attaches of the Helper's own to one target: neither reaches
        // the client.
        let (first, second) = (attach("T3"), attach("T3"));
        let ids = [
            sent_id(pipe.recv().await.unwrap()),
            sent_id(pipe.recv().await.unwrap()),
        ];
        from_browser(attached("own3", "T3"));
        from_browser(json!({ "id": ids[0], "result": { "sessionId": "own3" } }));
        from_browser(attached("own4", "T3"));
        from_browser(json!({ "id": ids[1], "result": { "sessionId": "own4" } }));
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(heard.try_recv().is_err(), "the client hears of neither");

        // The client is never attached to Cohelm's own pages: the relay lets
        // the page go instead of telling the client.
        from_browser(json!({ "method": "Target.attachedToTarget", "params": {
            "sessionId": "page", "waitingForDebugger": true, "targetInfo": {
                "targetId": "P", "type": "page",
                "url": format!("chrome-extension://{}/allow.html", crate::EXTENSION_ID) } } }));
        let resumed: Value = serde_json::from_str(&pipe.recv().await.unwrap()).unwrap();
        assert_eq!(resumed["method"], "Runtime.runIfWaitingForDebugger");
        assert_eq!(resumed["sessionId"], "page");
        let detached: Value = serde_json::from_str(&pipe.recv().await.unwrap()).unwrap();
        assert_eq!(detached["method"], "Target.detachFromTarget");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            heard.try_recv().is_err(),
            "the client isn't told of Cohelm's page"
        );
    }

    #[test]
    fn a_new_secret_each_time() {
        let a = new_secret().unwrap();
        let b = new_secret().unwrap();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }
}

/// The bridge against a stand-in browser that echoes every message back.
#[cfg(all(test, unix))]
mod pipe_tests {
    use std::os::unix::fs::PermissionsExt;

    use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};

    use super::*;
    use crate::browser::{BrowserCommand, launch};

    fn echo_browser(dir: &std::path::Path) -> BrowserCommand {
        let program = dir.join("echo-browser");
        std::fs::write(&program, "#!/bin/sh\nexec cat <&3 >&4\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        BrowserCommand::new(program, dir.join("profile"))
    }

    async fn next_text(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> String {
        use futures::StreamExt;
        loop {
            match tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .expect("a message within 5 s")
            {
                Some(Ok(tungstenite::Message::Text(text))) => return text.to_string(),
                Some(Ok(_)) => continue,
                other => panic!("socket ended: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn messages_pass_through_the_pipe_and_strangers_are_refused() {
        use futures::SinkExt;
        let dir = tempfile::tempdir().unwrap();
        let bridge = start(launch(&echo_browser(dir.path())).unwrap())
            .await
            .unwrap();

        let (mut socket, _) = tokio_tungstenite::connect_async(&bridge.ws_url)
            .await
            .expect("the Helper's client connects");
        let command = r#"{"id":1,"method":"Browser.getVersion"}"#;
        socket
            .send(tungstenite::Message::Text(command.into()))
            .await
            .unwrap();
        assert_eq!(next_text(&mut socket).await, command);

        // The client's ids are its own, whatever they are.
        let big = format!(r#"{{"id":{},"method":"Target.getTargets"}}"#, OWN_IDS + 7);
        socket
            .send(tungstenite::Message::Text(big.clone().into()))
            .await
            .unwrap();
        assert_eq!(next_text(&mut socket).await, big);

        // The Helper's own commands are answered to the Helper, not the
        // client. The echo browser answers with no result.
        let answer = bridge
            .asker()
            .ask("Browser.getVersion", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(answer, Value::Null);

        // A web page (it sends an Origin) and a wrong secret get nowhere.
        let mut from_page = bridge.ws_url.as_str().into_client_request().unwrap();
        from_page
            .headers_mut()
            .insert("origin", "https://evil.example".parse().unwrap());
        assert!(tokio_tungstenite::connect_async(from_page).await.is_err());
        // Change the secret's last digit; it must differ from the real one.
        let (head, last) = bridge.ws_url.split_at(bridge.ws_url.len() - 1);
        let guessed = format!("{head}{}", if last == "0" { "1" } else { "0" });
        assert!(tokio_tungstenite::connect_async(guessed).await.is_err());

        bridge.close_browser().await;
    }

    #[tokio::test]
    async fn a_new_client_replaces_the_old_one() {
        use futures::{SinkExt, StreamExt};
        let dir = tempfile::tempdir().unwrap();
        let bridge = start(launch(&echo_browser(dir.path())).unwrap())
            .await
            .unwrap();
        let command = r#"{"id":5,"method":"Browser.getVersion"}"#;

        let (mut old, _) = tokio_tungstenite::connect_async(&bridge.ws_url)
            .await
            .unwrap();
        old.send(tungstenite::Message::Text(command.into()))
            .await
            .unwrap();
        assert_eq!(next_text(&mut old).await, command);

        let (mut new, _) = tokio_tungstenite::connect_async(&bridge.ws_url)
            .await
            .unwrap();
        new.send(tungstenite::Message::Text(command.into()))
            .await
            .unwrap();
        assert_eq!(next_text(&mut new).await, command);

        // The old client is let go and hears nothing more.
        let ended = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match old.next().await {
                    Some(Ok(tungstenite::Message::Text(text))) => {
                        panic!("the old client got {text}")
                    }
                    Some(Ok(_)) => continue,
                    _ => break,
                }
            }
        })
        .await;
        assert!(ended.is_ok(), "the old client's connection ends");

        bridge.close_browser().await;
    }

    #[tokio::test]
    async fn quitting_the_browser_ends_the_bridge_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("quick-browser");
        std::fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let command = BrowserCommand::new(program, dir.path().join("profile"));
        let mut bridge = start(launch(&command).unwrap()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), bridge.browser_exited())
            .await
            .expect("the bridge sees the browser exit");
        // The Helper closes the bridge after the browser has gone.
        tokio::time::timeout(Duration::from_secs(5), bridge.close_browser())
            .await
            .expect("closing after the browser has gone returns at once");
    }
}
