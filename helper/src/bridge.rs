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
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{browser::LaunchedBrowser, config::HelperError};

/// The Helper's own DevTools commands use ids from here up, so their replies
/// are told apart from the client's (Puppeteer counts up from 1).
const OWN_IDS: u64 = 1 << 50;
/// Large enough for a full-page screenshot.
const MAX_MESSAGE: usize = 256 << 20;
/// How long the browser gets to close by itself before it's stopped.
const CLOSE_GRACE: Duration = Duration::from_secs(5);
/// Messages waiting for a client. One that falls this far behind is dropped,
/// rather than the Helper holding the browser's output in memory.
const CLIENT_BACKLOG: usize = 4096;
/// Commands waiting for the browser to read them.
const PIPE_BACKLOG: usize = 4096;

/// A running bridge.
pub(crate) struct Bridge {
    /// For Chrome DevTools MCP's `wsEndpoint`. Holds the secret.
    pub ws_url: String,
    close: Option<oneshot::Sender<()>>,
    /// Taken once the browser has exited.
    exited: Option<oneshot::Receiver<()>>,
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

    /// Resolves when the browser has exited, for example because the person
    /// quit it. Safe to call again afterwards.
    pub async fn browser_exited(&mut self) {
        if let Some(exited) = self.exited.as_mut() {
            let _ = exited.await;
            self.exited = None;
        }
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

    // Blocking threads read and write the pipe, the same on every platform.
    let reader_events = events.clone();
    std::thread::spawn(move || read_pipe(from_browser, reader_events));
    let (to_pipe, pipe_outbox) = mpsc::channel::<String>(PIPE_BACKLOG);
    std::thread::spawn(move || write_pipe(to_browser, pipe_outbox));

    let (close, close_requested) = oneshot::channel::<()>();
    let (exited_tx, exited) = oneshot::channel();
    tokio::spawn(Relay::new(to_pipe).run(inbox));
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
        events,
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
    /// The top-level ones, which the relay lets go of when the client goes.
    roots: HashSet<String>,
}

struct Relay {
    to_pipe: mpsc::Sender<String>,
    client: Option<Client>,
    /// The relay's id for each command still waiting on the browser, with
    /// the client that sent it and that client's own id.
    pending: HashMap<u64, (u64, Value)>,
    next_id: u64,
    own_id: u64,
}

impl Relay {
    fn new(to_pipe: mpsc::Sender<String>) -> Self {
        Self {
            to_pipe,
            client: None,
            pending: HashMap::new(),
            next_id: 0,
            own_id: OWN_IDS,
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
                        roots: HashSet::new(),
                    });
                }
                Event::Disconnected(id) => {
                    if self.client.as_ref().is_some_and(|c| c.id == id) {
                        self.release().await;
                    }
                }
                Event::Close => self.send_own("Browser.close", json!({}), None).await,
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
            self.next_id += 1;
            let relay_id = self.next_id;
            debug_assert!(relay_id < OWN_IDS);
            self.pending.insert(
                relay_id,
                (client_id, std::mem::replace(own, relay_id.into())),
            );
        }
        let _ = self.to_pipe.send(message.to_string()).await;
    }

    async fn on_browser_message(&mut self, text: String) {
        let Ok(mut message) = serde_json::from_str::<Value>(&text) else {
            return;
        };
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
        match (method, session, &mut self.client) {
            (Some("Target.attachedToTarget"), Some(session), Some(client)) => {
                let ours = parent.as_ref().is_none_or(|p| client.sessions.contains(p));
                if !ours {
                    return;
                }
                if parent.is_none() {
                    client.roots.insert(session.clone());
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
        for session in &client.roots {
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

    async fn send_own(&mut self, method: &str, params: Value, session: Option<&str>) {
        self.own_id += 1;
        let mut message = json!({ "id": self.own_id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = session.into();
        }
        let _ = self.to_pipe.send(message.to_string()).await;
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
