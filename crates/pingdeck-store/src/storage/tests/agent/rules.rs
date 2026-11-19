//! Permission rules: save and upsert, listing by scope, field validation, narrow Allow rules
//! and delete.

use super::*;

#[test]
fn permission_rules_save_list_by_scope_validate_and_delete() {
    let store = store_with(&["a"]);

    let ws_rule = folder_rule("w", "shell", PermissionAction::Ask);
    let chat_rule = PermissionRule {
        session_id: Some("a".to_owned()),
        arg_pattern: Some("cargo test *".to_owned()),
        created_at_ms: 30,
        ..rule("a1", "shell", PermissionAction::Allow)
    };
    store.save_permission_rule(&ws_rule).unwrap();
    store.save_permission_rule(&chat_rule).unwrap();
    assert_eq!(store.list_permission_rules(Some("a")).unwrap().len(), 2);
    assert_eq!(store.list_permission_rules(None).unwrap().len(), 1);

    for bad_tool in ["", "run shell", "a/b"] {
        assert!(
            store
                .save_permission_rule(&folder_rule("x", bad_tool, PermissionAction::Ask))
                .is_err()
        );
    }
    for arg in [
        String::new(),
        "bad\u{0}arg".to_owned(),
        "line\nbreak".to_owned(),
        "x".repeat(1025),
    ] {
        assert!(
            store
                .save_permission_rule(&PermissionRule {
                    arg_pattern: Some(arg),
                    ..folder_rule("x", "shell", PermissionAction::Ask)
                })
                .is_err()
        );
    }
    for directory in ["relative/dir", "", "./x"] {
        assert!(
            store
                .save_permission_rule(&PermissionRule {
                    workspace_dir: Some(directory.to_owned()),
                    ..rule("x", "shell", PermissionAction::Ask)
                })
                .is_err()
        );
    }
    assert!(
        store
            .save_permission_rule(&PermissionRule {
                session_id: Some("ghost".to_owned()),
                ..rule("x", "shell", PermissionAction::Ask)
            })
            .is_err()
    );
    assert!(
        store
            .connection
            .execute(
                "INSERT INTO agent_permission_rules (id, tool_pattern, action, created_at_ms)
         VALUES ('bad', 't', 'maybe', 1)",
                [],
            )
            .is_err()
    );
    assert!(store.delete_permission_rule("w").unwrap());
    assert!(!store.delete_permission_rule("w").unwrap());
}

#[test]
fn a_rule_needs_a_scope_and_an_allow_rule_a_narrow_argument_pattern() {
    let store = store_with(&["s"]);

    let scoped =
        |tool: &str, arg: Option<&str>, action: PermissionAction, chat: bool| PermissionRule {
            session_id: chat.then(|| "s".to_owned()),
            workspace_dir: (!chat).then(|| "/work".to_owned()),
            arg_pattern: arg.map(str::to_owned),
            ..rule("", tool, action)
        };
    assert!(
        store
            .save_permission_rule(&rule("", "shell", PermissionAction::Block))
            .is_err()
    );
    assert!(
        store
            .save_permission_rule(&scoped("shell", None, PermissionAction::Allow, true))
            .is_err()
    );
    assert!(
        store
            .save_permission_rule(&scoped(
                "shell",
                Some("cargo test *"),
                PermissionAction::Allow,
                true
            ))
            .is_ok()
    );
    assert!(
        store
            .save_permission_rule(&scoped(
                "write_file",
                Some("docs/**"),
                PermissionAction::Allow,
                false
            ))
            .is_ok()
    );
}
