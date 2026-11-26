//! Which Agents may drive the person's browser.
//!
//! An MCP client is known by the name it gives itself. The first time a
//! client the person hasn't approved calls a tool, the call is held until the
//! person allows or declines it in Cohelm. Allowed names are kept in a file
//! beside the profile, so they survive a restart; declined ones are refused
//! until Cohelm restarts, so a client that retries doesn't ask again and
//! again. Disconnecting an Agent forgets its approval and ends its sessions.
//!
//! A client's name is its own claim. Approval keeps out clients the person
//! didn't set up; it can't keep out a program on this computer that poses as
//! one, which could read the browser profile anyway (ADR 0007).

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tokio::sync::{oneshot, watch};

/// An MCP session, as the Helper's HTTP server names it.
pub type SessionId = Arc<str>;

/// The longest name kept for an Agent; longer ones are cut.
const MAX_NAME: usize = 64;
/// A client that doesn't keep a session counts as connected for this long
/// after its last call.
const RECENT: Duration = Duration::from_secs(5 * 60);

/// Why an Agent's call was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The person declined it.
    Declined(String),
    /// The person hasn't answered yet.
    Waiting(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Declined(name) => write!(
                f,
                "The person declined to let {name} use their browser. \
                 They can allow it from Cohelm's Agents list."
            ),
            Self::Waiting(name) => write!(
                f,
                "Cohelm is asking the person whether {name} may use their browser. \
                 Ask them to answer in Cohelm, then try again."
            ),
        }
    }
}

/// Who may drive the browser, who is asking and who is connected.
pub struct Agents {
    state: Mutex<State>,
    changed: watch::Sender<u64>,
    file: Option<PathBuf>,
}

#[derive(Default)]
struct State {
    allowed: BTreeSet<String>,
    declined: BTreeSet<String>,
    /// Agents waiting on the person, with the calls held for each.
    asking: BTreeMap<String, Vec<oneshot::Sender<bool>>>,
    /// Agents that have called a tool since they were allowed.
    seen: BTreeMap<String, Seen>,
}

struct Seen {
    sessions: HashSet<SessionId>,
    last: Instant,
}

/// What the person sees: a copy of the lists, newest state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub asking: Vec<String>,
    pub connected: Vec<String>,
    pub allowed: Vec<String>,
    pub declined: Vec<String>,
}

impl Snapshot {
    pub fn to_json(&self) -> Value {
        json!({
            "asking": self.asking,
            "connected": self.connected,
            "allowed": self.allowed,
            "declined": self.declined,
        })
    }
}

