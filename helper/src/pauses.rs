//! The Pause: an Agent's step that sends, pays, submits or deletes waits for
//! the person to allow or block it.
//!
//! Every tool call is checked before it reaches the page. Tools name
//! elements only by uid, so for a step that touches an element the Helper
//! takes its own snapshot of that page first and reads the element there,
//! as structured data rather than text a page could forge. Chrome DevTools
//! MCP then finds the uid in that same snapshot, so the step acts on the
//! element the person is asked about. A step Cohelm can't read, such as a
//! script or a click at a spot, counts as risky: when in doubt, the person
//! decides.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use rmcp::model::CallToolRequestParams;
use serde_json::{Map, Value, json};
use tokio::sync::{oneshot, watch};

/// How long a Pause waits for the person before it counts as blocked. The
/// one place this is set.
pub const PAUSE_WAIT: Duration = Duration::from_secs(120);

/// What a risky step may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Send,
    Pay,
    Submit,
    Delete,
}

impl Kind {
    fn may(self) -> &'static str {
        match self {
            Kind::Send => "may send something",
            Kind::Pay => "may pay or buy something",
            Kind::Submit => "may submit a form",
            Kind::Delete => "may delete something",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Send => "send",
            Kind::Pay => "pay",
            Kind::Submit => "submit",
            Kind::Delete => "delete",
        }
    }
}

/// A step that waits for the person, in plain words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub kind: Kind,
    /// What the Agent will do, such as `click “Place order”`.
    pub action: String,
    /// The site it happens on, if Cohelm knows it.
    pub site: Option<String>,
    /// What the Agent sends with it, such as the script or the text, kept
    /// short, so the person isn't deciding blind.
    pub detail: Option<String>,
}

impl Step {
    fn new(kind: Kind, action: impl Into<String>) -> Self {
        Self {
            kind,
            action: action.into(),
            site: None,
            detail: None,
        }
    }

    fn with_detail(mut self, detail: &str) -> Self {
        self.detail = Some(shortened(detail, 200));
        self
    }

    /// The whole step as the person reads it, without the Agent's name.
    pub fn describe(&self) -> String {
        match &self.site {
            Some(site) => format!("{} on {site}, which {}", self.action, self.kind.may()),
            None => format!("{}, which {}", self.action, self.kind.may()),
        }
    }
}

/// Words on a control that mark what pressing it does, checked in order,
/// so "Delete and send" counts as a delete. A word ending in `*` matches
/// any word it starts ("payment" for `payment*`), and `…` stands for up to
/// two other words ("Place your order" for `place … order*`).
const WORDS: &[(Kind, &[&str])] = &[
    (
        Kind::Delete,
        &[
            "delet*",
            "remov*",
            "discard*",
            "trash",
            "eras*",
            "destroy*",
            "unsubscrib*",
            "close … account",
            "deactivat*",
            "cancel … subscription*",
            "cancel … order*",
            "cancel … booking*",
            "cancel … membership*",
            "cancel … plan",
        ],
    ),
    (
        Kind::Pay,
        &[
            "pay",
            "payment*",
            "paypal",
            "buy*",
            "purchas*",
            "checkout",
            "check out",
            "place … order*",
            "order now",
            "complete … order*",
            "confirm … order*",
            "submit … order*",
            "donat*",
            "transfer*",
            "withdraw*",
            "sell",
            "bid",
            "add … funds",
        ],
    ),
    (
        Kind::Send,
        &[
            "send", "reply", "forward", "post", "publish*", "share", "invite*", "tweet", "comment",
            "upload*",
        ],
    ),
    (
        Kind::Submit,
        &[
            "submit*",
            "confirm*",
            "apply",
            "sign up",
            "register",
            "subscribe",
            "book",
            "reserve",
            "accept*",
            "agree*",
            "approv*",
            "authoriz*",
            "authoris*",
            "allow",
            "grant",
        ],
    ),
];

/// Tools that only look, wait or move between tabs, and never act for the
/// person. A tool not named here or in [`classify`] pauses, so a new tool
/// in Chrome DevTools MCP is never let through unseen.
const LOOKING: &[&str] = &[
    "list_pages",
    "select_page",
    "close_page",
    "resize_page",
    "take_snapshot",
    "take_screenshot",
    "wait_for",
    "hover",
    "emulate",
    "get_tab_id",
    "list_console_messages",
    "get_console_message",
    "list_network_requests",
    "get_network_request",
    "get_css_styles",
    "performance_start_trace",
    "performance_stop_trace",
    "performance_analyze_insight",
    "lighthouse_audit",
    "list_webmcp_tools",
    "list_extensions",
    "take_heapsnapshot",
    "close_heapsnapshot",
    "compare_heapsnapshots",
    "analyze_heapsnapshot_contexts",
    "query_heapsnapshot_objects",
    "get_heapsnapshot_summary",
    "get_heapsnapshot_details",
    "get_heapsnapshot_class_nodes",
    "get_heapsnapshot_dominators",
    "get_heapsnapshot_duplicate_strings",
    "get_heapsnapshot_edges",
    "get_heapsnapshot_object_details",
    "get_heapsnapshot_retainers",
    "get_heapsnapshot_retaining_paths",
];

