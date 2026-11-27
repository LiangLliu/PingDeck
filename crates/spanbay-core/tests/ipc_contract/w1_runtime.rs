//! Wire shapes of the optional fields the AI runtime added (not in the frozen samples).

use serde_json::json;
use spanbay_core::agent::model::{
    AgentMessageMeta, QueuedContent, ReasoningEffort, SkillUndelivered, SkillUndeliveredReason,
};

#[test]
fn the_w1_runtime_fields_have_their_wire_shape() {
    assert_eq!(
        serde_json::to_value(SkillUndelivered {
            name: "review".to_owned(),
            reason: SkillUndeliveredReason::TooLong,
        })
        .unwrap(),
        json!({"name": "review", "reason": "too_long"})
    );
    let meta = AgentMessageMeta {
        served_model: Some("backup/m2".to_owned()),
        ..AgentMessageMeta::default()
    };
    assert_eq!(
        serde_json::to_value(&meta).unwrap()["served_model"],
        "backup/m2"
    );
    assert!(
        serde_json::to_value(AgentMessageMeta::default())
            .unwrap()
            .get("served_model")
            .is_none()
    );
    // The effort of the call lives in `meta_json`: absent when none was sent, and a row stored without it still reads.
    let meta = AgentMessageMeta {
        reasoning_effort: Some(ReasoningEffort::High),
        ..AgentMessageMeta::default()
    };
    assert_eq!(
        serde_json::to_value(&meta).unwrap()["reasoning_effort"],
        "high"
    );
    let old: AgentMessageMeta =
        serde_json::from_value(json!({"served_model": "backup/m2"})).unwrap();
    assert_eq!(old.reasoning_effort, None);
    assert!(
        serde_json::to_value(AgentMessageMeta::default())
            .unwrap()
            .get("reasoning_effort")
            .is_none()
    );
    let content: QueuedContent =
        serde_json::from_value(json!({"parts": [], "skills": ["review"]})).unwrap();
    assert_eq!(content.skills, ["review"]);
}

#[test]
fn a_file_change_names_its_folder_only_when_it_is_known() {
    let mut row: spanbay_core::agent::model::FileChange = serde_json::from_value(json!({
        "id": "fc_1", "session_id": "as_1", "run_id": "run_1", "call_id": "call_1",
        "path": "src/main.rs", "kind": "edit", "before_sha256": null, "after_sha256": null,
        "added": 1, "removed": 0, "status": "pending", "created_at_ms": 1
    }))
    .unwrap();
    assert_eq!(row.folder, None);
    assert!(serde_json::to_value(&row).unwrap().get("folder").is_none());
    row.folder = Some("/work/a".to_owned());
    assert_eq!(serde_json::to_value(&row).unwrap()["folder"], "/work/a");
}
