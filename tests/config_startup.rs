//! Process-level tests for configuration validation during executable startup.

use std::process::{Command, Output};

const ACCOUNT_VARIABLE: &str = "CODECKS_ACCOUNT";
const TOKEN_VARIABLE: &str = "CODECKS_TOKEN";
const TIMEOUT_VARIABLE: &str = "CODECKS_TIMEOUT_SECONDS";
const LOG_LEVEL_VARIABLE: &str = "CODECKS_LOG_LEVEL";
const TEST_ACCOUNT: &str = "startup-test-account";
const TEST_TOKEN: &str = "startup-secret-sentinel";

fn server_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_codecks-mcp"));
    for variable in [
        ACCOUNT_VARIABLE,
        TOKEN_VARIABLE,
        TIMEOUT_VARIABLE,
        LOG_LEVEL_VARIABLE,
    ] {
        command.env_remove(variable);
    }
    command
}

fn run_with_required_values() -> Output {
    server_command()
        .env(ACCOUNT_VARIABLE, TEST_ACCOUNT)
        .env(TOKEN_VARIABLE, TEST_TOKEN)
        .output()
        .expect("the Codecks MCP executable should start")
}

#[test]
fn valid_configuration_starts_without_stdout_output() {
    let output = run_with_required_values();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn missing_required_values_fail_with_actionable_diagnostics() {
    for (missing_variable, configured_variable, configured_value) in [
        (ACCOUNT_VARIABLE, TOKEN_VARIABLE, TEST_TOKEN),
        (TOKEN_VARIABLE, ACCOUNT_VARIABLE, TEST_ACCOUNT),
    ] {
        let output = server_command()
            .env(configured_variable, configured_value)
            .output()
            .expect("the Codecks MCP executable should report invalid configuration");
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(stderr.contains(missing_variable), "stderr was: {stderr}");
        assert!(
            !stderr.contains(TEST_TOKEN),
            "stderr exposed the test token"
        );
    }
}

#[test]
fn invalid_optional_value_fails_without_exposing_secret() {
    let output = server_command()
        .env(ACCOUNT_VARIABLE, TEST_ACCOUNT)
        .env(TOKEN_VARIABLE, TEST_TOKEN)
        .env(TIMEOUT_VARIABLE, "zero")
        .output()
        .expect("the Codecks MCP executable should report invalid configuration");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(stderr.contains(TIMEOUT_VARIABLE), "stderr was: {stderr}");
    assert!(
        !stderr.contains(TEST_TOKEN),
        "stderr exposed the test token"
    );
}
