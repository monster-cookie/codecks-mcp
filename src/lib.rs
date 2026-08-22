//! Reusable configuration, Codecks API, project resolution, and MCP server behavior.
//!
//! The crate exposes credential-safe application boundaries plus the bounded stdio JSON-RPC server
//! used by the `codecks-mcp` executable. MCP discovery currently publishes read-only project
//! listing and deterministic project retrieval tools.

pub mod codecks_api;
pub mod config;
pub mod domain;
pub mod error;
pub mod mcp;
pub mod project_resolver;
