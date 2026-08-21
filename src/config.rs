//! Application configuration.
//!
//! This module will own configuration loading and validation without exposing credential values
//! outside the configuration boundary.

use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::time::Duration;

const ACCOUNT_VARIABLE: &str = "CODECKS_ACCOUNT";
const TOKEN_VARIABLE: &str = "CODECKS_TOKEN";
const TIMEOUT_VARIABLE: &str = "CODECKS_TIMEOUT_SECONDS";
const LOG_LEVEL_VARIABLE: &str = "CODECKS_LOG_LEVEL";
const DEFAULT_TIMEOUT_SECONDS: u64 = 30;

/// Runtime configuration required to connect the MCP server to Codecks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    account: String,
    authentication_token: Secret,
    request_timeout: Duration,
    log_level: LogLevel,
}

impl Config {
    /// Loads and validates configuration from the current process environment.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when a required variable is missing or empty, when a value is not
    /// valid Unicode, or when an optional value cannot be parsed.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(env::var_os)
    }

    /// Loads and validates configuration from supplied name-value pairs.
    ///
    /// This entry point supports tests and embedding scenarios without reading or mutating the
    /// process environment. When a variable occurs more than once, the last value is used.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] under the same conditions as [`Config::from_env`].
    pub fn from_values<I, K, V>(values: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        let values = values
            .into_iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect::<HashMap<_, _>>();

        Self::from_lookup(|variable| values.get(OsStr::new(variable)).cloned())
    }

    /// Returns the Codecks account name or subdomain.
    pub fn account(&self) -> &str {
        &self.account
    }

    /// Returns the authentication token through an explicitly secret-bearing value.
    pub fn authentication_token(&self) -> &Secret {
        &self.authentication_token
    }

    /// Returns the maximum duration allowed for a Codecks request.
    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    /// Returns the configured diagnostic logging level.
    pub fn log_level(&self) -> LogLevel {
        self.log_level
    }

    fn from_lookup<F>(lookup: F) -> Result<Self, ConfigError>
    where
        F: Fn(&'static str) -> Option<OsString>,
    {
        let account = required_value(&lookup, ACCOUNT_VARIABLE)?;
        let authentication_token = Secret(required_value(&lookup, TOKEN_VARIABLE)?);
        let request_timeout = optional_value(&lookup, TIMEOUT_VARIABLE)?
            .map(|value| parse_timeout(&value))
            .transpose()?
            .unwrap_or_else(|| Duration::from_secs(DEFAULT_TIMEOUT_SECONDS));
        let log_level = optional_value(&lookup, LOG_LEVEL_VARIABLE)?
            .map(|value| parse_log_level(&value))
            .transpose()?
            .unwrap_or_default();

        Ok(Self {
            account,
            authentication_token,
            request_timeout,
            log_level,
        })
    }
}

/// A credential value whose standard formatting is always redacted.
#[derive(Clone, Eq, PartialEq)]
pub struct Secret(String);

impl Secret {
    /// Exposes the credential for the narrow scope that must authenticate a request.
    ///
    /// Callers must not log, format, or include the returned value in diagnostics.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([REDACTED])")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

/// Diagnostic logging levels accepted by the configuration loader.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LogLevel {
    /// Emit only error diagnostics.
    Error,
    /// Emit warning and error diagnostics.
    Warn,
    /// Emit informational, warning, and error diagnostics.
    #[default]
    Info,
    /// Emit debug diagnostics in addition to higher-severity messages.
    Debug,
    /// Emit all available diagnostics.
    Trace,
}

