use std::{net::SocketAddr, path::PathBuf};

use crate::{backend::DevtoolsCommand, browser::BrowserCommand};

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
    #[error("the browser closed")]
    BrowserClosed,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl HelperConfig {
    pub fn new(listen: SocketAddr, browser: BrowserCommand) -> Self {
        Self {
            listen,
            browser,
            devtools: DevtoolsCommand::default(),
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
