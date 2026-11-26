//! An Agent attaches to a tab the person already has open and works in it on
//! the same signed-in session.
//!
//! Needs a Chromium build: set COHELM_TEST_CHROME to its executable. Set
//! COHELM_TEST_DEVTOOLS_MCP to a local Chrome DevTools MCP script to avoid
//! fetching it with npx. Without COHELM_TEST_CHROME the test is skipped, unless
//! COHELM_REQUIRE_BROWSER_TESTS is set, as it is in CI.

use std::{path::PathBuf, process::Stdio, time::Duration};

use axum::{
    Router,
    http::{HeaderMap, header},
    response::IntoResponse,
    routing::get,
};
use cohelm_helper::{DevtoolsCommand, HelperConfig, start};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, CallToolResult},
    transport::StreamableHttpClientTransport,
};
use serde_json::{Map, Value, json};

async fn sign_in() -> impl IntoResponse {
    (
        [(header::SET_COOKIE, "session=ada; Path=/; HttpOnly")],
        axum::response::Html("<h1>Welcome back</h1>"),
    )
}

async fn account(headers: HeaderMap) -> impl IntoResponse {
    let signed_in = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.split("; ").any(|p| p == "session=ada"));
    axum::response::Html(if signed_in {
        "<h1>Signed in as Ada</h1>"
    } else {
        "<h1>Not signed in</h1>"
    })
}

/// A site where the person signs in, standing in for their real sites.
async fn start_site() -> String {
    let app = Router::new()
        .route("/sign-in", get(sign_in))
        .route("/account", get(account));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

struct Browser {
    child: tokio::process::Child,
    debug_url: String,
    _profile: tempfile::TempDir,
}

/// The person's browser, already open on a page where they signed in.
async fn open_browser(chrome: PathBuf, first_page: &str) -> Browser {
    let profile = tempfile::tempdir().unwrap();
    let log_path = profile.path().join("chromium-stderr.log");
    let log = std::fs::File::create(&log_path).unwrap();
    // Extra flags for the test machine only, such as --no-sandbox on CI
    // runners that block Chromium's sandbox. Cohelm itself never adds them.
    let extra: Vec<String> = std::env::var("COHELM_TEST_CHROME_ARGS")
        .map(|v| v.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default();
    let mut child = tokio::process::Command::new(chrome)
        .args([
            "--headless=new",
            "--no-first-run",
            "--no-default-browser-check",
            "--remote-debugging-port=0",
        ])
        .args(&extra)
        .arg(format!("--user-data-dir={}", profile.path().display()))
        .arg(first_page)
        .stdout(Stdio::null())
        .stderr(log)
        .kill_on_drop(true)
        .spawn()
        .expect("start Chromium");
    let port_file = profile.path().join("DevToolsActivePort");
    let mut port = None;
    let mut exited = None;
    for _ in 0..300 {
        if let Ok(text) = std::fs::read_to_string(&port_file)
            && let Some(line) = text.lines().next().filter(|l| !l.is_empty())
        {
            port = Some(line.to_owned());
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            exited = Some(status);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let Some(port) = port else {
        let stderr = std::fs::read_to_string(&log_path).unwrap_or_default();
        let tail: Vec<&str> = stderr.lines().rev().take(20).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        match exited {
            Some(status) => panic!(
                "Chromium exited ({status}) before writing its DevTools port:\n{}",
                tail.join("\n")
            ),
            None => panic!(
                "Chromium wrote no DevTools port within 30 s:\n{}",
                tail.join("\n")
            ),
        }
    };
    Browser {
        child,
        debug_url: format!("http://127.0.0.1:{port}"),
        _profile: profile,
    }
}

fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn args(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap()
}

#[tokio::test]
async fn agent_works_in_the_persons_open_tab_on_their_session() {
    let Some(chrome) = std::env::var_os("COHELM_TEST_CHROME").map(PathBuf::from) else {
        assert!(
            std::env::var_os("COHELM_REQUIRE_BROWSER_TESTS").is_none(),
            "COHELM_REQUIRE_BROWSER_TESTS is set but COHELM_TEST_CHROME is not"
        );
        eprintln!("skipped: set COHELM_TEST_CHROME to a Chromium executable");
        return;
    };
    let site = start_site().await;
    let mut browser = open_browser(chrome, &format!("{site}/sign-in")).await;

    let mut config = HelperConfig::new("127.0.0.1:0".parse().unwrap(), &browser.debug_url);
    if let Some(script) = std::env::var_os("COHELM_TEST_DEVTOOLS_MCP") {
        config.devtools = DevtoolsCommand::node_script(script);
    }
    let helper = start(config).await.expect("Helper starts");

    let agent = ()
        .serve(StreamableHttpClientTransport::from_uri(
            helper.mcp_url.clone(),
        ))
        .await
        .expect("Agent connects");

    let tools = agent.list_all_tools().await.unwrap();
    for name in [
        "list_pages",
        "select_page",
        "navigate_page",
        "take_snapshot",
    ] {
        assert!(tools.iter().any(|t| t.name == name), "Agent can use {name}");
    }

    let pages = text_of(
        &agent
            .call_tool(CallToolRequestParams::new("list_pages"))
            .await
            .unwrap(),
    );
    let signed_in_tab = pages
        .lines()
        .find(|l| l.contains("/sign-in"))
        .unwrap_or_else(|| panic!("the person's open tab is listed:\n{pages}"));
    let page_id: u64 = signed_in_tab
        .split(':')
        .next()
        .and_then(|id| id.trim().parse().ok())
        .expect("tab has an id");

    agent
        .call_tool(
            CallToolRequestParams::new("select_page")
                .with_arguments(args(json!({ "pageId": page_id }))),
        )
        .await
        .unwrap();
    agent
        .call_tool(
            CallToolRequestParams::new("navigate_page").with_arguments(args(
                json!({ "type": "url", "url": format!("{site}/account") }),
            )),
        )
        .await
        .unwrap();
    let snapshot = text_of(
        &agent
            .call_tool(CallToolRequestParams::new("take_snapshot"))
            .await
            .unwrap(),
    );
    assert!(
        snapshot.contains("Signed in as Ada"),
        "Agent sees the page on the person's session:\n{snapshot}"
    );

    agent.cancel().await.unwrap();
    helper.stop().await;
    let _ = browser.child.kill().await;
}