/// Describes why runtime configuration could not be loaded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigError {
    /// A required environment variable was not provided.
    MissingVariable {
        /// The name of the missing variable.
        variable: &'static str,
    },
    /// A required environment variable contained only whitespace.
    EmptyVariable {
        /// The name of the empty variable.
        variable: &'static str,
    },
    /// An environment variable could not be represented as Unicode text.
    NonUnicodeVariable {
        /// The name of the non-Unicode variable.
        variable: &'static str,
    },
    /// The configured request timeout was not a positive integer.
    InvalidTimeout {
        /// The name of the timeout variable.
        variable: &'static str,
    },
    /// The configured logging level was not recognized.
    InvalidLogLevel {
        /// The name of the logging-level variable.
        variable: &'static str,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingVariable { variable } => {
                write!(
                    formatter,
                    "required environment variable {variable} is missing"
                )
            }
            Self::EmptyVariable { variable } => write!(
                formatter,
                "required environment variable {variable} must not be empty"
            ),
            Self::NonUnicodeVariable { variable } => write!(
                formatter,
                "environment variable {variable} must contain valid Unicode text"
            ),
            Self::InvalidTimeout { variable } => write!(
                formatter,
                "environment variable {variable} must be a positive integer number of seconds"
            ),
            Self::InvalidLogLevel { variable } => write!(
                formatter,
                "environment variable {variable} must be one of: error, warn, info, debug, trace"
            ),
        }
    }
}

impl Error for ConfigError {}