/// Every name Chrome DevTools MCP takes for Enter, which sends a form.
const ENTER: &[&str] = &["enter", "numpadenter", "\n", "\r"];
/// Space, which presses a focused button.
const SPACE: &[&str] = &["space", " "];

/// Roles that Space presses when they have focus.
const PRESSABLE: &[&str] = &[
    "button",
    "link",
    "checkbox",
    "radio",
    "switch",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "tab",
    "treeitem",
];

/// Roles that `fill` ticks or turns on rather than types into.
const TICKABLE: &[&str] = &["checkbox", "radio", "switch", "menuitemcheckbox"];

/// What the Helper must look at before it can check a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// Nothing: the step is safe whatever the page holds.
    Nothing,
    /// The page's address, to tell the person where the step happens.
    Site,
    /// A fresh snapshot of the page, for the elements the step touches.
    Snapshot,
}

/// A page as the Helper read it for one step.
#[derive(Debug, Default, Clone)]
pub struct Page {
    pub url: Option<String>,
    elements: HashMap<String, Element>,
    focused: Option<String>,
}

#[derive(Debug, Clone)]
struct Element {
    role: String,
    name: String,
}

impl Page {
    /// Read a snapshot that Chrome DevTools MCP gave as structured data:
    /// each node with its `id`, `role`, `name` and other properties, and
    /// the page itself at the root with its `url`.
    pub fn from_snapshot(root: &Value) -> Self {
        let mut page = Page {
            url: root.get("url").and_then(Value::as_str).map(str::to_owned),
            ..Page::default()
        };
        let mut nodes = vec![root];
        while let Some(node) = nodes.pop() {
            if let Some(id) = node.get("id").and_then(Value::as_str) {
                let text = |key| node.get(key).and_then(Value::as_str).unwrap_or_default();
                page.elements.insert(
                    id.to_owned(),
                    Element {
                        role: text("role").to_owned(),
                        name: text("name").to_owned(),
                    },
                );
                if node.get("focused") == Some(&Value::Bool(true)) {
                    page.focused = Some(id.to_owned());
                }
            }
            if let Some(children) = node.get("children").and_then(Value::as_array) {
                nodes.extend(children);
            }
        }
        page
    }

