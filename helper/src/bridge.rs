//! Lets Chrome DevTools MCP reach the browser's private pipe.
//!
//! Chrome DevTools MCP only speaks to a browser over a WebSocket, so the
//! Helper relays one: it listens on loopback at a path holding a fresh random
//! secret, refuses any request a web page sends, and passes messages to and
//! from the pipe. The secret reaches Chrome DevTools MCP in a config file only
//! this user can read, not on its command line (ADR 0005).
//!
//! One client is relayed at a time. When it goes, the Helper detaches the
//! sessions it opened and turns off its auto-attach, so the person's new tabs
//! never wait on a client that's no longer there.

use std::{
    collections::HashSet,
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

/// A running bridge.
pub(crate) struct Bridge {
    /// For Chrome DevTools MCP's `wsEndpoint`. Holds the secret.
    pub ws_url: String,
    close: Option<oneshot::Sender<()>>,
    exited: oneshot::Receiver<()>,
    cancel: CancellationToken,
}

impl Bridge {
    /// Ask the browser to close, and stop it if it hasn't after a few
    /// seconds.
    pub async fn close_browser(mut self) {
        if let Some(close) = self.close.take() {
            let _ = close.send(());
        }
        let _ = (&mut self.exited).await;
        self.cancel.cancel();
    }

    /// Resolves when the browser has exited, for example because the person
    /// quit it.
    pub async fn browser_exited(&mut self) {
        let _ = (&mut self.exited).await;
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
    Connected(u64, mpsc::UnboundedSender<String>),
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
    let (to_pipe, pipe_outbox) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || write_pipe(to_browser, pipe_outbox));

    let (close, close_requested) = oneshot::channel::<()>();
    let (exited_tx, exited) = oneshot::channel();
    tokio::spawn(relay(inbox, to_pipe));
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
        exited,
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

fn write_pipe(mut to_browser: std::io::PipeWriter, outbox: std::sync::mpsc::Receiver<String>) {
    for message in outbox {
        let written = to_browser
            .write_all(message.as_bytes())
            .and_then(|()| to_browser.write_all(&[0]))
            .and_then(|()| to_browser.flush());
        if written.is_err() {
            break;
        }
    }
}

/// What the relay knows about the client it serves now.
#[derive(Default)]
struct Client {
    id: u64,
    tx: Option<mpsc::UnboundedSender<String>>,
    /// Top-level sessions the browser attached for this client.
    sessions: HashSet<String>,
}

async fn relay(
    mut inbox: mpsc::UnboundedReceiver<Event>,
    to_pipe: std::sync::mpsc::Sender<String>,
) {
    let mut client = Client::default();
    let mut own_id = OWN_IDS;
    let mut send_own = |method: &str, params: Value| {
        own_id += 1;
        let message = json!({ "id": own_id, "method": method, "params": params });
        let _ = to_pipe.send(message.to_string());
    };
    while let Some(event) = inbox.recv().await {
        match event {
            Event::FromBrowser(text) => {
                let parsed: Option<Value> = serde_json::from_str(&text).ok();
                if let Some(message) = &parsed {
                    if message
                        .get("id")
                        .and_then(Value::as_u64)
                        .is_some_and(|id| id > OWN_IDS)
                    {
                        continue;
                    }
                    track_sessions(&mut client.sessions, message);
                }
                if let Some(tx) = &client.tx {
                    let _ = tx.send(text);
                }
            }
            Event::Connected(id, tx) => {
                if client.tx.is_some() {
                    tracing::info!("a new DevTools client replaces the old one");
                    release(&mut client, &mut send_own);
                }
                client = Client {
                    id,
                    tx: Some(tx),
                    sessions: HashSet::new(),
                };
            }
            Event::FromClient(id, text) => {
                if id == client.id && client.tx.is_some() {
                    let _ = to_pipe.send(text);
                }
            }
            Event::Disconnected(id) => {
                if id == client.id && client.tx.is_some() {
                    release(&mut client, &mut send_own);
                }
            }
            Event::Close => {
                send_own("Browser.close", json!({}));
            }
            Event::BrowserGone => break,
        }
    }
    // Closing the client's channel ends its WebSocket.
    client.tx = None;
}

/// Undo what a departed client left switched on in the browser.
fn release(client: &mut Client, send_own: &mut impl FnMut(&str, Value)) {
    client.tx = None;
    send_own(
        "Target.setAutoAttach",
        json!({ "autoAttach": false, "waitForDebuggerOnStart": false, "flatten": true }),
    );
    send_own("Target.setDiscoverTargets", json!({ "discover": false }));
    for session in client.sessions.drain() {
        send_own("Target.detachFromTarget", json!({ "sessionId": session }));
    }
}

/// Keep the list of top-level sessions current. Sessions inside a page end
/// with it.
fn track_sessions(sessions: &mut HashSet<String>, message: &Value) {
    if message.get("sessionId").is_some() {
        return;
    }
    let session = || {
        message
            .pointer("/params/sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    match message.get("method").and_then(Value::as_str) {
        Some("Target.attachedToTarget") => {
            if let Some(id) = session() {
                sessions.insert(id);
            }
        }
        Some("Target.detachedFromTarget") => {
            if let Some(id) = session() {
                sessions.remove(&id);
            }
        }
        _ => {}
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
    let (tx, mut outbox) = mpsc::unbounded_channel();
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

    #[test]
    fn top_level_sessions_are_tracked() {
        let mut sessions = HashSet::new();
        track_sessions(
            &mut sessions,
            &json!({ "method": "Target.attachedToTarget", "params": { "sessionId": "A" } }),
        );
        track_sessions(
            &mut sessions,
            &json!({ "method": "Target.attachedToTarget", "sessionId": "A",
                     "params": { "sessionId": "child" } }),
        );
        assert_eq!(sessions, HashSet::from(["A".to_owned()]));
        track_sessions(
            &mut sessions,
            &json!({ "method": "Target.detachedFromTarget", "params": { "sessionId": "A" } }),
        );
        assert!(sessions.is_empty());
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

        // The Helper's own replies aren't passed on.
        let own = format!(r#"{{"id":{},"result":{{}}}}"#, OWN_IDS + 7);
        socket
            .send(tungstenite::Message::Text(own.into()))
            .await
            .unwrap();
        let after = r#"{"id":2,"method":"Target.getTargets"}"#;
        socket
            .send(tungstenite::Message::Text(after.into()))
            .await
            .unwrap();
        assert_eq!(next_text(&mut socket).await, after);

        // A web page (it sends an Origin) and a wrong secret get nowhere.
        let mut from_page = bridge.ws_url.as_str().into_client_request().unwrap();
        from_page
            .headers_mut()
            .insert("origin", "https://evil.example".parse().unwrap());
        assert!(tokio_tungstenite::connect_async(from_page).await.is_err());
        let guessed = format!("{}0", &bridge.ws_url[..bridge.ws_url.len() - 1]);
        assert!(tokio_tungstenite::connect_async(guessed).await.is_err());

        bridge.close_browser().await;
    }
}