fn required_value<F>(lookup: &F, variable: &'static str) -> Result<String, ConfigError>
where
    F: Fn(&'static str) -> Option<OsString>,
{
    let value =
        optional_value(lookup, variable)?.ok_or(ConfigError::MissingVariable { variable })?;

    if value.trim().is_empty() {
        return Err(ConfigError::EmptyVariable { variable });
    }

    Ok(value)
}

fn optional_value<F>(lookup: &F, variable: &'static str) -> Result<Option<String>, ConfigError>
where
    F: Fn(&'static str) -> Option<OsString>,
{
    lookup(variable)
        .map(|value| {
            value
                .into_string()
                .map_err(|_| ConfigError::NonUnicodeVariable { variable })
        })
        .transpose()
}

fn parse_timeout(value: &str) -> Result<Duration, ConfigError> {
    let seconds = value
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|seconds| *seconds > 0)
        .ok_or(ConfigError::InvalidTimeout {
            variable: TIMEOUT_VARIABLE,
        })?;

    Ok(Duration::from_secs(seconds))
}

fn parse_log_level(value: &str) -> Result<LogLevel, ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "error" => Ok(LogLevel::Error),
        "warn" => Ok(LogLevel::Warn),
        "info" => Ok(LogLevel::Info),
        "debug" => Ok(LogLevel::Debug),
        "trace" => Ok(LogLevel::Trace),
        _ => Err(ConfigError::InvalidLogLevel {
            variable: LOG_LEVEL_VARIABLE,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_ACCOUNT: &str = "test-account";
    const TEST_TOKEN: &str = "secret-token-sentinel";

    fn required_values() -> [(&'static str, &'static str); 2] {
        [
            (ACCOUNT_VARIABLE, TEST_ACCOUNT),
            (TOKEN_VARIABLE, TEST_TOKEN),
        ]
    }

    #[test]
    fn loads_valid_configuration() {
        let config = Config::from_values([
            (ACCOUNT_VARIABLE, TEST_ACCOUNT),
            (TOKEN_VARIABLE, TEST_TOKEN),
            (TIMEOUT_VARIABLE, "45"),
            (LOG_LEVEL_VARIABLE, "debug"),
        ])
        .expect("valid configuration should load");

        assert_eq!(config.account(), TEST_ACCOUNT);
        assert_eq!(config.authentication_token().expose(), TEST_TOKEN);
        assert_eq!(config.request_timeout(), Duration::from_secs(45));
        assert_eq!(config.log_level(), LogLevel::Debug);
    }

    #[test]
    fn applies_optional_defaults() {
        let config = Config::from_values(required_values())
            .expect("required values should be enough to load configuration");

        assert_eq!(
            config.request_timeout(),
            Duration::from_secs(DEFAULT_TIMEOUT_SECONDS)
        );
        assert_eq!(config.log_level(), LogLevel::Info);
    }

    #[test]
    fn reports_missing_required_variables() {
        let missing_account_error = Config::from_values([(TOKEN_VARIABLE, TEST_TOKEN)])
            .expect_err("a missing account must fail");

        assert_eq!(
            missing_account_error,
            ConfigError::MissingVariable {
                variable: ACCOUNT_VARIABLE
            }
        );
        assert!(missing_account_error.to_string().contains(ACCOUNT_VARIABLE));

        let missing_token_error = Config::from_values([(ACCOUNT_VARIABLE, TEST_ACCOUNT)])
            .expect_err("a missing token must fail");

        assert_eq!(
            missing_token_error,
            ConfigError::MissingVariable {
                variable: TOKEN_VARIABLE
            }
        );
        assert!(missing_token_error.to_string().contains(TOKEN_VARIABLE));
    }

    #[test]
    fn reports_empty_required_variable() {
        let error = Config::from_values([(ACCOUNT_VARIABLE, "  "), (TOKEN_VARIABLE, TEST_TOKEN)])
            .expect_err("an empty account must fail");

        assert_eq!(
            error,
            ConfigError::EmptyVariable {
                variable: ACCOUNT_VARIABLE
            }
        );
    }

    #[test]
    fn rejects_invalid_timeout() {
        for invalid_timeout in ["", "0", "not-a-number"] {
            let error = Config::from_values([
                (ACCOUNT_VARIABLE, TEST_ACCOUNT),
                (TOKEN_VARIABLE, TEST_TOKEN),
                (TIMEOUT_VARIABLE, invalid_timeout),
            ])
            .expect_err("an invalid timeout must fail");

            assert_eq!(
                error,
                ConfigError::InvalidTimeout {
                    variable: TIMEOUT_VARIABLE
                }
            );
        }
    }

    #[test]
    fn accepts_case_insensitive_log_level() {
        let config = Config::from_values([
            (ACCOUNT_VARIABLE, TEST_ACCOUNT),
            (TOKEN_VARIABLE, TEST_TOKEN),
            (LOG_LEVEL_VARIABLE, "TRACE"),
        ])
        .expect("a case-insensitive log level should load");

        assert_eq!(config.log_level(), LogLevel::Trace);
    }

    #[test]
    fn rejects_invalid_log_level() {
        let error = Config::from_values([
            (ACCOUNT_VARIABLE, TEST_ACCOUNT),
            (TOKEN_VARIABLE, TEST_TOKEN),
            (LOG_LEVEL_VARIABLE, "verbose"),
        ])
        .expect_err("an unsupported log level must fail");

        assert_eq!(
            error,
            ConfigError::InvalidLogLevel {
                variable: LOG_LEVEL_VARIABLE
            }
        );
    }

    #[test]
    fn redacts_secret_from_standard_formatting() {
        let config = Config::from_values(required_values())
            .expect("valid configuration should load for formatting checks");
        let config_debug = format!("{config:?}");
        let secret_debug = format!("{:?}", config.authentication_token());
        let secret_display = config.authentication_token().to_string();

        for output in [config_debug, secret_debug, secret_display] {
            assert!(!output.contains(TEST_TOKEN));
            assert!(output.contains("REDACTED"));
        }
    }

    #[test]
    fn errors_never_include_other_configuration_values() {
        let error = Config::from_values([
            (ACCOUNT_VARIABLE, TEST_ACCOUNT),
            (TOKEN_VARIABLE, TEST_TOKEN),
            (TIMEOUT_VARIABLE, "invalid"),
        ])
        .expect_err("the invalid timeout should fail");
        let diagnostics = format!("{error:?}\n{error}");

        assert!(!diagnostics.contains(TEST_ACCOUNT));
        assert!(!diagnostics.contains(TEST_TOKEN));
        assert!(diagnostics.contains(TIMEOUT_VARIABLE));
    }
}
