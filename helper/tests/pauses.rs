//! A risky step waits at a Pause: it doesn't reach the page until the
//! person allows it, a blocked or unanswered one never does, and the Agent
//! is told it's waiting. The person answers in Cohelm's prompt; most of
//! these tests answer through the Helper's own hook, at the MCP seam.
//! Needs a Chromium build, as attach.rs does.

use std::time::Duration;

use cohelm_helper::{Kind, RunningHelper, start};
use rmcp::{
    ClientHandler, RoleClient, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation,
        NumberOrString, ProgressNotificationParam, ProgressToken, RequestParamsMeta,
    },
    service::{NotificationContext, Peer, RunningService},
    transport::StreamableHttpClientTransport,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

mod common;
use common::{TEST_AGENT, args, helper_config, start_site, test_chrome, text_of};

/// An Agent that hears progress, as an MCP client showing a long call does.
#[derive(Clone)]
struct Listening {
    progress: mpsc::UnboundedSender<String>,
}

impl ClientHandler for Listening {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new(TEST_AGENT, "1.0"),
        )
    }

    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        let _ = self.progress.send(params.message.unwrap_or_default());
    }
}

type Agent = RunningService<RoleClient, Listening>;

async fn connect(helper: &RunningHelper) -> (Agent, mpsc::UnboundedReceiver<String>) {
    helper.agents().answer(TEST_AGENT, true);
    let (progress, heard) = mpsc::unbounded_channel();
    let agent = Listening { progress }
        .serve(StreamableHttpClientTransport::from_uri(
            helper.mcp_url.clone(),
        ))
        .await
        .expect("Agent connects");
    (agent, heard)
}

async fn start_helper(pause_wait: Duration) -> (RunningHelper, String) {
    let chrome = test_chrome().expect("checked by the caller");
    let site = start_site().await;
    let profile = tempfile::tempdir().unwrap().keep();
    let mut config = helper_config(chrome, &profile, &format!("{site}/compose"));
    config.pause_wait = pause_wait;
    (start(config).await.expect("Helper starts"), site)
}

/// The uid of the element named `name` in a snapshot.
fn uid_of(snapshot: &str, name: &str) -> String {
    uid_where(snapshot, &format!("\"{name}\""))
}

/// The uid of the first element whose line holds `text`.
fn uid_where(snapshot: &str, text: &str) -> String {
    let name = text;
    snapshot
        .lines()
        .find(|l| l.contains(text))
        .and_then(|l| l.trim_start().strip_prefix("uid="))
        .and_then(|l| l.split(' ').next())
        .unwrap_or_else(|| panic!("{name} in {snapshot}"))
        .to_owned()
}

/// The compose tab's id. Chrome DevTools MCP routes page tools by pageId,
/// so every call below names it.
async fn tab(agent: &Peer<RoleClient>) -> u64 {
    for _ in 0..50 {
        let pages = agent
            .call_tool(CallToolRequestParams::new("list_pages"))
            .await
            .unwrap();
        let pages = text_of(&pages);
        if let Some(line) = pages.lines().find(|l| l.contains("/compose")) {
            return line
                .split(':')
                .next()
                .and_then(|id| id.trim().parse().ok())
                .expect("tab has an id");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the compose tab is listed");
}

/// `arguments` with the compose tab's id added.
async fn on_tab(agent: &Peer<RoleClient>, mut arguments: Value) -> Value {
    let id = tab(agent).await;
    arguments["pageId"] = json!(id);
    arguments
}

async fn snapshot(agent: &Peer<RoleClient>) -> String {
    let arguments = on_tab(agent, json!({})).await;
    let result = agent
        .call_tool(CallToolRequestParams::new("take_snapshot").with_arguments(args(arguments)))
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true), "{}", text_of(&result));
    text_of(&result)
}

async fn click(agent: &Peer<RoleClient>, uid: &str) -> Result<CallToolResult, rmcp::ServiceError> {
    let arguments = on_tab(agent, json!({ "uid": uid })).await;
    let mut request = CallToolRequestParams::new("click").with_arguments(args(arguments));
    request.set_progress_token(ProgressToken(NumberOrString::Number(7)));
    agent.call_tool(request).await
}

async fn call(agent: &Peer<RoleClient>, tool: &'static str, arguments: Value) -> CallToolResult {
    let arguments = on_tab(agent, arguments).await;
    agent
        .call_tool(CallToolRequestParams::new(tool).with_arguments(args(arguments)))
        .await
        .unwrap()
}

/// Take a step that should wait, check what the person is asked, then
/// block it and check the Agent is told.
async fn blocked(
    helper: &RunningHelper,
    agent: &Peer<RoleClient>,
    tool: &'static str,
    arguments: Value,
) -> cohelm_helper::Step {
    let held = tokio::spawn({
        let agent = agent.clone();
        async move { call(&agent, tool, arguments).await }
    });
    let id = paused(helper).await;
    let step = helper.pauses().waiting()[0].step.clone();
    helper.pauses().answer(id, false);
    let result = held.await.unwrap();
    assert!(
        text_of(&result).contains("The person blocked this step"),
        "{tool}: {}",
        text_of(&result)
    );
    step
}

