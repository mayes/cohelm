//! The Helper only serves Agents on this computer, and starts the browser in
//! Cohelm's own profile.

use cohelm_helper::{BrowserCommand, HelperConfig, HelperError, start};

fn browser() -> BrowserCommand {
    BrowserCommand::new("chromium", std::env::temp_dir().join("cohelm-test-profile"))
}

#[test]
fn loopback_addresses_are_accepted() {
    for addr in ["127.0.0.1:47600", "[::1]:47600"] {
        let config = HelperConfig::new(addr.parse().unwrap(), browser());
        assert!(config.validate().is_ok(), "{addr} should be accepted");
    }
}

#[test]
fn addresses_reachable_from_the_network_are_refused() {
    for addr in ["0.0.0.0:47600", "192.168.1.20:47600", "[::]:47600"] {
        let config = HelperConfig::new(addr.parse().unwrap(), browser());
        assert!(
            matches!(config.validate(), Err(HelperError::NotLoopback(_))),
            "{addr} should be refused"
        );
    }
}

#[test]
fn the_profile_must_be_a_full_path() {
    let config = HelperConfig::new(
        "127.0.0.1:47600".parse().unwrap(),
        BrowserCommand::new("chromium", "profile"),
    );
    assert!(matches!(config.validate(), Err(HelperError::Profile(_))));
}

#[tokio::test]
async fn a_browser_that_wont_start_is_reported() {
    let profile = tempfile::tempdir().unwrap();
    let config = HelperConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        BrowserCommand::new(profile.path().join("no-such-browser"), profile.path()),
    );
    assert!(matches!(
        start(config).await,
        Err(HelperError::BrowserStart(_, _))
    ));
}

#[tokio::test]
async fn a_second_cohelm_finds_the_first_one_running() {
    let first = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let busy = first.local_addr().unwrap();
    let profile = tempfile::tempdir().unwrap();
    let config = HelperConfig::new(
        busy,
        BrowserCommand::new(profile.path().join("no-such-browser"), profile.path()),
    );
    // The port is checked before the browser starts.
    assert!(matches!(
        start(config).await,
        Err(HelperError::AlreadyRunning(addr)) if addr == busy
    ));
}

#[tokio::test]
async fn another_program_on_the_port_is_not_taken_for_cohelm() {
    let other = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = other.local_addr().unwrap();
    // Answers every connection with something that isn't MCP.
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        while let Ok((mut socket, _)) = other.accept().await {
            let _ = socket
                .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n")
                .await;
        }
    });
    assert!(!cohelm_helper::cohelm_at(addr, std::time::Duration::from_secs(2)).await);
}
