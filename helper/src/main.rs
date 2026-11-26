// An installed Cohelm on Windows starts without a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::{ffi::OsString, net::SocketAddr, path::PathBuf};

use anyhow::Context;
use cohelm_helper::{BrowserCommand, DevtoolsCommand, HelperConfig, HelperError, Layout, start};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let installed = Layout::installed();
    let profile_dir = match std::env::var_os("COHELM_PROFILE_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => default_profile_dir()?,
    };
    init_logging(installed.is_some(), &profile_dir);

    let listen: SocketAddr = std::env::var("COHELM_HELPER_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:47600".into())
        .parse()?;
    let program = std::env::var_os("COHELM_BROWSER")
        .map(PathBuf::from)
        .or_else(|| installed.as_ref().map(|l| l.browser.clone()))
        .context("set COHELM_BROWSER to the Cohelm browser executable")?;
    let mut browser = BrowserCommand::new(program, profile_dir);
    browser.extension_dir = std::env::var_os("COHELM_EXTENSION_DIR")
        .map(PathBuf::from)
        .or_else(|| installed.as_ref().map(|l| l.extension.clone()));
    // Pages to open, as for any browser.
    let pages: Vec<OsString> = std::env::args_os().skip(1).collect();
    browser.args.extend(pages.iter().cloned());
    let mut config = HelperConfig::new(listen, browser.clone());
    if let Ok(script) = std::env::var("COHELM_DEVTOOLS_MCP") {
        config.devtools = DevtoolsCommand::node_script(script);
    } else if let Some(layout) = &installed {
        config.devtools = layout.devtools();
    }

    let mut helper = match start(config).await {
        Ok(helper) => helper,
        Err(HelperError::AlreadyRunning(addr)) => {
            tracing::info!("Cohelm is already running on {addr}; handing it the pages");
            return hand_to_running_browser(&browser, &pages).await;
        }
        Err(e) => {
            tracing::error!("Cohelm could not start: {e}");
            return Err(e.into());
        }
    };
    tracing::info!("Cohelm Helper is ready at {}", helper.mcp_url);
    eprintln!(
        "Cohelm Helper is ready. Connect your MCP client to {}",
        helper.mcp_url
    );
    tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            signal?;
            helper.stop().await;
            Ok(())
        }
        stopped = helper.stopped() => match stopped {
            // The person quit the browser: a normal way to quit Cohelm.
            Ok(()) | Err(HelperError::BrowserClosed) => Ok(()),
            Err(e) => {
                tracing::error!("Cohelm stopped: {e}");
                Err(e.into())
            }
        },
    }
}

/// A second start of Cohelm, such as opening a link with it, while one is
/// running. Chromium passes the pages on to the browser that already has the
/// profile open, which opens them, and exits. No new Helper starts.
async fn hand_to_running_browser(
    browser: &BrowserCommand,
    pages: &[OsString],
) -> anyhow::Result<()> {
    let mut profile = OsString::from("--user-data-dir=");
    profile.push(&browser.profile_dir);
    let status = tokio::process::Command::new(&browser.program)
        .arg(profile)
        .args(pages)
        .status()
        .await
        .context("could not reach the running Cohelm browser")?;
    anyhow::ensure!(status.success(), "the running Cohelm browser said {status}");
    Ok(())
}

/// An installed Helper has no terminal to write to, so it logs to a file
/// beside the profile. Run from a terminal, it logs there.
fn init_logging(installed: bool, profile_dir: &std::path::Path) {
    let builder = tracing_subscriber::fmt().with_ansi(!installed);
    if installed
        && let Some(file) = profile_dir.parent().and_then(|dir| {
            std::fs::create_dir_all(dir).ok()?;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("Helper.log"))
                .ok()
        })
    {
        builder.with_writer(std::sync::Mutex::new(file)).init();
    } else {
        builder.with_writer(std::io::stderr).init();
    }
}

/// Cohelm's own profile, apart from Chrome's.
fn default_profile_dir() -> anyhow::Result<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    let base =
        base.context("can't find a folder for the browser profile; set COHELM_PROFILE_DIR")?;
    Ok(base.join("Cohelm").join("User Data"))
}
