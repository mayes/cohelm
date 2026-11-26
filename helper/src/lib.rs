//! The Cohelm Helper: starts the person's browser with a private DevTools
//! pipe, and serves MCP to Agents on this computer so they can work in the
//! person's own tabs through Chrome DevTools MCP.

mod agents;
mod backend;
mod bridge;
mod browser;
mod config;
mod layout;
mod pages;
mod pauses;
mod server;
mod updates;

/// Cohelm's built-in extension, fixed by the key in `extension/manifest.json`.
pub const EXTENSION_ID: &str = "dojelhkgldmdbffaafmofoilmbbacppo";

pub use agents::Agents;
pub use agents::{Refusal, Snapshot};
pub use backend::DevtoolsCommand;
pub use browser::BrowserCommand;
pub use config::{HelperConfig, HelperError};
pub use layout::{Layout, Platform};
pub use pauses::{
    Answer, Kind, Need, PAUSE_WAIT, Page, Pauses, Step, Verdict, Waiting, classify, need,
};
pub use server::{BrowserProbe, RunningHelper, SERVER_NAME, cohelm_at, start};
pub use updates::{Update, UpdateCheck, newest};
