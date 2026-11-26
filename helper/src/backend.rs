use std::ffi::OsString;

use rmcp::{
    RoleClient, ServiceExt,
    service::RunningService,
    transport::{ConfigureCommandExt, TokioChildProcess},
};

use crate::config::HelperError;

/// The pinned Chrome DevTools MCP release the Helper is built on.
pub const DEVTOOLS_MCP_VERSION: &str = "1.14.0";

/// How to start Chrome DevTools MCP. The Helper always adds the flags that
/// attach it to the person's running browser and switch off Google's usage
/// statistics and CrUX lookups.
#[derive(Debug, Clone)]
pub struct DevtoolsCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
}

impl Default for DevtoolsCommand {
    fn default() -> Self {
        Self {
            program: "npx".into(),
            args: vec![
                "-y".into(),
                format!("chrome-devtools-mcp@{DEVTOOLS_MCP_VERSION}").into(),
            ],
        }
    }
}

impl DevtoolsCommand {
    /// Run a local copy of Chrome DevTools MCP with node.
    pub fn node_script(script: impl Into<OsString>) -> Self {
        Self {
            program: "node".into(),
            args: vec![script.into()],
        }
    }

    pub(crate) fn args_for(&self, browser_url: &str) -> Vec<OsString> {
        let mut args = self.args.clone();
        args.extend(
            [
                "--browserUrl",
                browser_url,
                "--no-usage-statistics",
                "--no-performance-crux",
            ]
            .map(OsString::from),
        );
        args
    }
}

pub(crate) type Backend = RunningService<RoleClient, ()>;

/// Start Chrome DevTools MCP attached to the running browser.
pub(crate) async fn connect(
    command: &DevtoolsCommand,
    browser_url: &str,
) -> Result<Backend, HelperError> {
    let args = command.args_for(browser_url);
    let transport = TokioChildProcess::new(
        tokio::process::Command::new(&command.program).configure(|cmd| {
            cmd.args(&args)
                .env("CHROME_DEVTOOLS_MCP_NO_USAGE_STATISTICS", "1")
                .kill_on_drop(true);
        }),
    )?;
    ().serve(transport)
        .await
        .map_err(|e| HelperError::Devtools(e.to_string()))
}
