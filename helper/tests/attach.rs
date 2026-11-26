//! An Agent attaches to a tab the person already has open and works in it on
//! the same signed-in session.
//!
//! Needs a Chromium build: set COHELM_TEST_CHROME to its executable. Set
//! COHELM_TEST_DEVTOOLS_MCP to a local Chrome DevTools MCP script to avoid
//! fetching it with npx. Without COHELM_TEST_CHROME the test is skipped, unless
//! COHELM_REQUIRE_BROWSER_TESTS is set, as it is in CI.

use std::time::Duration;

use cohelm_helper::start;
use rmcp::model::CallToolRequestParams;
use serde_json::json;

mod common;
use common::{
    EXTENSION_PAGE, TEST_AGENT, allow_next_pause, args, call, connect_as, helper_config,
    start_site, test_chrome,
};

#[tokio::test]
async fn agent_works_in_the_persons_open_tab_on_their_session() {
    let Some(chrome) = test_chrome() else {
        return;
    };
    let site = start_site().await;
    let profile = tempfile::tempdir().unwrap();
    let config = helper_config(chrome, profile.path(), &format!("{site}/sign-in"));
    let helper = start(config).await.expect("Helper starts");
    assert!(
        cohelm_helper::cohelm_at(helper.addr, Duration::from_secs(10)).await,
        "the Helper names itself to Agents"
    );
    // The browser is driven over the Helper's private pipe only: it opened
    // no debugging port.
    assert!(
        !profile.path().join("DevToolsActivePort").exists(),
        "the browser opened a debugging port"
    );

    // Approving Agents has its own tests; this one is allowed up front.
    helper.agents().answer(TEST_AGENT, true);
    let agent = connect_as(TEST_AGENT, &helper.mcp_url)
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
    // A script could do anything, so it waits at a Pause; the person allows
    // this one.
    let (own, ()) = tokio::join!(
        call(
            &agent,
            "evaluate_script",
            json!({ "pageId": page_id, "function": "() => navigator.userAgent" }),
        ),
        allow_next_pause(&helper),
    );
    let own_ua = own
        .lines()
        .find_map(|l| l.trim().strip_prefix('"')?.strip_suffix('"'))
        .unwrap_or_else(|| panic!("page reports its user agent:\n{own}"));
    assert!(
        seen.contains(own_ua),
        "site sees the browser's own user agent {own_ua}:\n{seen}"
    );

    // Cohelm's built-in extension is running, and Agents can't open its
    // pages: they will hold the Pause prompts the person answers.
    let mut running = false;
    for _ in 0..50 {
        running = helper.extension_running().await.unwrap();
        if running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(running, "the built-in extension is running");
    let refused = agent
        .call_tool(
            CallToolRequestParams::new("navigate_page").with_arguments(args(
                json!({ "pageId": page_id, "type": "url", "url": EXTENSION_PAGE }),
            )),
        )
        .await
        .unwrap();
    assert_eq!(
        refused.is_error,
        Some(true),
        "an Agent can't open the extension's pages"
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
