//! The person decides which Agents drive their browser: a new MCP client is
//! held until they allow it, a declined one is refused, an allowed one comes
//! back without asking, and disconnecting one ends its session at once.
//!
//! The person answers in Cohelm's prompt; these tests answer through the
//! Helper's own hook, at the MCP seam. Needs a Chromium build, as attach.rs
//! does.

use std::time::Duration;

use cohelm_helper::{RunningHelper, start};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::json;

mod common;
use common::{Agent, args, connect_as, helper_config, start_site, test_chrome, text_of};

async fn list_pages(agent: &Agent) -> Result<CallToolResult, rmcp::ServiceError> {
    agent
        .call_tool(CallToolRequestParams::new("list_pages").with_arguments(args(json!({}))))
        .await
}

/// Wait until the person is being asked about `name`.
async fn asked_about(helper: &RunningHelper, name: &str) {
    let mut changed = helper.agents().watch();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !helper.agents().snapshot().asking.iter().any(|n| n == name) {
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the person is asked about {name}"));
}

#[tokio::test]
async fn a_new_agent_waits_for_the_person_and_a_declined_one_is_refused() {
    let Some(chrome) = test_chrome() else {
        return;
    };
    let site = start_site().await;
    let profile = tempfile::tempdir().unwrap();
    let mut config = helper_config(chrome, profile.path(), &format!("{site}/sign-in"));
    config.approval_wait = Duration::from_secs(30);
    let helper = start(config).await.expect("Helper starts");

    // A new Agent may connect and see the tools, but its first call waits.
    let newcomer = connect_as("newcomer", &helper.mcp_url).await.unwrap();
    assert!(!newcomer.list_all_tools().await.unwrap().is_empty());
    let held = tokio::spawn(async move {
        let result = list_pages(&newcomer).await;
        (newcomer, result)
    });
    asked_about(&helper, "newcomer").await;
    assert!(!held.is_finished(), "the call waits for the person");
    helper.agents().answer("newcomer", true);
    let (newcomer, result) = held.await.unwrap();
    let result = result.unwrap();
    assert_ne!(result.is_error, Some(true), "{}", text_of(&result));
    assert!(
        text_of(&result).contains("/sign-in"),
        "{}",
        text_of(&result)
    );
    let now = helper.agents().snapshot();
    assert!(now.asking.is_empty());
    assert_eq!(now.connected, ["newcomer"]);

    // A declined Agent is refused, and not asked about again.
    let stranger = connect_as("stranger", &helper.mcp_url).await.unwrap();
    let held = tokio::spawn(async move {
        let result = list_pages(&stranger).await;
        (stranger, result)
    });
    asked_about(&helper, "stranger").await;
    helper.agents().answer("stranger", false);
    let (stranger, refused) = held.await.unwrap();
    let refused = refused.unwrap();
    assert_eq!(refused.is_error, Some(true));
    assert!(
        text_of(&refused).contains("declined"),
        "{}",
        text_of(&refused)
    );
    let again = list_pages(&stranger).await.unwrap();
    assert_eq!(again.is_error, Some(true));
    assert!(helper.agents().snapshot().asking.is_empty());
    assert_eq!(helper.agents().snapshot().connected, ["newcomer"]);

    stranger.cancel().await.unwrap();
    newcomer.cancel().await.unwrap();
    helper.stop().await;
}

#[tokio::test]
async fn an_allowed_agent_comes_back_without_asking_until_disconnected() {
    let Some(chrome) = test_chrome() else {
        return;
    };
    let site = start_site().await;
    let profile = tempfile::tempdir().unwrap();
    let first_page = format!("{site}/sign-in");
    let config = |chrome| {
        let mut config = helper_config(chrome, profile.path(), &first_page);
        // Short, so a call that asks the person fails the test quickly.
        config.approval_wait = Duration::from_secs(2);
        config
    };
    let helper = start(config(chrome.clone())).await.expect("Helper starts");
    helper.agents().answer("regular", true);
    let regular = connect_as("regular", &helper.mcp_url).await.unwrap();
    let result = list_pages(&regular).await.unwrap();
    assert_ne!(result.is_error, Some(true), "{}", text_of(&result));
    regular.cancel().await.unwrap();

    // A new connection, then a restart: still allowed.
    let regular = connect_as("regular", &helper.mcp_url).await.unwrap();
    let result = list_pages(&regular).await.unwrap();
    assert_ne!(result.is_error, Some(true), "{}", text_of(&result));
    regular.cancel().await.unwrap();
    helper.stop().await;

    let helper = start(config(chrome)).await.expect("Helper starts again");
    let regular = connect_as("regular", &helper.mcp_url).await.unwrap();
    let result = list_pages(&regular).await.unwrap();
    assert_ne!(
        result.is_error,
        Some(true),
        "allowed after a restart: {}",
        text_of(&result)
    );
    assert_eq!(helper.agents().snapshot().connected, ["regular"]);

    // Disconnecting ends the session at once: its next call fails.
    helper.disconnect("regular").await;
    assert!(helper.agents().snapshot().connected.is_empty());
    let after = list_pages(&regular).await;
    assert!(
        after.as_ref().is_err() || after.as_ref().is_ok_and(|r| r.is_error == Some(true)),
        "the disconnected session can't call: {after:?}"
    );

    // It has to be allowed again.
    let back = connect_as("regular", &helper.mcp_url).await.unwrap();
    let asks = list_pages(&back).await.unwrap();
    assert_eq!(asks.is_error, Some(true));
    assert!(
        text_of(&asks).contains("asking the person"),
        "{}",
        text_of(&asks)
    );
    assert_eq!(helper.agents().snapshot().asking, ["regular"]);

    back.cancel().await.unwrap();
    let _ = regular.cancel().await;
    helper.stop().await;
}
