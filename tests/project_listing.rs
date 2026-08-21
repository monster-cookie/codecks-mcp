//! Public contract tests for Codecks project models.

use codecks_mcp::domain::Project;
use serde_json::json;

#[test]
fn project_model_preserves_codecks_uuid_and_display_name() {
    let project: Project = serde_json::from_value(json!({
        "id": "4228032c-9c42-11f1-b100-7327c739f59b",
        "name": "Current Project Display Name",
    }))
    .expect("a Codecks project entity should deserialize");

    assert_eq!(project.uuid(), "4228032c-9c42-11f1-b100-7327c739f59b");
    assert_eq!(project.name(), "Current Project Display Name");
}

#[test]
fn project_model_accepts_legacy_underscore_id_responses() {
    let project: Project = serde_json::from_value(json!({
        "_id": "legacy-project-uuid",
        "name": "Legacy Project Name",
    }))
    .expect("a legacy Codecks project entity should deserialize");

    assert_eq!(project.uuid(), "legacy-project-uuid");
    assert_eq!(project.name(), "Legacy Project Name");
}
