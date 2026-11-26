use std::net::{IpAddr, SocketAddr};

use url::Url;

use crate::backend::DevtoolsCommand;

/// How the Helper is started.
#[derive(Debug, Clone)]
pub struct HelperConfig {
    /// Where Agents connect. Must be a loopback address.
    pub listen: SocketAddr,
    /// The DevTools address of the person's running Cohelm browser.
    pub browser_url: String,
    /// How to start Chrome DevTools MCP.
    pub devtools: DevtoolsCommand,
}

#[derive(Debug, thiserror::Error)]
pub enum HelperError {
    #[error("the Helper only listens on this computer, not on {0}")]
    NotLoopback(SocketAddr),
    #[error("the browser must run on this computer, not at {0}")]
    BrowserNotLocal(String),
    #[error("could not start Chrome DevTools MCP: {0}")]
    Devtools(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl HelperConfig {
    pub fn new(listen: SocketAddr, browser_url: impl Into<String>) -> Self {
        Self {
            listen,
            browser_url: browser_url.into(),
            devtools: DevtoolsCommand::default(),
        }
    }

    /// Refuse anything that would let another machine drive the browser.
    pub fn validate(&self) -> Result<(), HelperError> {
        if !self.listen.ip().is_loopback() {
            return Err(HelperError::NotLoopback(self.listen));
        }
        let local = Url::parse(&self.browser_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .is_some_and(|host| is_local_host(&host));
        if !local {
            return Err(HelperError::BrowserNotLocal(self.browser_url.clone()));
        }
        Ok(())
    }
}

fn is_local_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}
