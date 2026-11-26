//! An Agent attaches to a tab the person already has open and works in it on
//! the same signed-in session.
//!
//! Needs a Chromium build: set COHELM_TEST_CHROME to its executable. Set
//! COHELM_TEST_DEVTOOLS_MCP to a local Chrome DevTools MCP script to avoid
//! fetching it with npx. Without COHELM_TEST_CHROME the test is skipped, unless
//! COHELM_REQUIRE_BROWSER_TESTS is set, as it is in CI.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

use axum::{
    Router,
    http::{HeaderMap, header},
    response::IntoResponse,
    routing::get,
};
use cohelm_helper::{BrowserCommand, DevtoolsCommand, HelperConfig, start};
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

/// Echoes what the site is told about the browser.
async fn whoami(headers: HeaderMap) -> impl IntoResponse {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("(none)")
            .to_owned()
    };
    axum::response::Html(format!(
        "<p>user-agent: {}</p><p>sec-ch-ua: {}</p>",
        get("user-agent"),
        get("sec-ch-ua")
    ))
}

/// A site where the person signs in, standing in for their real sites.
async fn start_site() -> String {
    let app = Router::new()
        .route("/sign-in", get(sign_in))
        .route("/account", get(account))
        .route("/whoami", get(whoami));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// The person's browser as the Helper starts it, open on a page where they
/// sign in.
const EXTENSION_PAGE: &str = "chrome-extension://dojelhkgldmdbffaafmofoilmbbacppo/about.html";

fn browser_command(chrome: PathBuf, profile: &Path, first_page: &str) -> BrowserCommand {
    let mut browser = BrowserCommand::new(chrome, profile);
    browser.extension_dir = Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("../extension"));
    browser.args = [
        "--headless=new",
        "--no-first-run",
        "--no-default-browser-check",
    ]
    .map(OsString::from)
    .to_vec();
    // Extra flags for the test machine only, such as --no-sandbox on CI
    // runners that block Chromium's sandbox. Cohelm itself never adds them.
    if let Ok(extra) = std::env::var("COHELM_TEST_CHROME_ARGS") {
        browser
            .args
            .extend(extra.split_whitespace().map(OsString::from));
    }
    browser.args.push(first_page.into());
    browser
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

/// Calls a tool and fails the test if the tool reports an error.
async fn call(
    agent: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    tool: &'static str,
    arguments: Value,
) -> String {
    let result = agent
        .call_tool(CallToolRequestParams::new(tool).with_arguments(args(arguments)))
        .await
        .unwrap_or_else(|e| panic!("{tool} failed: {e}"));
    let text = text_of(&result);
    assert!(
        result.is_error != Some(true),
        "{tool} returned an error:\n{text}"
    );
    text
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
    let profile = tempfile::tempdir().unwrap();
    let mut config = HelperConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        browser_command(chrome, profile.path(), &format!("{site}/sign-in")),
    );
    if let Some(script) = std::env::var_os("COHELM_TEST_DEVTOOLS_MCP") {
        config.devtools = DevtoolsCommand::node_script(script);
    }
    let helper = start(config).await.expect("Helper starts");
    // The browser is driven over the Helper's private pipe only: it opened
    // no debugging port.
    assert!(
        !profile.path().join("DevToolsActivePort").exists(),
        "the browser opened a debugging port"
    );

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

    // The person's tab may still be loading when the Helper is ready.
    let mut pages = String::new();
    for _ in 0..50 {
        pages = call(&agent, "list_pages", json!({})).await;
        if pages.contains("/sign-in") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let signed_in_tab = pages
        .lines()
        .find(|l| l.contains("/sign-in"))
        .unwrap_or_else(|| panic!("the person's open tab is listed:\n{pages}"));
    let page_id: u64 = signed_in_tab
        .split(':')
        .next()
        .and_then(|id| id.trim().parse().ok())
        .expect("tab has an id");

    // chrome-devtools-mcp routes page tools by pageId, so each Agent names
    // the tab it works in.
    call(&agent, "select_page", json!({ "pageId": page_id })).await;
    call(
        &agent,
        "navigate_page",
        json!({ "pageId": page_id, "type": "url", "url": format!("{site}/account") }),
    )
    .await;
    let snapshot = call(&agent, "take_snapshot", json!({ "pageId": page_id })).await;
    assert!(
        snapshot.contains("Signed in as Ada"),
        "Agent sees the page on the person's session:\n{snapshot}"
    );

    // Sites see the browser's own identity, with no Cohelm name in it.
    call(
        &agent,
        "navigate_page",
        json!({ "pageId": page_id, "type": "url", "url": format!("{site}/whoami") }),
    )
    .await;
    let seen = call(&agent, "take_snapshot", json!({ "pageId": page_id })).await;
    assert!(
        seen.contains("Chrome/"),
        "site sees a Chrome user agent:\n{seen}"
    );
    assert!(
        !seen.to_lowercase().contains("cohelm"),
        "site sees no Cohelm name:\n{seen}"
    );
    let own = call(
        &agent,
        "evaluate_script",
        json!({ "pageId": page_id, "function": "() => navigator.userAgent" }),
    )
    .await;
    let own_ua = own
        .lines()
        .find_map(|l| l.trim().strip_prefix('"')?.strip_suffix('"'))
        .unwrap_or_else(|| panic!("page reports its user agent:\n{own}"));
    assert!(
        seen.contains(own_ua),
        "site sees the browser's own user agent {own_ua}:\n{seen}"
    );

    // Cohelm's built-in extension is loaded. Its ID comes from the key in
    // extension/manifest.json.
    call(
        &agent,
        "navigate_page",
        json!({ "pageId": page_id, "type": "url", "url": EXTENSION_PAGE }),
    )
    .await;
    let extension = call(&agent, "take_snapshot", json!({ "pageId": page_id })).await;
    assert!(
        extension.contains("Cohelm extension ready"),
        "the built-in extension is loaded:\n{extension}"
    );

    // An Agent can't change that identity.
    let refused = agent
        .call_tool(CallToolRequestParams::new("emulate").with_arguments(args(
            json!({ "pageId": page_id, "userAgent": "Cohelm/0.1" }),
        )))
        .await
        .unwrap();
    assert_eq!(
        refused.is_error,
        Some(true),
        "emulate with userAgent is refused"
    );

    agent.cancel().await.unwrap();
    helper.stop().await;
}
