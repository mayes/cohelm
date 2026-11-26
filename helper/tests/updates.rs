//! A newer Cohelm on GitHub shows as a notice in the Agents list, linking to
//! its notes and download, and nothing is downloaded. Needs a Chromium
//! build, as attach.rs does.

use std::time::Duration;

use axum::{Json, Router, routing::get};
use cohelm_helper::{UpdateCheck, start};
use serde_json::{Value, json};

mod common;
use common::{helper_config, start_site, test_chrome};

const RELEASES: &str = "https://github.com/mayes/cohelm/releases";

async fn start_feed(tag: &'static str) -> String {
    let app = Router::new().route(
        "/releases",
        get(move || async move {
            Json(json!([{
                "tag_name": tag,
                "draft": false,
                "prerelease": false,
                "html_url": format!("{RELEASES}/tag/{tag}"),
                "assets": [],
            }]))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/releases")
}

#[tokio::test]
async fn a_newer_release_shows_in_the_agents_list() {
    let Some(chrome) = test_chrome() else {
        return;
    };
    let site = start_site().await;
    let profile = tempfile::tempdir().unwrap();
    let mut config = helper_config(chrome, profile.path(), &format!("{site}/sign-in"));
    config.updates = Some(UpdateCheck {
        feed: start_feed("v99.0.0").await,
        first: Duration::ZERO,
        every: Duration::from_secs(3600),
        retry: Duration::from_millis(200),
    });
    let helper = start(config).await.expect("Helper starts");

    let probe = helper.probe();
    probe.open_cohelm_page("agents.html").await.unwrap();
    let mut shown = Value::Null;
    for _ in 0..150 {
        shown = probe
            .run_in_cohelm_page(
                "agents.html",
                "document.getElementById('update').hidden ? null : {
                    version: document.getElementById('update-version').textContent,
                    current: document.getElementById('current-version').textContent,
                    notes: document.getElementById('update-notes').href,
                    download: document.getElementById('update-download').href,
                }",
            )
            .await
            .ok()
            .flatten()
            .unwrap_or(Value::Null);
        if !shown.is_null() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let page = format!("{RELEASES}/tag/v99.0.0");
    assert_eq!(
        shown,
        json!({
            "version": "99.0.0",
            "current": env!("CARGO_PKG_VERSION"),
            "notes": page,
            // No download for this computer in the feed: the release page.
            "download": page,
        })
    );
    assert_eq!(helper.update().unwrap().version, "99.0.0");
    helper.stop().await;
}

#[tokio::test]
async fn no_notice_when_this_cohelm_is_current() {
    let Some(chrome) = test_chrome() else {
        return;
    };
    let site = start_site().await;
    let profile = tempfile::tempdir().unwrap();
    let mut config = helper_config(chrome, profile.path(), &format!("{site}/sign-in"));
    let current: &'static str = concat!("v", env!("CARGO_PKG_VERSION"));
    config.updates = Some(UpdateCheck {
        feed: start_feed(current).await,
        first: Duration::ZERO,
        every: Duration::from_secs(3600),
        retry: Duration::from_millis(200),
    });
    let helper = start(config).await.expect("Helper starts");
    let probe = helper.probe();
    probe.open_cohelm_page("agents.html").await.unwrap();
    // Wait for the page to show the lists, then give the check time to run.
    let mut ready = false;
    for _ in 0..150 {
        ready = probe
            .run_in_cohelm_page("agents.html", "!document.getElementById('lists').hidden")
            .await
            .ok()
            .flatten()
            == Some(json!(true));
        if ready {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ready, "the Agents list shows");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let hidden = probe
        .run_in_cohelm_page("agents.html", "document.getElementById('update').hidden")
        .await
        .unwrap();
    assert_eq!(hidden, Some(json!(true)));
    assert_eq!(helper.update(), None);
    helper.stop().await;
}
