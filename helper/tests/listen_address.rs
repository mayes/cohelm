//! The Helper only serves Agents on this computer.

use cohelm_helper::{HelperConfig, HelperError};

#[test]
fn loopback_addresses_are_accepted() {
    for addr in ["127.0.0.1:47600", "[::1]:47600"] {
        let config = HelperConfig::new(addr.parse().unwrap(), "http://127.0.0.1:9222");
        assert!(config.validate().is_ok(), "{addr} should be accepted");
    }
}

#[test]
fn addresses_reachable_from_the_network_are_refused() {
    for addr in ["0.0.0.0:47600", "192.168.1.20:47600", "[::]:47600"] {
        let config = HelperConfig::new(addr.parse().unwrap(), "http://127.0.0.1:9222");
        assert!(
            matches!(config.validate(), Err(HelperError::NotLoopback(_))),
            "{addr} should be refused"
        );
    }
}

#[test]
fn browser_must_be_on_this_computer() {
    let config = HelperConfig::new("127.0.0.1:47600".parse().unwrap(), "http://10.0.0.5:9222");
    assert!(matches!(
        config.validate(),
        Err(HelperError::BrowserNotLocal(_))
    ));
}