/// Wait until a Pause is waiting, and give its id.
async fn paused(helper: &RunningHelper) -> u64 {
    let mut changed = helper.pauses().watch();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(waiting) = helper.pauses().waiting().first() {
                return waiting.id;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .expect("a Pause waits for the person")
}

#[tokio::test]
async fn a_risky_step_waits_until_the_person_allows_or_blocks_it() {
    if test_chrome().is_none() {
        return;
    }
    let (helper, site) = start_helper(Duration::from_secs(30)).await;
    let (agent, mut heard) = connect(&helper).await;
    let peer = agent.peer().clone();
    let page = snapshot(&peer).await;

    // A plain click goes straight through.
    let more = click(&peer, &uid_of(&page, "Show more")).await.unwrap();
    assert_ne!(more.is_error, Some(true), "{}", text_of(&more));
    assert!(helper.pauses().waiting().is_empty());
    assert!(snapshot(&peer).await.contains("More shown"));

    // Sending waits, and nothing reaches the page meanwhile.
    let send = uid_of(&page, "Send message");
    let held = tokio::spawn({
        let peer = peer.clone();
        let send = send.clone();
        async move { click(&peer, &send).await }
    });
    let id = paused(&helper).await;
    let waiting = &helper.pauses().waiting()[0];
    assert_eq!(waiting.agent, TEST_AGENT);
    assert_eq!(waiting.step.kind, Kind::Send);
    assert_eq!(waiting.step.action, "click “Send message”");
    let host = site.trim_start_matches("http://");
    assert_eq!(waiting.step.site.as_deref(), Some(host));
    let told = tokio::time::timeout(Duration::from_secs(10), heard.recv())
        .await
        .expect("the Agent is told it's waiting")
        .unwrap();
    assert!(told.starts_with("Waiting for the person"), "{told}");
    assert!(snapshot(&peer).await.contains("Not sent"));
    assert!(!held.is_finished());

    helper.pauses().answer(id, true);
    let allowed = held.await.unwrap().unwrap();
    assert_ne!(allowed.is_error, Some(true), "{}", text_of(&allowed));
    assert!(snapshot(&peer).await.contains("Message sent"));

    // Blocked: the Agent is told, and the page never sees it.
    // A fresh copy of the page, opened by its address: a reload would
    // send the form again, so it waits too.
    let fresh = call(
        &peer,
        "navigate_page",
        json!({ "type": "url", "url": format!("{site}/compose") }),
    )
    .await;
    assert_ne!(fresh.is_error, Some(true), "{}", text_of(&fresh));
    let page = snapshot(&peer).await;
    let held = tokio::spawn({
        let peer = peer.clone();
        let send = uid_of(&page, "Send message");
        async move { click(&peer, &send).await }
    });
    helper.pauses().answer(paused(&helper).await, false);
    let blocked = held.await.unwrap().unwrap();
    assert_eq!(blocked.is_error, Some(true));
    assert!(
        text_of(&blocked).contains("The person blocked this step"),
        "{}",
        text_of(&blocked)
    );
    assert!(snapshot(&peer).await.contains("Not sent"));

    agent.cancel().await.unwrap();
    helper.stop().await;
}

#[tokio::test]
async fn an_unanswered_pause_times_out_as_blocked() {
    if test_chrome().is_none() {
        return;
    }
    let (helper, _) = start_helper(Duration::from_secs(2)).await;
    let (agent, _heard) = connect(&helper).await;
    let peer = agent.peer().clone();
    let page = snapshot(&peer).await;
    let result = click(&peer, &uid_of(&page, "Send message")).await.unwrap();
    assert_eq!(result.is_error, Some(true));
    assert!(
        text_of(&result).contains("Nobody answered the Pause in 2 seconds"),
        "{}",
        text_of(&result)
    );
    assert!(helper.pauses().waiting().is_empty());
    assert!(snapshot(&peer).await.contains("Not sent"));
    agent.cancel().await.unwrap();
    helper.stop().await;
}

#[tokio::test]
async fn the_person_answers_a_pause_in_cohelms_prompt() {
    if test_chrome().is_none() {
        return;
    }
    let (helper, site) = start_helper(Duration::from_secs(30)).await;
    let host = site.trim_start_matches("http://").to_owned();
    let (agent, _heard) = connect(&helper).await;
    let peer = agent.peer().clone();
    let page = snapshot(&peer).await;
    let held = tokio::spawn({
        let peer = peer.clone();
        let send = uid_of(&page, "Send message");
        async move { click(&peer, &send).await }
    });
    paused(&helper).await;

    let probe = helper.probe();
    let mut shown = Value::Null;
    for _ in 0..150 {
        shown = probe
            .run_in_cohelm_page(
                "pause.html",
                "document.getElementById('prompt').hidden || document.getElementById('allow').disabled
                    ? null
                    : document.querySelector('h1').textContent + ' | ' + document.getElementById('site').textContent + ' | ' + document.getElementById('may').textContent",
            )
            .await
            .ok()
            .flatten()
            .unwrap_or(Value::Null);
        if !shown.is_null() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        shown,
        json!(format!(
            "{TEST_AGENT} wants to click “Send message” | {host} | This may send something in your name."
        ))
    );
    probe
        .run_in_cohelm_page(
            "pause.html",
            "document.getElementById('allow').click(); true",
        )
        .await
        .unwrap();
    let allowed = tokio::time::timeout(Duration::from_secs(10), held)
        .await
        .expect("the step goes ahead once allowed")
        .unwrap()
        .unwrap();
    assert_ne!(allowed.is_error, Some(true), "{}", text_of(&allowed));
    assert!(snapshot(&peer).await.contains("Message sent"));
    agent.cancel().await.unwrap();
    helper.stop().await;
}

#[tokio::test]
async fn a_step_reads_the_page_as_it_is_now() {
    if test_chrome().is_none() {
        return;
    }
    let (helper, _) = start_helper(Duration::from_secs(30)).await;
    let (agent, _heard) = connect(&helper).await;
    let peer = agent.peer().clone();
    let page = snapshot(&peer).await;
    let next = uid_of(&page, "Continue");

    // The page rewords the button after the Agent looked. Clicking it by
    // the old uid asks about what it says now.
    let reworded = call(
        &peer,
        "click",
        json!({ "uid": uid_of(&page, "Change wording") }),
    )
    .await;
    assert_ne!(reworded.is_error, Some(true), "{}", text_of(&reworded));
    let step = blocked(&helper, &peer, "click", json!({ "uid": next })).await;
    assert_eq!(step.action, "click “Post it”");
    assert_eq!(step.kind, Kind::Send);

    let gone = call(&peer, "click", json!({ "uid": "999_1" })).await;
    assert_eq!(gone.is_error, Some(true));
    assert!(
        text_of(&gone).contains("isn't on the page"),
        "{}",
        text_of(&gone)
    );
    assert!(helper.pauses().waiting().is_empty());
    assert!(snapshot(&peer).await.contains("Not sent"));
    agent.cancel().await.unwrap();
    helper.stop().await;
}

#[tokio::test]
async fn enter_and_scripts_wait_however_they_are_sent() {
    if test_chrome().is_none() {
        return;
    }
    let (helper, _) = start_helper(Duration::from_secs(30)).await;
    let (agent, _heard) = connect(&helper).await;
    let peer = agent.peer().clone();
    let page = snapshot(&peer).await;
    let note = uid_where(&page, "textbox \"Note\"");

    let step = blocked(
        &helper,
        &peer,
        "fill",
        json!({ "uid": note, "value": "hi\n" }),
    )
    .await;
    assert_eq!(step.action, "type into “Note” and press Enter");
    assert_eq!(step.detail.as_deref(), Some("hi\n"));
    let step = blocked(&helper, &peer, "type_text", json!({ "text": "hello\n" })).await;
    assert_eq!(step.detail.as_deref(), Some("hello\n"));
    blocked(&helper, &peer, "press_key", json!({ "key": "NumpadEnter" })).await;
    // With nothing focused, keys can be the page's shortcuts.
    let step = blocked(&helper, &peer, "press_key", json!({ "key": "Delete" })).await;
    assert_eq!(step.kind, Kind::Delete);
    let step = blocked(&helper, &peer, "navigate_page", json!({ "type": "reload" })).await;
    assert_eq!(step.action, "reload the page");
    let send = "document.getElementById('status').textContent = 'Message sent'";
    let step = blocked(
        &helper,
        &peer,
        "navigate_page",
        json!({ "type": "reload", "initScript": send }),
    )
    .await;
    assert_eq!(step.action, "run a script when the page loads");
    blocked(
        &helper,
        &peer,
        "navigate_page",
        json!({ "type": "url", "url": format!("javascript:{send}") }),
    )
    .await;
    assert!(snapshot(&peer).await.contains("Not sent"));
    agent.cancel().await.unwrap();
    helper.stop().await;
}

#[tokio::test]
async fn disconnecting_an_agent_blocks_its_waiting_step() {
    if test_chrome().is_none() {
        return;
    }
    let (helper, _) = start_helper(Duration::from_secs(30)).await;
    let (agent, _heard) = connect(&helper).await;
    let peer = agent.peer().clone();
    let page = snapshot(&peer).await;
    let held = tokio::spawn({
        let peer = peer.clone();
        let send = uid_of(&page, "Send message");
        async move { click(&peer, &send).await }
    });
    paused(&helper).await;
    helper.disconnect(TEST_AGENT).await;
    let mut changed = helper.pauses().watch();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !helper.pauses().waiting().is_empty() {
            changed.changed().await.unwrap();
        }
    })
    .await
    .expect("the Pause goes away with the Agent");
    // The call ends one way or another: blocked, or its session closed.
    let _ = tokio::time::timeout(Duration::from_secs(10), held).await;

    let (again, _heard) = connect(&helper).await;
    assert!(snapshot(again.peer()).await.contains("Not sent"));
    let _ = agent.cancel().await;
    again.cancel().await.unwrap();
    helper.stop().await;
}
