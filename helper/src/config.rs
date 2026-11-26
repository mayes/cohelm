use std::{net::SocketAddr, path::PathBuf, time::Duration};

use crate::{backend::DevtoolsCommand, browser::BrowserCommand, updates::UpdateCheck};

/// How the Helper is started.
#[derive(Debug, Clone)]
pub struct HelperConfig {
    /// Where Agents connect. Must be a loopback address.
    pub listen: SocketAddr,
    /// How to start the person's Cohelm browser, which the Helper drives
    /// over a private pipe.
    pub browser: BrowserCommand,
    /// How to start Chrome DevTools MCP.
    pub devtools: DevtoolsCommand,
    /// Where the Agents the person allowed are kept. By default a file in
    /// the browser profile, so they go with it.
    pub agents_file: Option<PathBuf>,
    /// How long a new Agent's call is held while the person decides.
    pub approval_wait: Duration,
    /// How long a Pause waits for the person before it counts as blocked.
    pub pause_wait: Duration,
    /// How the Helper looks for a newer Cohelm. None never looks.
    pub updates: Option<UpdateCheck>,
}

#[derive(Debug, thiserror::Error)]
pub enum HelperError {
    #[error("the Helper only listens on this computer, not on {0}")]
    NotLoopback(SocketAddr),
    #[error("the browser profile folder must be a full path, not {0}")]
    Profile(PathBuf),
    #[error("no extension manifest.json in {0}")]
    Extension(PathBuf),
    #[error("could not start the browser {0}: {1}")]
    BrowserStart(String, String),
    #[error("could not start Chrome DevTools MCP: {0}")]
    Devtools(String),
    #[error("can't reach the browser: {0}")]
    BrowserUnreachable(String),
    #[error("lost the connection to Chrome DevTools MCP")]
    DevtoolsClosed,
    #[error("Cohelm is already running: {0} is in use")]
    AlreadyRunning(SocketAddr),
    #[error("the browser closed")]
    BrowserClosed,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl HelperConfig {
    pub fn new(listen: SocketAddr, browser: BrowserCommand) -> Self {
        Self {
            listen,
            agents_file: Some(browser.profile_dir.join("Cohelm Agents.json")),
            browser,
            devtools: DevtoolsCommand::default(),
            approval_wait: Duration::from_secs(60),
            pause_wait: crate::pauses::PAUSE_WAIT,
            updates: None,
        }
    }

    /// Refuse anything that would let another machine drive the browser.
    pub fn validate(&self) -> Result<(), HelperError> {
        if !self.listen.ip().is_loopback() {
            return Err(HelperError::NotLoopback(self.listen));
        }
        self.browser.validate()
    }
}
