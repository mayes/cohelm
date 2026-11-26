//! Cohelm's own pages in the browser: the prompts where the person allows or
//! declines a new Agent and answers a Pause, and the Agents list.
//!
//! The pages belong to Cohelm's built-in extension. The Helper reaches them
//! over its private DevTools pipe, not over the network: it attaches to each
//! Cohelm page it finds, gives the page a `cohelmSend` function to answer
//! through, and pushes it the lists with `cohelmReceive` (ADR 0007). No other
//! program on the computer can answer for the person this way, and Agents
//! can't open these pages.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{EXTENSION_ID, bridge::Asker, config::HelperError, server::Control, updates::Update};

/// The page that asks the person about a new Agent.
pub(crate) const PROMPT: &str = "allow.html";
/// The page that asks the person about a risky step.
pub(crate) const PAUSE: &str = "pause.html";
/// The function each Cohelm page answers through.
const BINDING: &str = "cohelmSend";
/// How often the Helper looks for Cohelm pages it hasn't seen opening.
const LOOK_EVERY: Duration = Duration::from_secs(1);
/// How many times the Helper tries to reach a Cohelm page, about once a
/// second, before it gives up on it.
const MAX_TRIES: u32 = 10;

fn page_prefix() -> String {
    format!("chrome-extension://{EXTENSION_ID}/")
}

/// Keep Cohelm's pages up to date and act on what the person does there,
/// until `cancel`.
pub(crate) async fn run(
    browser: Asker,
    control: Control,
    mcp_url: String,
    cancel: CancellationToken,
) {
    let mut pages = Pages {
        browser: browser.clone(),
        control: control.clone(),
        mcp_url,
        attached: HashMap::new(),
        prompts: HashMap::new(),
        failed: HashMap::new(),
        badged: false,
    };
    let mut events = browser.watch();
    let mut changed = control.agents.watch();
    let mut update = control.update.subscribe();
    let mut paused = control.pauses.watch();
    let mut look = tokio::time::interval(LOOK_EVERY);
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            _ = look.tick() => pages.look().await,
            seen = changed.changed() => {
                if seen.is_err() {
                    return;
                }
                pages.agents_changed().await;
            }
            seen = paused.changed() => {
                if seen.is_err() {
                    return;
                }
                pages.pauses_changed().await;
            }
            seen = update.changed() => {
                if seen.is_err() {
                    return;
                }
                pages.badge().await;
                pages.push_all().await;
            }
            event = events.recv() => match event {
                Ok(event) => pages.on_event(event).await,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => pages.look().await,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            },
        }
    }
}

struct Pages {
    browser: Asker,
    control: Control,
    mcp_url: String,
    /// Each Cohelm page the Helper is attached to, by target, with its
    /// session.
    attached: HashMap<String, String>,
    /// Prompt windows the Helper opened, by target, with what each asks
    /// about. Only these may answer it.
    prompts: HashMap<String, Prompt>,
    /// Targets the Helper couldn't attach to, with how many times it tried.
    failed: HashMap<String, u32>,
    /// Whether Cohelm's toolbar button shows that an update is out.
    badged: bool,
}

/// What a prompt window asks the person.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Prompt {
    /// Whether to allow this new Agent.
    Agent(String),
    /// Whether to allow this risky step.
    Pause(u64),
}

impl Pages {
    /// Find Cohelm pages that opened without the Helper hearing of it.
    async fn look(&mut self) {
        let Ok(targets) = self.browser.ask("Target.getTargets", json!({})).await else {
            return;
        };
        let all: Vec<&Value> = targets["targetInfos"]
            .as_array()
            .into_iter()
            .flatten()
            .collect();
        // Gone means gone from the browser: a new prompt may not show its
        // address yet.
        let ids: HashSet<&str> = all.iter().filter_map(|t| t["targetId"].as_str()).collect();
        let open: Vec<String> = all
            .iter()
            .filter(|t| cohelm_page(t).is_some())
            .filter_map(|t| Some(t["targetId"].as_str()?.to_owned()))
            .collect();
        let gone: Vec<String> = self
            .attached
            .keys()
            .chain(self.prompts.keys())
            .filter(|id| !ids.contains(id.as_str()))
            .cloned()
            .collect();
        for target in gone {
            self.closed(&target);
        }
        for target in open {
            self.found(&target).await;
        }
    }