    /// Read the address of page `id`, or of the selected page, from the
    /// structured pages list.
    pub fn from_pages(pages: &Value, id: Option<i64>) -> Self {
        let url = pages.as_array().and_then(|pages| {
            pages
                .iter()
                .find(|p| match id {
                    Some(id) => p.get("id").and_then(Value::as_i64) == Some(id),
                    None => p.get("selected") == Some(&Value::Bool(true)),
                })
                .and_then(|p| p.get("url"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        Page {
            url,
            ..Page::default()
        }
    }

    fn site(&self) -> Option<String> {
        self.url.as_deref().and_then(site)
    }
}

/// The site a person knows a page by: its host.
fn site(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', '?', '#', '\\']).next()?;
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    (!host.is_empty()).then(|| host.to_owned())
}

/// What pressing a control called `label` does, if it's risky.
fn kind_of(label: &str) -> Option<Kind> {
    let label = label.to_lowercase();
    let words: Vec<&str> = label
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    WORDS
        .iter()
        .find(|(_, marks)| marks.iter().any(|mark| has(&words, mark)))
        .map(|(kind, _)| *kind)
        .or_else(|| signs(&words).then_some(Kind::Submit))
}

/// Whether `words` hold `mark`, read as [`WORDS`] describes.
fn has(words: &[&str], mark: &str) -> bool {
    let mark: Vec<&str> = mark.split(' ').collect();
    (0..words.len()).any(|start| matches_at(&words[start..], &mark))
}

fn matches_at(words: &[&str], mark: &[&str]) -> bool {
    let Some((first, rest)) = mark.split_first() else {
        return true;
    };
    if *first == "…" {
        return (0..=2.min(words.len())).any(|skip| matches_at(&words[skip..], rest));
    }
    let Some((word, after)) = words.split_first() else {
        return false;
    };
    let fits = match first.strip_suffix('*') {
        Some(stem) => word.starts_with(stem),
        None => word == first,
    };
    fits && matches_at(after, rest)
}

/// "Sign" as in signing a document, not signing in, out or up.
fn signs(words: &[&str]) -> bool {
    words.iter().enumerate().any(|(i, w)| {
        *w == "sign" && !matches!(words.get(i + 1), Some(&("in" | "out" | "up" | "off")))
    })
}

/// Words from a page or an Agent, kept short and on one line.
fn shortened(text: &str, most: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut short: String = text.chars().take(most).collect();
    if short.len() < text.len() {
        short.push('…');
    }
    short
}

/// Quote a page's words for the person.
fn quoted(text: &str) -> String {
    format!("“{}”", shortened(text, 60))
}

/// Whether `key`, such as `Control+Enter`, presses one of `keys`.
fn presses(key: &str, keys: &[&str]) -> bool {
    let parts: Vec<&str> = if key == "+" {
        vec![key]
    } else {
        key.split('+').collect()
    };
    parts
        .iter()
        .any(|part| keys.contains(part) || keys.contains(&part.to_lowercase().as_str()))
}

fn has_newline(text: &str) -> bool {
    text.contains(['\n', '\r'])
}

/// An address that runs a script where it opens, read the way Chromium
/// reads addresses: tabs and newlines dropped, spaces trimmed.
fn script_address(url: &str) -> bool {
    let read: String = url
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let url = read.trim_matches(|c: char| c <= ' ').to_ascii_lowercase();
    ["javascript:", "data:", "vbscript:"]
        .iter()
        .any(|scheme| url.starts_with(scheme))
}

/// The arguments of a tool call.
fn args(request: &CallToolRequestParams) -> &Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    request
        .arguments
        .as_ref()
        .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}

/// What the Helper must look at before [`classify`] can check this step.
pub fn need(request: &CallToolRequestParams) -> Need {
    let args = args(request);
    let text = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or_default();
    match request.name.as_ref() {
        "click" | "fill" | "fill_form" | "drag" => Need::Snapshot,
        "type_text" => {
            let key = text("submitKey");
            if has_newline(text("text")) || presses(key, ENTER) {
                Need::Site
            } else if text("text").contains(' ') || presses(key, SPACE) {
                Need::Snapshot
            } else {
                Need::Nothing
            }
        }
        "press_key" => {
            if presses(text("key"), ENTER) {
                Need::Site
            } else if presses(text("key"), SPACE) {
                Need::Snapshot
            } else {
                Need::Nothing
            }
        }
        "handle_dialog" if text("action") != "accept" => Need::Nothing,
        "navigate_page" | "new_page"
            if !script_address(text("url"))
                && args.get("initScript").is_none_or(Value::is_null) =>
        {
            Need::Nothing
        }
        tool if LOOKING.contains(&tool) => Need::Nothing,
        _ => Need::Site,
    }
}

/// What to do with a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Pass it to the page.
    Go,
    /// Wait for the person.
    Pause(Step),
    /// Turn it away, with what to tell the Agent.
    Refuse(String),
}

/// Whether this step waits for the person, read against `page`, which the
/// Helper fetched as [`need`] asked.
pub fn classify(request: &CallToolRequestParams, page: &Page) -> Verdict {
    let mut verdict = check(request, page);
    if let Verdict::Pause(step) = &mut verdict {
        step.site = page.site();
    }
    verdict
}

fn gone(uid: &str) -> Verdict {
    Verdict::Refuse(format!(
        "Element uid \"{uid}\" isn't on the page any more. Take a new snapshot and try again."
    ))
}

fn check(request: &CallToolRequestParams, page: &Page) -> Verdict {
    let tool = request.name.as_ref();
    let args = args(request);
    let text = |key: &str| args.get(key).and_then(Value::as_str);
    let element = |key: &str| -> Result<&Element, Verdict> {
        let Some(uid) = text(key) else {
            return Err(Verdict::Refuse(format!(
                "{tool} needs an element's uid in {key}."
            )));
        };
        page.elements.get(uid).ok_or_else(|| gone(uid))
    };
    match tool {
        "click" => match element("uid") {
            Ok(element) => pressing(element, "click"),
            Err(refusal) => refusal,
        },
        "fill" => match element("uid") {
            Ok(element) => filling(element, text("value").unwrap_or_default()),
            Err(refusal) => refusal,
        },
        "fill_form" => {
            let Some(fields) = args.get("elements").and_then(Value::as_array) else {
                return Verdict::Refuse("fill_form needs a list of elements.".into());
            };
            for field in fields {
                let value = field
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let verdict = match field.get("uid").and_then(Value::as_str) {
                    Some(uid) => match page.elements.get(uid) {
                        Some(element) => filling(element, value),
                        None => gone(uid),
                    },
                    None => Verdict::Refuse("Each element in fill_form needs a uid.".into()),
                };
                if verdict != Verdict::Go {
                    return verdict;
                }
            }
            Verdict::Go
        }
        "drag" => match (element("from_uid"), element("to_uid")) {
            (Ok(from), Ok(to)) => match kind_of(&from.name).or_else(|| kind_of(&to.name)) {
                Some(kind) => Verdict::Pause(Step::new(
                    kind,
                    format!("drag {} onto {}", label(from), label(to)),
                )),
                None => Verdict::Go,
            },
            (Err(refusal), _) | (_, Err(refusal)) => refusal,
        },
        "type_text" => {
            let typed = text("text").unwrap_or_default();
            let key = text("submitKey").unwrap_or_default();
            if has_newline(typed) || presses(key, ENTER) {
                Verdict::Pause(Step::new(Kind::Submit, "type and press Enter").with_detail(typed))
            } else if typed.contains(' ') || presses(key, SPACE) {
                focused(page, "type a space on")
            } else {
                Verdict::Go
            }
        }
        "press_key" => {
            let key = text("key").unwrap_or_default();
            if presses(key, ENTER) {
                Verdict::Pause(Step::new(Kind::Submit, "press Enter"))
            } else if presses(key, SPACE) {
                focused(page, "press Space on")
            } else {
                Verdict::Go
            }
        }
        "upload_file" => {
            let count = args
                .get("filePaths")
                .and_then(Value::as_array)
                .map_or(1, Vec::len);
            let files = if count == 1 {
                "a file".to_owned()
            } else {
                format!("{count} files")
            };
            Verdict::Pause(Step::new(Kind::Send, format!("upload {files}")))
        }
        "handle_dialog" => match text("action") {
            Some("accept") => {
                let step = Step::new(Kind::Submit, "accept the page's dialog");
                Verdict::Pause(match text("promptText") {
                    Some(typed) => step.with_detail(typed),
                    None => step,
                })
            }
            _ => Verdict::Go,
        },
        "evaluate_script" => Verdict::Pause(
            Step::new(Kind::Submit, "run a script on the page")
                .with_detail(text("function").unwrap_or_default()),
        ),
        "click_at" => Verdict::Pause(Step::new(
            Kind::Submit,
            "click a spot on the page Cohelm can't name",
        )),
        "navigate_page" | "new_page" => {
            let url = text("url").unwrap_or_default();
            if let Some(script) = text("initScript").filter(|s| !s.is_empty()) {
                Verdict::Pause(
                    Step::new(Kind::Submit, "run a script when the page loads").with_detail(script),
                )
            } else if args
                .get("initScript")
                .is_some_and(|v| !v.is_null() && !v.is_string())
            {
                Verdict::Refuse("initScript must be text.".into())
            } else if script_address(url) {
                Verdict::Pause(
                    Step::new(Kind::Submit, "open an address that runs a script").with_detail(url),
                )
            } else {
                Verdict::Go
            }
        }
        _ if LOOKING.contains(&tool) => Verdict::Go,
        _ => Verdict::Pause(
            Step::new(Kind::Submit, format!("use the {tool} tool"))
                .with_detail(&Value::Object(args.clone()).to_string()),
        ),
    }
}

fn label(element: &Element) -> String {
    if element.name.is_empty() {
        let role = if element.role.is_empty() {
            "control"
        } else {
            &element.role
        };
        format!("a {role} with no label")
    } else {
        quoted(&element.name)
    }
}

/// Clicking `element`, or pressing it some other way.
fn pressing(element: &Element, verb: &str) -> Verdict {
    let kind = kind_of(&element.name).or_else(|| {
        // A button with no words, in a form, may submit it.
        (element.name.is_empty() && element.role == "button").then_some(Kind::Submit)
    });
    match kind {
        Some(kind) => Verdict::Pause(Step::new(kind, format!("{verb} {}", label(element)))),
        None => Verdict::Go,
    }
}

/// Filling `element` with `value`: a new line presses Enter, and a box such
/// as "I agree" is ticked rather than typed into.
fn filling(element: &Element, value: &str) -> Verdict {
    if has_newline(value) {
        return Verdict::Pause(
            Step::new(
                Kind::Submit,
                format!("type into {} and press Enter", label(element)),
            )
            .with_detail(value),
        );
    }
    if TICKABLE.contains(&element.role.as_str()) {
        return pressing(element, "tick");
    }
    Verdict::Go
}

/// A key that presses whatever has focus.
fn focused(page: &Page, verb: &str) -> Verdict {
    let element = page.focused.as_ref().and_then(|id| page.elements.get(id));
    match element {
        Some(element) if PRESSABLE.contains(&element.role.as_str()) => pressing(element, verb),
        _ => Verdict::Go,
    }
}

/// How a Pause ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Allowed,
    /// The person blocked it, or disconnected the Agent.
    Blocked,
    /// Nobody answered in time.
    TimedOut,
    /// Nothing in Cohelm can ask the person.
    CantAsk,
    /// This Agent already has a step waiting, and the person answers one
    /// at a time.
    Busy,
}

/// A Pause waiting for the person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiting {
    pub id: u64,
    pub agent: String,
    pub step: Step,
}

