//! Core library boundaries for the Codecks MCP server.
//!
//! The modules are intentionally minimal while the workspace is bootstrapped. Protocol and API
//! behavior will be added by focused follow-up work.

pub mod codecks_api;
pub mod config;
pub mod domain;
pub mod error;
pub mod mcp;
