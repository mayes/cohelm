// An installed Cohelm on Windows starts without a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::{ffi::OsString, net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::Context;
use cohelm_helper::{
    BrowserCommand, BrowserProbe, DevtoolsCommand, HelperConfig, HelperError, Layout, cohelm_at,
    start,
};

/// How long a second Cohelm waits for the first one to finish starting.
const FIRST_START_WAIT: Duration = Duration::from_secs(120);
/// How long the running browser gets to take the pages.
const HAND_OFF_WAIT: Duration = Duration::from_secs(30);
/// The log is started afresh past this size.
const MAX_LOG: u64 = 10 << 20;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Listen for a request to quit before anything starts, so one that comes
    // during start-up still closes the browser cleanly.
    let quit = QuitSignal::listen()?;
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
        .or_else(|| installed.as_ref().map(|l| l.extension.clone()))
        .or_else(dev_extension_dir);
    if browser.extension_dir.is_none() {
        tracing::warn!(
            "no built-in extension: only Agents allowed before can connect, \
             since nothing can ask the person about a new one"
        );
    }
    // Pages to open, as for any browser. Anything that looks like a switch
    // is dropped, so nothing can change how the browser runs this way.
    let pages: Vec<OsString> = std::env::args_os()
        .skip(1)
        .filter(|arg| {
            let switch = arg.to_string_lossy().starts_with('-');
            if switch {
                tracing::warn!(
                    "ignoring {}: Cohelm takes only pages to open",
                    arg.display()
                );
            }
            !switch
        })
        .collect();
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
            if !cohelm_at(addr, FIRST_START_WAIT).await {
                tracing::error!("another program is using {addr}");
                anyhow::bail!("another program is using {addr}; quit it or set COHELM_HELPER_ADDR");
            }
            tracing::info!("Cohelm is already running on {addr}; handing it the pages");
            return hand_to_running_browser(&browser, &pages).await;
        }
        Err(e) => {
            tracing::error!("Cohelm could not start: {e}");
            return Err(e.into());
        }
    };
    tracing::info!("Cohelm Helper is ready at {}", helper.mcp_url);
    if browser.extension_dir.is_some() {
        tokio::spawn(report_extension(helper.probe()));
    }
    eprintln!(
        "Cohelm Helper is ready. Connect your MCP client to {}",
        helper.mcp_url
    );
    tokio::select! {
        () = quit.wait() => {
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

/// Ctrl-C, or on Mac and Linux a request to end such as logging out.
struct QuitSignal {
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
}

impl QuitSignal {
    fn listen() -> std::io::Result<Self> {
        Ok(Self {
            #[cfg(unix)]
            terminate: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
        })
    }

    async fn wait(mut self) {
        #[cfg(unix)]
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = self.terminate.recv() => {}
        }
        #[cfg(not(unix))]
        {
            let _ = &mut self;
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

/// Say in the log whether the built-in extension came up, since the person
/// would otherwise see no Pause prompts and not know why.
async fn report_extension(probe: BrowserProbe) {
    for _ in 0..50 {
        if let Ok(true) = probe.extension_running().await {
            tracing::info!("Cohelm's built-in extension is running");
            if let Ok(kinds) = probe.target_kinds().await {
                tracing::info!("browser targets: {}", kinds.join(", "));
            }
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    tracing::warn!("Cohelm's built-in extension didn't start");
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
    let handed = tokio::process::Command::new(&browser.program)
        .arg(profile)
        .args(pages)
        .kill_on_drop(true)
        .status();
    let status = tokio::time::timeout(HAND_OFF_WAIT, handed)
        .await
        .context("the running Cohelm browser didn't take the pages")?
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
            let log = dir.join("Helper.log");
            if std::fs::metadata(&log).is_ok_and(|m| m.len() > MAX_LOG) {
                let _ = std::fs::rename(&log, dir.join("Helper.old.log"));
            }
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log)
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

/// A development build run from the repo uses the extension beside it, so
/// the prompt works without setting `COHELM_EXTENSION_DIR`.
fn dev_extension_dir() -> Option<PathBuf> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../extension");
    dir.join("manifest.json").is_file().then_some(dir)
}