    async fn on_event(&mut self, event: Value) {
        let method = event["method"].as_str().unwrap_or_default();
        match method {
            "Target.targetCreated" | "Target.targetInfoChanged" => {
                let info = &event["params"]["targetInfo"];
                if let (Some(target), Some(_)) = (info["targetId"].as_str(), cohelm_page(info)) {
                    // Try again: a page that just loaded may answer now.
                    self.failed.remove(target);
                    self.found(target).await;
                } else if method == "Target.targetCreated" && background(info) {
                    self.badge().await;
                }
            }
            "Target.targetDestroyed" => {
                if let Some(target) = event["params"]["targetId"].as_str() {
                    self.closed(target);
                }
            }
            "Target.detachedFromTarget" => {
                if let Some(session) = event["params"]["sessionId"].as_str() {
                    self.attached.retain(|_, s| s != session);
                }
            }
            "Runtime.bindingCalled" if event["params"]["name"] == BINDING => {
                let session = event["sessionId"].as_str().unwrap_or_default();
                let target = self
                    .attached
                    .iter()
                    .find(|(_, s)| *s == session)
                    .map(|(t, _)| t.clone());
                let said = event["params"]["payload"]
                    .as_str()
                    .and_then(|p| serde_json::from_str::<Value>(p).ok());
                if let (Some(target), Some(said)) = (target, said) {
                    self.on_answer(&target, &said).await;
                }
            }
            _ => {}
        }
    }

    /// A Cohelm page is open: attach to it and give it the lists.
    async fn found(&mut self, target: &str) {
        let tries = self.failed.get(target).copied().unwrap_or(0);
        if self.attached.contains_key(target) || tries >= MAX_TRIES {
            return;
        }
        match self.attach(target).await {
            Ok(session) => {
                self.attached.insert(target.to_owned(), session);
                self.push(target).await;
            }
            Err(e) => {
                if tries + 1 == MAX_TRIES {
                    tracing::warn!("couldn't reach one of Cohelm's pages: {e}");
                }
                self.failed.insert(target.to_owned(), tries + 1);
            }
        }
    }

    async fn attach(&self, target: &str) -> Result<String, HelperError> {
        let session = self.browser.attach(target).await?;
        let ready = async {
            self.browser
                .ask_in(&session, "Runtime.enable", json!({}))
                .await?;
            self.browser
                .ask_in(&session, "Runtime.addBinding", json!({ "name": BINDING }))
                .await
        };
        if let Err(e) = ready.await {
            let _ = self
                .browser
                .ask("Target.detachFromTarget", json!({ "sessionId": session }))
                .await;
            return Err(e);
        }
        Ok(session)
    }

    fn closed(&mut self, target: &str) {
        self.attached.remove(target);
        self.failed.remove(target);
        // Closing the prompt without answering declines, for now: the Agent
        // is refused until Cohelm restarts or the person allows it from the
        // Agents list.
        match self.prompts.remove(target) {
            Some(Prompt::Agent(name)) if self.control.agents.snapshot().asking.contains(&name) => {
                self.control.agents.answer(&name, false);
            }
            // Closing a Pause without answering blocks the step.
            Some(Prompt::Pause(id)) => self.control.pauses.answer(id, false),
            _ => {}
        }
    }

