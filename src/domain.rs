//! Shared domain models.
//!
//! These application concepts are shared by the MCP and Codecks API boundaries.

use serde::Deserialize;

/// A Codecks project available to the authenticated account.
///
/// The UUID is the stable project identity used for repository mappings and later project-scoped
/// API operations. The name is the current display value returned by Codecks and is intentionally
/// preserved without normalization.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Project {
    #[serde(rename = "id", alias = "_id")]
    uuid: String,
    name: String,
}

impl Project {
    /// Returns the stable Codecks project UUID.
    #[must_use]
    pub fn uuid(&self) -> &str {
        &self.uuid
    }

    /// Returns the current Codecks project display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}
