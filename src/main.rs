//! Executable entry point for the Codecks MCP server.

use std::process::ExitCode;

use codecks_mcp::config::Config;

fn main() -> ExitCode {
    match Config::from_env() {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("configuration error: {error}");
            ExitCode::FAILURE
        }
    }
}
