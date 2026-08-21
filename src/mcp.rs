//! MCP transport and protocol integration.
//!
//! This module owns MCP-facing representations without coupling the protocol layer to Codecks API
//! transport details.

use std::fmt;

use crate::error::ApplicationError;

/// A structured, credential-safe error exposed to MCP clients.
///
/// The code and message are static values derived from [`ApplicationError`]. Raw credentials,
/// request headers, response bodies, and upstream error text cannot be attached to this type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct McpError {
    code: &'static str,
    message: &'static str,
}

impl McpError {
    /// Returns the stable machine-readable application error code.
    pub const fn code(self) -> &'static str {
        self.code
    }

    /// Returns the credential-safe message intended for an MCP client.
    pub const fn message(self) -> &'static str {
        self.message
    }
}

impl From<ApplicationError> for McpError {
    fn from(error: ApplicationError) -> Self {
        Self {
            code: error.code(),
            message: error.message(),
        }
    }
}

impl fmt::Display for McpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_ERRORS: [ApplicationError; 11] = [
        ApplicationError::AuthenticationFailed,
        ApplicationError::AuthorizationFailed,
        ApplicationError::ProjectNotFound,
        ApplicationError::ProjectAmbiguous,
        ApplicationError::CardNotFound,
        ApplicationError::CardIdentifierAmbiguous,
        ApplicationError::InvalidIdentifier,
        ApplicationError::Timeout,
        ApplicationError::NetworkFailure,
        ApplicationError::CodecksApiError,
        ApplicationError::InvalidCodecksResponse,
    ];

    #[test]
    fn converts_every_application_error_into_a_structured_mcp_error() {
        for application_error in ALL_ERRORS {
            let mcp_error = McpError::from(application_error);

            assert_eq!(mcp_error.code(), application_error.code());
            assert_eq!(mcp_error.message(), application_error.message());
            assert_eq!(mcp_error.to_string(), application_error.message());
        }
    }

    #[test]
    fn mcp_error_output_never_contains_credentials() {
        const CREDENTIAL_SENTINEL: &str = "credential-sentinel";

        for application_error in ALL_ERRORS {
            let mcp_error = McpError::from(application_error);
            let output = format!("{mcp_error:?}\n{mcp_error}");

            assert!(!output.contains(CREDENTIAL_SENTINEL));
            assert_eq!(mcp_error.message(), application_error.message());
        }
    }
}
