//! Public contract tests for deterministic Codecks project resolution.

use codecks_mcp::domain::Project;
use codecks_mcp::error::ApplicationError;
use codecks_mcp::project_resolver::resolve_project;
use serde_json::json;

fn project(uuid: &str, name: &str) -> Project {
    serde_json::from_value(json!({"id": uuid, "name": name}))
        .expect("the project test fixture should deserialize")
}

#[test]
fn selects_the_only_project_without_an_explicit_selector() {
    let projects = [project("only-project-uuid", "Only Project")];

    let resolved = resolve_project(&projects, None).expect("the only project should be selected");

    assert_eq!(resolved.uuid(), "only-project-uuid");
    assert_eq!(resolved.name(), "Only Project");
}

#[test]
fn reports_project_not_found_when_no_projects_are_available() {
    let error = resolve_project(&[], None).expect_err("an empty project list should fail");

    assert_eq!(error, ApplicationError::ProjectNotFound);
    assert_eq!(error.code(), "project_not_found");
}

#[test]
fn reports_project_ambiguous_for_multiple_projects_without_a_selector() {
    let projects = [
        project("project-a", "Project A"),
        project("project-b", "Project B"),
    ];

    let error = resolve_project(&projects, None)
        .expect_err("multiple projects require an explicit selector");

    assert_eq!(error, ApplicationError::ProjectAmbiguous);
    assert_eq!(error.code(), "project_ambiguous");
}

#[test]
fn resolves_an_explicit_uuid_before_matching_project_names() {
    let projects = [
        project("selected-uuid", "First Project"),
        project("other-uuid", "selected-uuid"),
    ];

    let resolved = resolve_project(&projects, Some("selected-uuid"))
        .expect("an exact UUID should take precedence over a matching display name");

    assert_eq!(resolved.uuid(), "selected-uuid");
    assert_eq!(resolved.name(), "First Project");
}

#[test]
fn resolves_an_explicit_project_name() {
    let projects = [
        project("project-a", "Project A"),
        project("project-b", "Project B"),
    ];

    let resolved = resolve_project(&projects, Some("Project B"))
        .expect("an exact project name should resolve");

    assert_eq!(resolved.uuid(), "project-b");
    assert_eq!(resolved.name(), "Project B");
}

#[test]
fn reports_project_not_found_for_an_unknown_explicit_selector() {
    let projects = [project("known-project", "Known Project")];

    let error = resolve_project(&projects, Some("unknown-project"))
        .expect_err("an unknown explicit selector should fail");

    assert_eq!(error, ApplicationError::ProjectNotFound);
    assert_eq!(error.code(), "project_not_found");
}

#[test]
fn reports_project_ambiguous_for_a_duplicate_explicit_name() {
    let projects = [
        project("project-a", "Shared Name"),
        project("project-b", "Shared Name"),
    ];

    let error = resolve_project(&projects, Some("Shared Name"))
        .expect_err("a duplicate project name should be ambiguous");

    assert_eq!(error, ApplicationError::ProjectAmbiguous);
    assert_eq!(error.code(), "project_ambiguous");
}

#[test]
fn resolves_the_only_project_one_hundred_consecutive_times() {
    let projects = [project("stable-project", "Stable Project")];

    for _ in 0..100 {
        let resolved = resolve_project(&projects, None)
            .expect("single-project resolution should remain stable across repeated calls");

        assert_eq!(resolved.uuid(), "stable-project");
        assert_eq!(resolved.name(), "Stable Project");
    }
}
