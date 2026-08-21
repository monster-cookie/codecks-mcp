//! Integration tests for the production-facing Codecks HTTP client.

use codecks_mcp::codecks_api::{CODECKS_API_ENDPOINT, CodecksClient};
use codecks_mcp::config::Config;

const TEST_ACCOUNT: &str = "client-test-account";
const TEST_TOKEN: &str = "client-secret-sentinel";

fn config() -> Config {
    Config::from_values([
        ("CODECKS_ACCOUNT", TEST_ACCOUNT),
        ("CODECKS_TOKEN", TEST_TOKEN),
    ])
    .expect("the client test configuration should be valid")
}

#[test]
fn production_client_uses_fixed_codecks_endpoint_without_exposing_credentials() {
    let client = CodecksClient::new(&config()).expect("the production client should build");
    let diagnostics = format!("{client:?}");

    assert!(diagnostics.contains(CODECKS_API_ENDPOINT));
    assert!(diagnostics.contains("REDACTED"));
    assert!(!diagnostics.contains(TEST_ACCOUNT));
    assert!(!diagnostics.contains(TEST_TOKEN));
}
