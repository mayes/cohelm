//! The Cohelm Helper: serves MCP to Agents on this computer and drives the
//! person's own running browser through Chrome DevTools MCP.

mod backend;
mod config;
mod server;

pub use backend::DevtoolsCommand;
pub use config::{HelperConfig, HelperError};
pub use server::{RunningHelper, start};
