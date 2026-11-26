//! Shared pieces of the browser tests: a stand-in site and an Agent's calls.

#![allow(dead_code)]

use axum::{
    Router,
    http::{HeaderMap, header},
    response::IntoResponse,
    routing::get,
};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value};

/// A page of Cohelm's built-in extension. Its ID comes from the key in
/// extension/manifest.json.
pub const EXTENSION_PAGE: &str = "chrome-extension://dojelhkgldmdbffaafmofoilmbbacppo/about.html";

pub type Agent = rmcp::service::RunningService<rmcp::RoleClient, ()>;

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
pub async fn start_site() -> String {
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
pub fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn args(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap()
}

/// Calls a tool and fails the test if the tool reports an error.
pub async fn call(agent: &Agent, tool: &'static str, arguments: Value) -> String {
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
