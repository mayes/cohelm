use std::{ffi::OsString, path::Path};

use rmcp::{
    RoleClient, ServiceExt,
    model::CallToolRequestParams,
    service::RunningService,
    transport::{ConfigureCommandExt, TokioChildProcess, which_command},
};

use crate::config::HelperError;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The pinned Chrome DevTools MCP release the Helper is built on.
pub const DEVTOOLS_MCP_VERSION: &str = "1.14.0";

/// How to start Chrome DevTools MCP. The Helper always adds the config file
/// that points it at the browser, the flags that switch off Google's usage
/// statistics and CrUX lookups, and the environment that stops its daily
/// update check and its search for a user config file.
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

    pub(crate) fn args_for(&self, config_file: &Path) -> Vec<OsString> {
        let mut args = self.args.clone();
        args.push("--config".into());
        args.push(config_file.into());
        // Structured content lets the Pause read snapshots as data, not as
        // text a page could forge lines into.
        args.extend(
            [
                "--no-usage-statistics",
                "--no-performance-crux",
                "--experimentalStructuredContent",
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

/// Write the config file that points Chrome DevTools MCP at the bridge. It
/// holds the bridge's secret, so it goes in a folder only this user can read
/// rather than on a command line other programs can see.
pub(crate) fn write_config(dir: &Path, ws_url: &str) -> Result<std::path::PathBuf, HelperError> {
    let path = dir.join("chrome-devtools-mcp.json");
    let config = serde_json::json!({ "wsEndpoint": ws_url });
    std::fs::write(&path, config.to_string())?;
    Ok(path)
}

/// Start Chrome DevTools MCP on the browser, and check that the browser
/// answers before any Agent is told the Helper is ready.
pub(crate) async fn connect(
    command: &DevtoolsCommand,
    config_file: &Path,
) -> Result<Backend, HelperError> {
    let args = command.args_for(config_file);
    // Resolves npx.cmd and the like on Windows.
    let program = which_command(&command.program).map_err(|e| {
        HelperError::Devtools(format!(
            "can't find {}: {e}",
            command.program.to_string_lossy()
        ))
    })?;
    let transport = TokioChildProcess::new(program.configure(|cmd| {
        cmd.args(&args).envs(DEVTOOLS_ENV).kill_on_drop(true);
        // Node is a console program: without this an installed Cohelm,
        // which has no console, would open a window for it, and closing
        // that window would stop Chrome DevTools MCP.
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
    }))?;
    let backend = ().serve(transport).await.map_err(|e| HelperError::Devtools(e.to_string()))?;

    let probe = backend
        .call_tool(CallToolRequestParams::new("list_pages"))
        .await
        .map_err(|e| HelperError::BrowserUnreachable(e.to_string()))?;
    if probe.is_error == Some(true) {
        let detail = probe
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
            .collect::<Vec<_>>()
            .join(" ");
        return Err(HelperError::BrowserUnreachable(detail));
    }
    Ok(backend)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devtools_is_always_told_to_stay_private() {
        let args = DevtoolsCommand::default().args_for(Path::new("/run/cohelm/cdm.json"));
        for flag in [
            "--no-usage-statistics",
            "--no-performance-crux",
            "--experimentalStructuredContent",
            "--config",
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
    fn the_secret_stays_off_the_command_line() {
        let dir = tempfile::tempdir().unwrap();
        let url = "ws://127.0.0.1:5000/devtools/browser/s3cret";
        let file = write_config(dir.path(), url).unwrap();
        let args = DevtoolsCommand::default().args_for(&file);
        assert!(!args.iter().any(|a| a.to_string_lossy().contains("s3cret")));
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
        assert_eq!(written["wsEndpoint"], url);
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