impl Agents {
    /// Load the allowed Agents from `file`, if given. A missing or unreadable
    /// file means no Agent is allowed yet.
    pub(crate) fn load(file: Option<PathBuf>) -> Arc<Self> {
        let allowed = file
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .map(|saved| {
                saved["allowed"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(name_of)
                    .collect()
            })
            .unwrap_or_default();
        Arc::new(Self {
            state: Mutex::new(State {
                allowed,
                ..State::default()
            }),
            changed: watch::channel(0).0,
            file,
        })
    }

    /// Let a call from `name` through once the person has allowed it,
    /// waiting up to `wait` for them to answer.
    pub(crate) async fn admit(
        &self,
        name: &str,
        session: Option<SessionId>,
        wait: Duration,
    ) -> Result<(), Refusal> {
        let name = name_of(name);
        let answered = {
            let mut state = self.lock();
            if state.allowed.contains(&name) {
                drop(state);
                self.saw(name, session);
                return Ok(());
            }
            if state.declined.contains(&name) {
                return Err(Refusal::Declined(name));
            }
            let (answer, answered) = oneshot::channel();
            let waiting = state.asking.entry(name.clone()).or_default();
            let new = waiting.is_empty();
            waiting.push(answer);
            drop(state);
            if new {
                tracing::info!("{name} asks to use the browser");
                self.bump();
            }
            answered
        };
        match tokio::time::timeout(wait, answered).await {
            Ok(Ok(true)) => {
                self.saw(name, session);
                Ok(())
            }
            Ok(Ok(false)) => Err(Refusal::Declined(name)),
            // Still waiting: keep asking, without this call.
            Ok(Err(_)) | Err(_) => {
                if let Some(waiting) = self.lock().asking.get_mut(&name) {
                    waiting.retain(|a| !a.is_closed());
                }
                Err(Refusal::Waiting(name))
            }
        }
    }

    /// Count a call from an allowed Agent.
    fn saw(&self, name: String, session: Option<SessionId>) {
        let mut state = self.lock();
        let mut fresh = false;
        let seen = state.seen.entry(name).or_insert_with(|| {
            fresh = true;
            Seen {
                sessions: HashSet::new(),
                last: Instant::now(),
            }
        });
        seen.last = Instant::now();
        if let Some(session) = session {
            fresh |= seen.sessions.insert(session);
        }
        drop(state);
        if fresh {
            self.bump();
        }
    }

    /// The person's answer for `name`. Allowing is remembered across
    /// restarts; declining lasts until Cohelm restarts.
    pub fn answer(&self, name: &str, allow: bool) {
        let name = name_of(name);
        let mut state = self.lock();
        if allow {
            state.declined.remove(&name);
            state.allowed.insert(name.clone());
        } else {
            state.allowed.remove(&name);
            state.declined.insert(name.clone());
            state.seen.remove(&name);
        }
        let waiting = state.asking.remove(&name).unwrap_or_default();
        let allowed = state.allowed.clone();
        drop(state);
        tracing::info!(
            "the person {} {name}",
            if allow { "allowed" } else { "declined" }
        );
        for answer in waiting {
            let _ = answer.send(allow);
        }
        self.save(&allowed);
        self.bump();
    }

    /// Forget `name`'s approval and return its sessions, to be ended. Its
    /// next call asks the person again.
    pub fn disconnect(&self, name: &str) -> Vec<SessionId> {
        let name = name_of(name);
        let mut state = self.lock();
        state.allowed.remove(&name);
        let sessions = state
            .seen
            .remove(&name)
            .map(|s| s.sessions.into_iter().collect())
            .unwrap_or_default();
        let allowed = state.allowed.clone();
        drop(state);
        tracing::info!("the person disconnected {name}");
        self.save(&allowed);
        self.bump();
        sessions
    }

    /// Drop sessions that have ended, and clients without a session that
    /// haven't called for a while.
    pub(crate) fn prune(&self, live: impl Fn(&SessionId) -> bool) {
        let mut changed = false;
        let mut state = self.lock();
        state.seen.retain(|_, seen| {
            let before = seen.sessions.len();
            seen.sessions.retain(|s| live(s));
            changed |= seen.sessions.len() != before;
            let keep = !seen.sessions.is_empty() || seen.last.elapsed() < RECENT;
            changed |= !keep;
            keep
        });
        drop(state);
        if changed {
            self.bump();
        }
    }

    /// Every session of every Agent, to check which are still live.
    pub(crate) fn sessions(&self) -> Vec<SessionId> {
        self.lock()
            .seen
            .values()
            .flat_map(|s| s.sessions.iter().cloned())
            .collect()
    }

    pub fn snapshot(&self) -> Snapshot {
        let state = self.lock();
        Snapshot {
            asking: state.asking.keys().cloned().collect(),
            connected: state.seen.keys().cloned().collect(),
            allowed: state.allowed.iter().cloned().collect(),
            declined: state.declined.iter().cloned().collect(),
        }
    }

    /// Changes whenever the lists do.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn bump(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Write the allowed names, through a new file so a crash never leaves
    /// half of one.
    fn save(&self, allowed: &BTreeSet<String>) {
        let Some(path) = &self.file else {
            return;
        };
        let text = json!({ "allowed": allowed }).to_string();
        let written = path
            .parent()
            .ok_or_else(|| std::io::Error::other("no folder"))
            .and_then(|dir| {
                std::fs::create_dir_all(dir)?;
                let mut file = tempfile::NamedTempFile::new_in(dir)?;
                std::io::Write::write_all(&mut file, text.as_bytes())?;
                file.persist(path).map_err(|e| e.error)?;
                Ok(())
            });
        if let Err(e) = written {
            tracing::warn!(
                "couldn't save the allowed Agents to {}: {e}",
                path.display()
            );
        }
    }
}

/// The name the person sees: trimmed, without control characters, and cut
/// to [`MAX_NAME`] characters.
fn name_of(given: &str) -> String {
    let name: String = given
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_NAME)
        .collect();
    if name.is_empty() {
        "An unnamed client".to_owned()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHORT: Duration = Duration::from_millis(50);

    #[tokio::test]
    async fn a_new_agent_waits_for_the_person_and_is_let_in_once_allowed() {
        let agents = Agents::load(None);
        let held = tokio::spawn({
            let agents = agents.clone();
            async move {
                agents
                    .admit("claude-code", Some("s1".into()), Duration::from_secs(5))
                    .await
            }
        });
        let mut watch = agents.watch();
        while agents.snapshot().asking.is_empty() {
            watch.changed().await.unwrap();
        }
        assert_eq!(agents.snapshot().asking, ["claude-code"]);
        agents.answer("claude-code", true);
        assert_eq!(held.await.unwrap(), Ok(()));
        let now = agents.snapshot();
        assert!(now.asking.is_empty());
        assert_eq!(now.connected, ["claude-code"]);
        assert_eq!(now.allowed, ["claude-code"]);
    }

    #[tokio::test]
    async fn a_declined_agent_is_refused_without_asking_again() {
        let agents = Agents::load(None);
        agents.answer("stranger", false);
        assert_eq!(
            agents.admit("stranger", None, SHORT).await,
            Err(Refusal::Declined("stranger".into()))
        );
        assert!(agents.snapshot().asking.is_empty());
    }

    #[tokio::test]
    async fn an_unanswered_agent_is_told_to_wait_and_still_asks() {
        let agents = Agents::load(None);
        assert_eq!(
            agents.admit("slow", None, SHORT).await,
            Err(Refusal::Waiting("slow".into()))
        );
        assert_eq!(agents.snapshot().asking, ["slow"]);
    }

    #[tokio::test]
    async fn approvals_survive_a_restart_and_disconnecting_forgets_them() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Agents.json");
        let agents = Agents::load(Some(file.clone()));
        agents.answer("claude-code", true);
        agents
            .admit("claude-code", Some("s1".into()), SHORT)
            .await
            .unwrap();

        let restarted = Agents::load(Some(file.clone()));
        assert_eq!(
            restarted.admit("claude-code", None, SHORT).await,
            Ok(()),
            "an allowed Agent isn't asked again"
        );

        let ended = agents.disconnect("claude-code");
        assert_eq!(ended, [SessionId::from("s1")]);
        assert!(agents.snapshot().connected.is_empty());
        let restarted = Agents::load(Some(file));
        assert_eq!(
            restarted.admit("claude-code", None, SHORT).await,
            Err(Refusal::Waiting("claude-code".into())),
            "a disconnected Agent asks again"
        );
    }

    #[tokio::test]
    async fn ended_sessions_drop_off_the_connected_list() {
        let agents = Agents::load(None);
        agents.answer("a", true);
        agents.admit("a", Some("s1".into()), SHORT).await.unwrap();
        agents.prune(|_| true);
        assert_eq!(agents.snapshot().connected, ["a"]);
        agents.prune(|_| false);
        // No session left, but it called just now.
        assert_eq!(agents.snapshot().connected, ["a"]);
        let mut state = agents.lock();
        let seen = state.seen.get_mut("a").unwrap();
        seen.last = Instant::now().checked_sub(RECENT).unwrap_or(seen.last);
        let long_ago = seen.last.elapsed() >= RECENT;
        drop(state);
        if !long_ago {
            return; // The clock hasn't run long enough to go back that far.
        }
        agents.prune(|_| false);
        assert!(agents.snapshot().connected.is_empty());
    }

    #[test]
    fn names_are_cleaned_up_for_the_person() {
        assert_eq!(name_of("  claude-code \n"), "claude-code");
        assert_eq!(name_of("a\u{7}b"), "ab");
        assert_eq!(name_of(""), "An unnamed client");
        assert_eq!(name_of(&"x".repeat(100)).len(), MAX_NAME);
    }
}