    async fn agents_changed(&mut self) {
        let asking = self.control.agents.snapshot().asking;
        // Close prompts that have been answered some other way.
        let answered: Vec<String> = self
            .prompts
            .iter()
            .filter(|(_, p)| matches!(p, Prompt::Agent(name) if !asking.contains(name)))
            .map(|(target, _)| target.clone())
            .collect();
        self.close_prompts(answered).await;
        for name in asking {
            let prompt = Prompt::Agent(name);
            if !self.prompts.values().any(|p| *p == prompt) {
                self.open_prompt(prompt).await;
            }
        }
        self.push_all().await;
    }

    async fn push_all(&self) {
        for target in self.attached.keys() {
            self.push(target).await;
        }
    }

    /// Mark Cohelm's toolbar button while an update is out, so the person
    /// sees it without opening the Agents list. The button is set from the
    /// extension's background worker, when it's running.
    async fn badge(&mut self) {
        let out = self.control.update.borrow().is_some();
        if out == self.badged {
            return;
        }
        let Ok(targets) = self.browser.ask("Target.getTargets", json!({})).await else {
            return;
        };
        let Some(worker) = targets["targetInfos"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| background(t))
            .and_then(|t| t["targetId"].as_str())
        else {
            return;
        };
        let Ok(session) = self.browser.attach(worker).await else {
            return;
        };
        let text = if out { "1" } else { "" };
        let set = self
            .browser
            .ask_in(
                &session,
                "Runtime.evaluate",
                json!({
                    "expression": format!(
                        "chrome.action.setBadgeText({{ text: {text:?} }}).then(() => true)"
                    ),
                    "awaitPromise": true,
                    "returnByValue": true,
                }),
            )
            .await;
        let _ = self
            .browser
            .ask("Target.detachFromTarget", json!({ "sessionId": session }))
            .await;
        if set.is_ok_and(|r| r["result"]["value"] == true) {
            self.badged = out;
        }
    }

    async fn pauses_changed(&mut self) {
        let waiting: HashSet<u64> = self.control.pauses.waiting().iter().map(|w| w.id).collect();
        let answered: Vec<String> = self
            .prompts
            .iter()
            .filter(|(_, p)| matches!(p, Prompt::Pause(id) if !waiting.contains(id)))
            .map(|(target, _)| target.clone())
            .collect();
        self.close_prompts(answered).await;
        for id in waiting {
            let prompt = Prompt::Pause(id);
            if !self.prompts.values().any(|p| *p == prompt) {
                self.open_prompt(prompt).await;
            }
        }
    }

    async fn close_prompts(&mut self, targets: Vec<String>) {
        for target in targets {
            self.prompts.remove(&target);
            let _ = self
                .browser
                .ask("Target.closeTarget", json!({ "targetId": target }))
                .await;
        }
    }

    async fn open_prompt(&mut self, prompt: Prompt) {
        let page = match prompt {
            Prompt::Agent(_) => PROMPT,
            Prompt::Pause(_) => PAUSE,
        };
        let opened = self
            .browser
            .ask(
                "Target.createTarget",
                json!({
                    "url": format!("{}{page}", page_prefix()),
                    "newWindow": true,
                    "width": 440,
                    "height": 400,
                }),
            )
            .await;
        match opened.map(|o| o["targetId"].as_str().map(str::to_owned)) {
            Ok(Some(target)) => {
                self.prompts.insert(target.clone(), prompt);
                self.found(&target).await;
            }
            Ok(None) | Err(_) => {
                tracing::warn!("couldn't open Cohelm's prompt: {prompt:?}");
                // Nobody can answer a Pause that couldn't be shown.
                if let Prompt::Pause(id) = prompt {
                    self.control.pauses.answer(id, false);
                }
            }
        }
    }