impl Waiting {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "agent": self.agent,
            "kind": self.step.kind.name(),
            "action": self.step.action,
            "site": self.step.site,
            "detail": self.step.detail,
            "says": format!("{} wants to {}.", self.agent, self.step.describe()),
        })
    }
}

/// The Pauses waiting for the person, shared by every Agent's calls.
pub struct Pauses {
    next: AtomicU64,
    waiting: Mutex<BTreeMap<u64, (Waiting, oneshot::Sender<bool>)>>,
    changed: watch::Sender<u64>,
}

impl Default for Pauses {
    fn default() -> Self {
        Self {
            next: AtomicU64::new(1),
            waiting: Mutex::new(BTreeMap::new()),
            changed: watch::Sender::new(0),
        }
    }
}

impl Pauses {
    /// Hold `step` until the person answers or `wait` runs out. None means
    /// nothing in Cohelm can ask them. Each Agent has at most one step
    /// waiting, so one Agent can't bury the person in prompts.
    pub async fn hold(&self, agent: &str, step: Step, wait: Option<Duration>) -> Answer {
        let Some(wait) = wait else {
            return Answer::CantAsk;
        };
        let (tx, rx) = oneshot::channel();
        let id = {
            let mut waiting = self.lock();
            if waiting.values().any(|(w, _)| w.agent == agent) {
                return Answer::Busy;
            }
            let id = self.next.fetch_add(1, Ordering::Relaxed);
            let entry = Waiting {
                id,
                agent: agent.to_owned(),
                step,
            };
            waiting.insert(id, (entry, tx));
            id
        };
        self.bump();
        // Taken out however the wait ends, even if the Agent gives up.
        let _done = Done { pauses: self, id };
        match tokio::time::timeout(wait, rx).await {
            Ok(Ok(true)) => Answer::Allowed,
            Ok(Ok(false)) | Ok(Err(_)) => Answer::Blocked,
            Err(_) => Answer::TimedOut,
        }
    }

