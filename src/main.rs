//! Executable entry point for the Codecks MCP server.

use std::process::ExitCode;

use codecks_mcp::codecks_api::CodecksClient;
use codecks_mcp::config::Config;
use codecks_mcp::mcp::{McpServer, run_stdio};

#[tokio::main]
async fn main() -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("configuration error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let client = match CodecksClient::new(&config) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("Codecks client error: {error}");
            return ExitCode::FAILURE;
        }
    };

    match run_stdio(McpServer::new(client)).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("MCP transport error: {error}");
            ExitCode::FAILURE
        }
    }
}
