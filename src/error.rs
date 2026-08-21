//! Application error types and reporting.
//!
//! This module provides a credential-safe error boundary shared by the Codecks API and MCP layers
//! without depending on a third-party error framework.

use std::error::Error;
use std::fmt;

/// A failure produced while resolving or communicating with Codecks.
///
/// Variants intentionally carry no raw credentials, request headers, response bodies, or upstream
/// error text. This keeps standard formatting safe for MCP responses and diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationError {
    /// Codecks rejected or could not validate the configured authentication credentials.
    AuthenticationFailed,
    /// The authenticated Codecks user is not permitted to perform the requested operation.
    AuthorizationFailed,
    /// No Codecks project matched the requested identifier.
    ProjectNotFound,
    /// The requested project identifier matched more than one Codecks project.
    ProjectAmbiguous,
    /// No Codecks card matched the requested identifier.
    CardNotFound,
    /// The requested card identifier matched more than one Codecks card.
    CardIdentifierAmbiguous,
    /// An identifier was malformed or unsupported.
    InvalidIdentifier,
    /// A Codecks request exceeded its configured deadline.
    Timeout,
    /// A network failure prevented communication with Codecks.
    NetworkFailure,
    /// Codecks reported an API-level failure.
    CodecksApiError,
    /// Codecks returned a response that could not be validated.
    InvalidCodecksResponse,
}

impl ApplicationError {
    /// Returns the stable machine-readable code used at the MCP boundary.
    pub const fn code(self) -> &'static str {
        match self {
            Self::AuthenticationFailed => "authentication_failed",
            Self::AuthorizationFailed => "authorization_failed",
            Self::ProjectNotFound => "project_not_found",
            Self::ProjectAmbiguous => "project_ambiguous",
            Self::CardNotFound => "card_not_found",
            Self::CardIdentifierAmbiguous => "card_identifier_ambiguous",
            Self::InvalidIdentifier => "invalid_identifier",
            Self::Timeout => "timeout",
            Self::NetworkFailure => "network_failure",
            Self::CodecksApiError => "codecks_api_error",
            Self::InvalidCodecksResponse => "invalid_codecks_response",
        }
    }

    /// Returns a credential-safe message suitable for MCP clients and diagnostics.
    pub const fn message(self) -> &'static str {
        match self {
            Self::AuthenticationFailed => "Codecks authentication failed.",
            Self::AuthorizationFailed => "Codecks authorization failed.",
            Self::ProjectNotFound => "The Codecks project was not found.",
            Self::ProjectAmbiguous => "The Codecks project identifier is ambiguous.",
            Self::CardNotFound => "The Codecks card was not found.",
            Self::CardIdentifierAmbiguous => "The Codecks card identifier is ambiguous.",
            Self::InvalidIdentifier => "The supplied identifier is invalid.",
            Self::Timeout => "The Codecks request timed out.",
            Self::NetworkFailure => "The Codecks network request failed.",
            Self::CodecksApiError => "The Codecks API reported an error.",
            Self::InvalidCodecksResponse => "Codecks returned an invalid response.",
        }
    }
}

impl fmt::Display for ApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl Error for ApplicationError {}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

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
    fn error_codes_are_stable_and_unique() {
        let codes = ALL_ERRORS
            .iter()
            .copied()
            .map(ApplicationError::code)
            .collect::<HashSet<_>>();

        assert_eq!(codes.len(), ALL_ERRORS.len());
        assert!(codes.contains("authentication_failed"));
        assert!(codes.contains("invalid_codecks_response"));
    }

    #[test]
    fn standard_error_output_is_static_and_credential_safe() {
        const CREDENTIAL_SENTINEL: &str = "credential-sentinel";

        for error in ALL_ERRORS {
            let output = format!("{error:?}\n{error}");

            assert!(!output.contains(CREDENTIAL_SENTINEL));
            assert_eq!(error.to_string(), error.message());
        }
    }

    #[test]
    fn implements_standard_error_contract() {
        fn require_error(error: &(dyn Error + 'static)) -> String {
            error.to_string()
        }

        assert_eq!(
            require_error(&ApplicationError::NetworkFailure),
            "The Codecks network request failed."
        );
    }
}
