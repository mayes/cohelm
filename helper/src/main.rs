use std::net::SocketAddr;

use cohelm_helper::{DevtoolsCommand, HelperConfig, start};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let listen: SocketAddr = std::env::var("COHELM_HELPER_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:47600".into())
        .parse()?;
    let browser_url =
        std::env::var("COHELM_BROWSER_URL").unwrap_or_else(|_| "http://127.0.0.1:9222".into());
    let mut config = HelperConfig::new(listen, browser_url);
    if let Ok(script) = std::env::var("COHELM_DEVTOOLS_MCP") {
        config.devtools = DevtoolsCommand::node_script(script);
    }

    let helper = start(config).await?;
    eprintln!(
        "Cohelm Helper is ready. Connect your MCP client to {}",
        helper.mcp_url
    );
    tokio::signal::ctrl_c().await?;
    helper.stop().await;
    Ok(())
}
