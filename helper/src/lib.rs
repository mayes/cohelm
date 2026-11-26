//! The Cohelm Helper: starts the person's browser with a private DevTools
//! pipe, and serves MCP to Agents on this computer so they can work in the
//! person's own tabs through Chrome DevTools MCP.

mod backend;
mod bridge;
mod browser;
mod config;
mod server;

pub use backend::DevtoolsCommand;
pub use browser::BrowserCommand;
pub use config::{HelperConfig, HelperError};
pub use server::{RunningHelper, start};
