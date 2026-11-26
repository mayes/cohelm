use std::{net::SocketAddr, path::PathBuf};

use anyhow::Context;
use cohelm_helper::{BrowserCommand, DevtoolsCommand, HelperConfig, HelperError, start};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let listen: SocketAddr = std::env::var("COHELM_HELPER_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:47600".into())
        .parse()?;
    let program = std::env::var_os("COHELM_BROWSER")
        .map(PathBuf::from)
        .context("set COHELM_BROWSER to the Cohelm browser executable")?;
    let profile_dir = match std::env::var_os("COHELM_PROFILE_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => default_profile_dir()?,
    };
    let mut browser = BrowserCommand::new(program, profile_dir);
    // Pages to open, as for any browser.
    browser.args.extend(std::env::args_os().skip(1));
    let mut config = HelperConfig::new(listen, browser);
    if let Ok(script) = std::env::var("COHELM_DEVTOOLS_MCP") {
        config.devtools = DevtoolsCommand::node_script(script);
    }

    let mut helper = start(config).await?;
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
            Err(e) => Err(e.into()),
        },
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
