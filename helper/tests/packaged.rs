//! Smoke test of an installed Cohelm: the person starts the app, the Helper
//! starts the bundled browser with the built-in extension, and an Agent works
//! in the person's tab through the bundled Chrome DevTools MCP.
//!
//! Set COHELM_PACKAGED_HELPER to the installed Helper: on a Mac
//! `Cohelm.app/Contents/MacOS/Cohelm`, on Windows the installed `Cohelm.exe`.
//! The test runs it as installed, with nothing pointing it at other parts.
//! Skipped when the variable isn't set.

use std::{path::PathBuf, process::Stdio, time::Duration};

use rmcp::{ServiceExt, transport::StreamableHttpClientTransport};
use serde_json::json;

mod common;
use common::{Agent, call, start_site};

#[tokio::test]
async fn installed_cohelm_starts_its_browser_and_serves_an_agent() {
    let Some(helper_exe) = std::env::var_os("COHELM_PACKAGED_HELPER").map(PathBuf::from) else {
        eprintln!("skipped: set COHELM_PACKAGED_HELPER to an installed Cohelm Helper");
        return;
    };
    let site = start_site().await;
    let home = tempfile::tempdir().unwrap();
    let profile = home.path().join("User Data");
    let addr = {
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        free.local_addr().unwrap()
    };

    let mut helper = tokio::process::Command::new(&helper_exe)
        .arg(format!("{site}/sign-in"))
        .env("COHELM_PROFILE_DIR", &profile)
        .env("COHELM_HELPER_ADDR", addr.to_string())
        .env_remove("COHELM_BROWSER")
        .env_remove("COHELM_EXTENSION_DIR")
        .env_remove("COHELM_DEVTOOLS_MCP")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("the installed Helper starts");

    let agent = connect(&format!("http://{addr}/mcp"), &mut helper).await;
    let mut pages = String::new();
    for _ in 0..100 {
        pages = call(&agent, "list_pages", json!({})).await;
        if pages.contains("/sign-in") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        pages.contains("/sign-in"),
        "the person's tab is listed:\n{pages}"
    );
    let page_id: u64 = pages
        .lines()
        .find(|l| l.contains("/sign-in"))
        .and_then(|l| l.split(':').next()?.trim().parse().ok())
        .expect("tab has an id");
    call(
        &agent,
        "navigate_page",
        json!({ "pageId": page_id, "type": "url", "url": format!("{site}/account") }),
    )
    .await;
    let snapshot = call(&agent, "take_snapshot", json!({ "pageId": page_id })).await;
    assert!(
        snapshot.contains("Signed in as Ada"),
        "Agent works on the person's session:\n{snapshot}"
    );
    call(
        &agent,
        "navigate_page",
        json!({ "pageId": page_id, "type": "url", "url": format!("{site}/whoami") }),
    )
    .await;
    let seen = call(&agent, "take_snapshot", json!({ "pageId": page_id })).await;
    assert!(seen.contains("Chrome/"), "site sees Chrome:\n{seen}");
    assert!(
        !seen.to_lowercase().contains("cohelm"),
        "site sees no Cohelm name:\n{seen}"
    );

    // The installed Helper logs beside the profile.
    let log = home.path().join("Helper.log");
    let mut logged = String::new();
    for _ in 0..150 {
        logged = std::fs::read_to_string(&log).unwrap_or_default();
        if logged.contains("built-in extension") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        logged.contains("Cohelm's built-in extension is running"),
        "the built-in extension is running:\n{logged}"
    );

    agent.cancel().await.unwrap();
    quit(&mut helper).await;
}

/// The installed Helper may take a while on its first start.
async fn connect(url: &str, helper: &mut tokio::process::Child) -> Agent {
    for _ in 0..240 {
        if let Some(status) = helper.try_wait().unwrap() {
            panic!("the Helper exited early: {status}");
        }
        if let Ok(agent) = ().serve(StreamableHttpClientTransport::from_uri(url.to_owned())).await {
            return agent;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("the Helper didn't serve Agents within 2 minutes");
}

/// Ask the Helper to quit as the system would, so it closes the browser.
async fn quit(helper: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = helper.id() {
        // SAFETY: signals a child process this test started.
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        if tokio::time::timeout(Duration::from_secs(15), helper.wait())
            .await
            .is_ok()
        {
            return;
        }
    }
    // Windows has no polite signal for a windowless program; end the Helper
    // and the browser it started.
    #[cfg(windows)]
    if let Some(pid) = helper.id() {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status();
    }
    let _ = helper.kill().await;
}
