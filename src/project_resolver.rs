//! Deterministic Codecks project resolution.
//!
//! This module keeps project selection rules independent from transport and MCP protocol details.

use crate::domain::Project;
use crate::error::ApplicationError;

/// Resolves a project from the available projects and an optional explicit selector.
///
/// Explicit selectors are matched against stable project UUIDs before exact display names. Without
/// a selector, the sole available project is selected automatically. Empty or unmatched results
/// return [`ApplicationError::ProjectNotFound`], while non-unique selections return
/// [`ApplicationError::ProjectAmbiguous`].
///
/// # Errors
///
/// Returns [`ApplicationError::ProjectNotFound`] when no project can be selected, or
/// [`ApplicationError::ProjectAmbiguous`] when automatic selection or name matching is not unique.
pub fn resolve_project<'projects>(
    projects: &'projects [Project],
    selector: Option<&str>,
) -> Result<&'projects Project, ApplicationError> {
    let Some(selector) = selector else {
        return match projects {
            [project] => Ok(project),
            [] => Err(ApplicationError::ProjectNotFound),
            _ => Err(ApplicationError::ProjectAmbiguous),
        };
    };

    if let Some(project) = projects.iter().find(|project| project.uuid() == selector) {
        return Ok(project);
    }

    let mut name_matches = projects.iter().filter(|project| project.name() == selector);
    let project = name_matches
        .next()
        .ok_or(ApplicationError::ProjectNotFound)?;

    if name_matches.next().is_some() {
        Err(ApplicationError::ProjectAmbiguous)
    } else {
        Ok(project)
    }
}