    /// The person's answer to Pause `id`.
    pub fn answer(&self, id: u64, allow: bool) {
        let answered = self.lock().remove(&id);
        if let Some((_, tx)) = answered {
            let _ = tx.send(allow);
            self.bump();
        }
    }

    /// Block every step `agent` has waiting, as when the person disconnects
    /// it.
    pub fn block_agent(&self, agent: &str) {
        let blocked: Vec<_> = {
            let mut waiting = self.lock();
            let ids: Vec<u64> = waiting
                .iter()
                .filter(|(_, (w, _))| w.agent == agent)
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| waiting.remove(&id))
                .collect()
        };
        if !blocked.is_empty() {
            for (_, tx) in blocked {
                let _ = tx.send(false);
            }
            self.bump();
        }
    }

    pub fn waiting(&self) -> Vec<Waiting> {
        self.lock().values().map(|(w, _)| w.clone()).collect()
    }

    pub fn watch(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<u64, (Waiting, oneshot::Sender<bool>)>> {
        self.waiting.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn bump(&self) {
        self.changed.send_modify(|n| *n += 1);
    }
}

struct Done<'a> {
    pauses: &'a Pauses,
    id: u64,
}

impl Drop for Done<'_> {
    fn drop(&mut self) {
        if self.pauses.lock().remove(&self.id).is_some() {
            self.pauses.bump();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(tool: &'static str, args: Value) -> CallToolRequestParams {
        CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone())
    }

    /// A cart page as Chrome DevTools MCP gives it with structured content.
    fn shop() -> Page {
        Page::from_snapshot(&json!({
            "id": "3_0", "role": "RootWebArea", "name": "Your cart",
            "url": "https://shop.example.com/cart",
            "children": [
                { "id": "3_1", "role": "button", "name": "Place your order", "focusable": true },
                { "id": "3_2", "role": "link", "name": "Show more" },
                { "id": "3_3", "role": "button", "name": "Remove item" },
                { "id": "3_4", "role": "textbox", "name": "Search", "focusable": true, "focused": true },
                { "id": "3_5", "role": "button", "name": "Send to \"Sam\"" },
                { "id": "3_6", "role": "button" },
                { "id": "3_7", "role": "link", "name": "Compost tips" },
                { "id": "3_8", "role": "button", "name": "Delete and send" },
                { "id": "3_9", "role": "checkbox", "name": "I agree to the terms" },
                { "id": "3_10", "role": "checkbox", "name": "Remember me" },
                { "id": "3_11", "role": "generic", "name": "Inbox", "children": [
                    { "id": "3_12", "role": "listitem", "name": "A note from Sam" },
                    { "id": "3_13", "role": "link", "name": "Trash" },
                ]},
            ],
        }))
    }

    fn with_focus(uid: &str) -> Page {
        let mut page = shop();
        page.focused = Some(uid.to_owned());
        page
    }

    fn verdict(tool: &'static str, args: Value) -> Verdict {
        classify(&call(tool, args), &shop())
    }

    fn step(tool: &'static str, args: Value) -> Step {
        match verdict(tool, args.clone()) {
            Verdict::Pause(step) => step,
            other => panic!("{tool} {args}: {other:?}"),
        }
    }

    fn click(uid: &str) -> Verdict {
        verdict("click", json!({ "uid": uid }))
    }

    #[test]
    fn plain_clicks_and_typing_go_ahead() {
        assert_eq!(click("3_2"), Verdict::Go, "a link to show more");
        assert_eq!(click("3_4"), Verdict::Go, "focusing a search box");
        assert_eq!(
            click("3_7"),
            Verdict::Go,
            "a word inside another isn't a match"
        );
        for (tool, args) in [
            ("fill", json!({ "uid": "3_4", "value": "pay rise" })),
            ("fill", json!({ "uid": "3_10", "value": "true" })),
            ("type_text", json!({ "text": "send help" })),
            ("type_text", json!({ "text": "x", "submitKey": "Tab" })),
            ("press_key", json!({ "key": "ArrowDown" })),
            ("press_key", json!({ "key": "Space" })),
            (
                "navigate_page",
                json!({ "url": "https://example.com/delete" }),
            ),
            (
                "navigate_page",
                json!({ "type": "reload", "initScript": null }),
            ),
            ("new_page", json!({ "url": "https://example.com/" })),
            ("take_snapshot", json!({})),
            ("hover", json!({ "uid": "3_1" })),
            ("drag", json!({ "from_uid": "3_12", "to_uid": "3_4" })),
            ("handle_dialog", json!({ "action": "dismiss" })),
            ("list_pages", json!({})),
        ] {
            assert_eq!(verdict(tool, args.clone()), Verdict::Go, "{tool} {args}");
        }
    }

    #[test]
    fn send_pay_submit_and_delete_wait() {
        let pay = step("click", json!({ "uid": "3_1" }));
        assert_eq!(pay.kind, Kind::Pay);
        assert_eq!(
            pay.describe(),
            "click “Place your order” on shop.example.com, which may pay or buy something"
        );
        assert_eq!(step("click", json!({ "uid": "3_3" })).kind, Kind::Delete);
        let send = step("click", json!({ "uid": "3_5" }));
        assert_eq!(send.kind, Kind::Send);
        assert_eq!(send.action, "click “Send to \"Sam\"”");
        assert_eq!(
            step("click", json!({ "uid": "3_6" })).action,
            "click a button with no label"
        );
        assert_eq!(
            step("click", json!({ "uid": "3_8" })).kind,
            Kind::Delete,
            "delete comes first"
        );
        let agree = step("fill", json!({ "uid": "3_9", "value": "true" }));
        assert_eq!(agree.action, "tick “I agree to the terms”");
        let trash = step("drag", json!({ "from_uid": "3_12", "to_uid": "3_13" }));
        assert_eq!(
            (trash.kind, trash.action.as_str()),
            (Kind::Delete, "drag “A note from Sam” onto “Trash”")
        );
        assert_eq!(
            step("upload_file", json!({ "uid": "3_4", "filePaths": ["/a"] })).kind,
            Kind::Send
        );
        assert_eq!(
            step("handle_dialog", json!({ "action": "accept" })).kind,
            Kind::Submit
        );
    }

    #[test]
    fn every_way_of_pressing_enter_waits() {
        for (tool, args) in [
            ("type_text", json!({ "text": "hi", "submitKey": "Enter" })),
            ("type_text", json!({ "text": "hi", "submitKey": "\r" })),
            ("type_text", json!({ "text": "hi\n" })),
            ("type_text", json!({ "text": "a\rb" })),
            ("press_key", json!({ "key": "Control+Enter" })),
            ("press_key", json!({ "key": "NumpadEnter" })),
            ("press_key", json!({ "key": "\n" })),
            ("press_key", json!({ "key": "enter" })),
            ("fill", json!({ "uid": "3_4", "value": "hi\n" })),
            (
                "fill_form",
                json!({ "elements": [
                    { "uid": "3_4", "value": "fine" },
                    { "uid": "3_4", "value": "then\r" },
                ]}),
            ),
        ] {
            assert_eq!(step(tool, args).kind, Kind::Submit);
        }
        let typed = step("type_text", json!({ "text": "Hello Sam\n" }));
        assert_eq!(typed.detail.as_deref(), Some("Hello Sam"));
    }

    #[test]
    fn space_on_a_focused_control_counts_as_pressing_it() {
        let space = |uid, tool, args| classify(&call(tool, args), &with_focus(uid));
        let send = space("3_5", "press_key", json!({ "key": "Space" }));
        assert!(
            matches!(&send, Verdict::Pause(s) if s.action == "press Space on “Send to \"Sam\"”")
        );
        assert!(matches!(
            space("3_1", "press_key", json!({ "key": " " })),
            Verdict::Pause(_)
        ));
        assert!(matches!(
            space("3_1", "type_text", json!({ "text": "a b" })),
            Verdict::Pause(_)
        ));
        assert_eq!(
            space("3_2", "press_key", json!({ "key": "Space" })),
            Verdict::Go,
            "a harmless link"
        );
        assert_eq!(
            space("3_4", "type_text", json!({ "text": "pay the bill" })),
            Verdict::Go,
            "typing in a text box"
        );
    }

    #[test]
    fn scripts_wait_however_they_arrive() {
        for (tool, args) in [
            ("evaluate_script", json!({ "function": "() => 1" })),
            (
                "navigate_page",
                json!({ "type": "reload", "initScript": "document.forms[0].submit()" }),
            ),
            (
                "navigate_page",
                json!({ "type": "url", "url": "javascript:document.forms[0].submit()" }),
            ),
            ("navigate_page", json!({ "url": " \tJavaScript:alert(1)" })),
            ("new_page", json!({ "url": "java\nscript:alert(1)" })),
            ("new_page", json!({ "url": "data:text/html,<form>" })),
        ] {
            let step = step(tool, args.clone());
            assert!(step.detail.is_some(), "{tool} {args} shows what it runs");
        }
        let script = step(
            "evaluate_script",
            json!({ "function": "() => {\n  send()\n}" }),
        );
        assert_eq!(script.detail.as_deref(), Some("() => { send() }"));
    }

    #[test]
    fn what_cohelm_cant_read_waits_or_is_turned_away() {
        let gone = verdict("click", json!({ "uid": "9_9" }));
        assert!(matches!(&gone, Verdict::Refuse(r) if r.contains("isn't on the page")));
        assert!(matches!(verdict("click", json!({})), Verdict::Refuse(_)));
        assert!(matches!(
            verdict("click", json!({ "uid": 5 })),
            Verdict::Refuse(_)
        ));
        assert!(matches!(
            verdict(
                "fill_form",
                json!({ "elements": [{ "uid": "9_9", "value": "x" }] })
            ),
            Verdict::Refuse(_)
        ));
        step("click_at", json!({ "x": 1, "y": 2 }));
        let tool = step("execute_webmcp_tool", json!({ "name": "checkout" }));
        assert_eq!(tool.action, "use the execute_webmcp_tool tool");
        assert_eq!(tool.detail.as_deref(), Some(r#"{"name":"checkout"}"#));
        step("get_os_app_state", json!({}));
    }

    #[test]
    fn need_matches_classify() {
        // Whatever needs nothing goes ahead on an empty page.
        for (tool, args) in [
            ("type_text", json!({ "text": "word" })),
            ("press_key", json!({ "key": "Tab" })),
            ("navigate_page", json!({ "url": "https://a.example/" })),
            ("handle_dialog", json!({ "action": "dismiss" })),
            ("take_snapshot", json!({})),
        ] {
            let request = call(tool, args);
            assert_eq!(need(&request), Need::Nothing, "{tool}");
            assert_eq!(classify(&request, &Page::default()), Verdict::Go, "{tool}");
        }
        assert_eq!(
            need(&call("click", json!({ "uid": "1_1" }))),
            Need::Snapshot
        );
        assert_eq!(
            need(&call("press_key", json!({ "key": "Space" }))),
            Need::Snapshot
        );
        assert_eq!(
            need(&call("press_key", json!({ "key": "Enter" }))),
            Need::Site
        );
        assert_eq!(need(&call("evaluate_script", json!({}))), Need::Site);
        assert_eq!(need(&call("mock_network", json!({}))), Need::Site);
    }

    #[test]
    fn labels_are_read_as_words() {
        for (label, kind) in [
            ("Send", Some(Kind::Send)),
            ("SEND NOW", Some(Kind::Send)),
            ("Check out", Some(Kind::Pay)),
            ("Pay $20.00", Some(Kind::Pay)),
            ("Make payment", Some(Kind::Pay)),
            ("Proceed to payment", Some(Kind::Pay)),
            ("PayPal", Some(Kind::Pay)),
            ("Place your order", Some(Kind::Pay)),
            ("Buy now", Some(Kind::Pay)),
            ("Withdraw funds", Some(Kind::Pay)),
            ("Sign up for free", Some(Kind::Submit)),
            ("Sign the agreement", Some(Kind::Submit)),
            ("Authorize", Some(Kind::Submit)),
            ("Allow", Some(Kind::Submit)),
            ("Grant access", Some(Kind::Submit)),
            ("Move to trash", Some(Kind::Delete)),
            ("Cancel my subscription", Some(Kind::Delete)),
            ("Subscribe", Some(Kind::Submit)),
            ("Sign in", None),
            ("Next", None),
            ("Good news", None),
            ("Payload viewer", None),
            ("Sender details", None),
            ("Your orders", None),
            ("Menu", None),
        ] {
            assert_eq!(kind_of(label), kind, "{label}");
        }
    }

    #[test]
    fn pages_are_read_from_structured_content() {
        let page = shop();
        assert_eq!(page.site().as_deref(), Some("shop.example.com"));
        assert_eq!(page.focused.as_deref(), Some("3_4"));
        assert_eq!(page.elements["3_13"].name, "Trash", "nested nodes");
        // A name holding what looks like an address is only a name.
        let page = Page::from_snapshot(&json!({
            "id": "1_0", "role": "RootWebArea",
            "name": "x\" url=\"https://bank.example/",
            "url": "https://news.example/",
        }));
        assert_eq!(page.site().as_deref(), Some("news.example"));

        let pages = json!([
            { "id": 1, "url": "https://news.example/", "title": "https://bank.example/", "selected": true },
            { "id": 2, "url": "https://user@bank.example:8443/x", "title": "Bank", "selected": false },
        ]);
        assert_eq!(
            Page::from_pages(&pages, None).site().as_deref(),
            Some("news.example")
        );
        assert_eq!(
            Page::from_pages(&pages, Some(2)).site().as_deref(),
            Some("bank.example:8443")
        );
        assert_eq!(Page::from_pages(&pages, Some(7)).site(), None);
    }

    #[tokio::test]
    async fn a_pause_is_allowed_blocked_or_times_out() {
        let pauses = std::sync::Arc::new(Pauses::default());
        let Verdict::Pause(mut step) = click("3_1") else {
            panic!("a pay button pauses");
        };
        step.site = Some("shop.example.com".into());
        let hold = |agent: &'static str, wait| {
            let pauses = pauses.clone();
            let step = step.clone();
            tokio::spawn(async move { pauses.hold(agent, step, wait).await })
        };
        let mut changed = pauses.watch();
        let ten = Some(Duration::from_secs(10));

        let held = hold("agent", ten);
        changed
            .wait_for(|_| !pauses.waiting().is_empty())
            .await
            .unwrap();
        let waiting = pauses.waiting();
        assert_eq!(
            waiting[0].to_json()["says"],
            "agent wants to click “Place your order” on shop.example.com, which may pay or buy something."
        );
        // One step at a time from each Agent.
        assert_eq!(hold("agent", ten).await.unwrap(), Answer::Busy);
        pauses.answer(waiting[0].id, true);
        assert_eq!(held.await.unwrap(), Answer::Allowed);

        let held = hold("agent", ten);
        changed
            .wait_for(|_| !pauses.waiting().is_empty())
            .await
            .unwrap();
        pauses.answer(pauses.waiting()[0].id, false);
        assert_eq!(held.await.unwrap(), Answer::Blocked);

        assert_eq!(
            hold("agent", Some(Duration::from_millis(50)))
                .await
                .unwrap(),
            Answer::TimedOut
        );
        assert!(pauses.waiting().is_empty(), "a timed-out Pause is gone");
        assert_eq!(hold("agent", None).await.unwrap(), Answer::CantAsk);

        // Disconnecting an Agent blocks what it has waiting, and no one
        // else's.
        let mine = hold("agent", ten);
        let theirs = hold("other", ten);
        changed
            .wait_for(|_| pauses.waiting().len() == 2)
            .await
            .unwrap();
        pauses.block_agent("agent");
        assert_eq!(mine.await.unwrap(), Answer::Blocked);
        assert_eq!(pauses.waiting().len(), 1);
        assert_eq!(pauses.waiting()[0].agent, "other");
        theirs.abort();
        let _ = theirs.await;
        assert!(
            pauses.waiting().is_empty(),
            "a dropped wait takes its Pause away"
        );
    }
}
