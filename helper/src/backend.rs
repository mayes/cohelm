use std::ffi::OsString;

use rmcp::{
    RoleClient, ServiceExt,
    model::CallToolRequestParams,
    service::RunningService,
    transport::{ConfigureCommandExt, TokioChildProcess, which_command},
};

use crate::config::HelperError;

/// The pinned Chrome DevTools MCP release the Helper is built on.
pub const DEVTOOLS_MCP_VERSION: &str = "1.14.0";

/// How to start Chrome DevTools MCP. The Helper always adds the flags that
/// attach it to the person's running browser and switch off Google's usage
/// statistics and CrUX lookups, and the environment that stops its daily
/// update check and its loading of a user config file.
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

/// Environment the Helper sets on Chrome DevTools MCP so it sends no usage
/// data, makes no update checks and reads no config file of its own.
pub(crate) const DEVTOOLS_ENV: [(&str, &str); 3] = [
    ("CHROME_DEVTOOLS_MCP_NO_USAGE_STATISTICS", "1"),
    ("CHROME_DEVTOOLS_MCP_NO_UPDATE_CHECKS", "1"),
    ("CHROME_DEVTOOLS_MCP_NO_CONFIG_DISCOVERY", "1"),
];

/// Start Chrome DevTools MCP attached to the running browser, and check that
/// the browser answers before any Agent is told the Helper is ready.
pub(crate) async fn connect(
    command: &DevtoolsCommand,
    browser_url: &str,
) -> Result<Backend, HelperError> {
    let args = command.args_for(browser_url);
    // Resolves npx.cmd and the like on Windows.
    let program = which_command(&command.program).map_err(|e| {
        HelperError::Devtools(format!(
            "can't find {}: {e}",
            command.program.to_string_lossy()
        ))
    })?;
    let transport = TokioChildProcess::new(program.configure(|cmd| {
        cmd.args(&args).envs(DEVTOOLS_ENV).kill_on_drop(true);
    }))?;
    let backend = ().serve(transport).await.map_err(|e| HelperError::Devtools(e.to_string()))?;

    let probe = backend
        .call_tool(CallToolRequestParams::new("list_pages"))
        .await
        .map_err(|e| HelperError::BrowserUnreachable(browser_url.to_owned(), e.to_string()))?;
    if probe.is_error == Some(true) {
        let detail = probe
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
            .collect::<Vec<_>>()
            .join(" ");
        return Err(HelperError::BrowserUnreachable(
            browser_url.to_owned(),
            detail,
        ));
    }
    Ok(backend)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devtools_is_always_told_to_stay_private() {
        let args = DevtoolsCommand::default().args_for("http://127.0.0.1:9222");
        for flag in [
            "--no-usage-statistics",
            "--no-performance-crux",
            "--browserUrl",
        ] {
            assert!(args.iter().any(|a| a == flag), "missing {flag}");
        }
        for key in [
            "CHROME_DEVTOOLS_MCP_NO_USAGE_STATISTICS",
            "CHROME_DEVTOOLS_MCP_NO_UPDATE_CHECKS",
            "CHROME_DEVTOOLS_MCP_NO_CONFIG_DISCOVERY",
        ] {
            assert!(DEVTOOLS_ENV.iter().any(|(k, _)| *k == key), "missing {key}");
        }
    }

    #[test]
    fn devtools_pins_its_release() {
        let args = DevtoolsCommand::default().args;
        assert!(
            args.iter()
                .any(|a| a == format!("chrome-devtools-mcp@{DEVTOOLS_MCP_VERSION}").as_str())
        );
    }
}