    async fn on_answer(&mut self, target: &str, said: &Value) {
        let agents = &self.control.agents;
        if said["hello"] == true {
            self.push(target).await;
        } else if let Some(allow) = said["allow"].as_bool() {
            // A prompt answers for the one thing it asks about.
            if let Some(prompt) = self.prompts.remove(target) {
                match prompt {
                    Prompt::Agent(name) => agents.answer(&name, allow),
                    Prompt::Pause(id) => self.control.pauses.answer(id, allow),
                }
                let _ = self
                    .browser
                    .ask("Target.closeTarget", json!({ "targetId": target }))
                    .await;
            }
        } else if let Some(name) = said["allowAgain"].as_str() {
            if agents.snapshot().declined.iter().any(|n| n == name) {
                agents.answer(name, true);
            }
        } else if let Some(name) = said["disconnect"].as_str() {
            self.control.disconnect(name).await;
        }
    }

    /// Give a Cohelm page the newest lists.
    async fn push(&self, target: &str) {
        let Some(session) = self.attached.get(target) else {
            return;
        };
        let update = self.control.update.borrow().as_ref().map(Update::to_json);
        let (asking, pause) = match self.prompts.get(target) {
            Some(Prompt::Agent(name)) => (Some(name.clone()), None),
            Some(Prompt::Pause(id)) => (
                None,
                self.control
                    .pauses
                    .waiting()
                    .into_iter()
                    .find(|w| w.id == *id)
                    .map(|w| w.to_json()),
            ),
            None => (None, None),
        };
        let state = json!({
            "asking": asking,
            "pause": pause,
            "update": update,
            "version": env!("CARGO_PKG_VERSION"),
            "agents": self.control.agents.snapshot().to_json(),
            "connect": connection_details(&self.mcp_url),
        });
        let _ = self
            .browser
            .ask_in(
                session,
                "Runtime.evaluate",
                json!({
                    "expression": format!(
                        "globalThis.cohelmReceive && globalThis.cohelmReceive({state})"
                    ),
                }),
            )
            .await;
    }
}

/// The address and how to add it to common MCP clients, for the person to
/// copy.
pub(crate) fn connection_details(mcp_url: &str) -> Value {
    json!({
        "url": mcp_url,
        "claudeCode": format!("claude mcp add --transport http cohelm {mcp_url}"),
        "json": json!({ "mcpServers": { "cohelm": { "type": "http", "url": mcp_url } } })
            .to_string(),
    })
}

/// Whether the target is anything of Cohelm's extension, its background
/// worker included. Agents never attach to these.
pub(crate) fn cohelm_target(info: &Value) -> bool {
    info["url"]
        .as_str()
        .is_some_and(|url| url.starts_with(&page_prefix()))
}

/// Whether the target is Cohelm's background worker.
fn background(info: &Value) -> bool {
    info["type"] == "service_worker" && cohelm_target(info)
}

/// The page's address, if it's one of Cohelm's pages a person sees. The
/// extension's service worker isn't.
pub(crate) fn cohelm_page(info: &Value) -> Option<String> {
    let url = info["url"].as_str()?;
    (info["type"] != "service_worker" && url.starts_with(&page_prefix())).then(|| url.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_cohelms_pages_count() {
        let page = |kind: &str, url: &str| json!({ "type": kind, "url": url });
        let ours = format!("{}agents.html", page_prefix());
        assert!(cohelm_page(&page("page", &ours)).is_some());
        assert!(cohelm_page(&page("other", &ours)).is_some());
        assert!(
            cohelm_page(&page(
                "service_worker",
                &format!("{}background.js", page_prefix())
            ))
            .is_none()
        );
        assert!(cohelm_page(&page("page", "chrome-extension://another/agents.html")).is_none());
        assert!(cohelm_page(&page("page", "https://example.com")).is_none());
    }

    #[test]
    fn connection_details_name_the_address() {
        let details = connection_details("http://127.0.0.1:47600/mcp");
        assert_eq!(
            details["claudeCode"],
            "claude mcp add --transport http cohelm http://127.0.0.1:47600/mcp"
        );
        let config: Value = serde_json::from_str(details["json"].as_str().unwrap()).unwrap();
        assert_eq!(
            config["mcpServers"]["cohelm"]["url"],
            "http://127.0.0.1:47600/mcp"
        );
    }
}
