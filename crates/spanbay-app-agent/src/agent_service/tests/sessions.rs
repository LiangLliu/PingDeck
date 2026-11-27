//! Session use-case tests: create, get, list, update, tombstone delete and restore, search,
//! export, and the message-tree edits.

use super::*;
use crate::agent::model::{
    AgentRunKind, AgentRunStatus, AgentRunSummary, AgentSessionDetail, AgentSessionFilter,
    AgentTodoItem, AgentTodoStatus, ReasoningEffort,
};
use crate::agent_service::{
    agent_message_prepare_edit_impl, agent_message_prepare_retry_impl, agent_session_branch_impl,
    agent_session_delete_message_impl, agent_session_delete_message_preview_impl,
    agent_session_export_preview_impl, agent_session_export_redacted_impl,
    agent_session_restore_impl, agent_session_search_impl, agent_session_set_head_impl,
};
use crate::llm::hub::{HubCitation, HubDocument, HubDocumentSource, HubImage, HubUsage};
use crate::settings::ToolApproval;

fn run_row(id: &str, session_id: &str, status: AgentRunStatus) -> AgentRunSummary {
    AgentRunSummary {
        run_id: id.to_owned(),
        session_id: session_id.to_owned(),
        kind: AgentRunKind::Main,
        parent_run_id: None,
        parent_call_id: None,
        status,
        model_label: "m".to_owned(),
        target: None,
        iterations: 0,
        usage: HubUsage::default(),
        cost_micro_usd: 0,
        started_at_ms: 1,
        finished_at_ms: None,
        last_event_ms: None,
        last_step: None,
        stop_reason: None,
        error_kind: None,
        error: None,
        last_seq: 0,
        failure: None,
        role: None,
        cost_currency: None,
        cap_inapplicable: None,
        status_detail: None,
    }
}

fn get(state: &AppState, id: &str) -> AgentSessionDetail {
    agent_session_get_impl(state, id, AgentPageRequest::default()).expect("get")
}

fn todo(id: &str, content: &str, status: AgentTodoStatus) -> AgentTodoItem {
    AgentTodoItem {
        id: id.to_owned(),
        content: content.to_owned(),
        status,
        ..AgentTodoItem::default()
    }
}

fn text_part(text: &str) -> Vec<HubPart> {
    vec![HubPart::text(text)]
}

fn put_attachment(state: &AppState, attachment_id: &str, name: &str) {
    use crate::agent::model::{AgentAttachment, AttachmentKind};
    state
        .store()
        .insert_attachment(&AgentAttachment {
            id: attachment_id.to_owned(),
            sha256: format!("sha-{attachment_id}"),
            name: name.to_owned(),
            media_type: "image/png".to_owned(),
            size_bytes: 12,
            kind: AttachmentKind::Image,
            width: Some(4),
            height: Some(3),
            thumb_data_url: None,
            removed: false,
            lines: None,
            created_at_ms: 7,
        })
        .expect("attachment");
}

fn image_ref(attachment_id: &str) -> HubPart {
    HubPart::Image {
        image: HubImage::AttachmentRef {
            attachment_id: attachment_id.to_owned(),
            media_type: "image/png".to_owned(),
        },
    }
}

fn chat(state: &AppState, title: &str) -> AgentSession {
    let made = session(state, title);
    for (id, role, text, at) in [
        ("u1", AgentRole::User, "first question", 1),
        ("a1", AgentRole::Assistant, "first answer", 2),
        ("u2", AgentRole::User, "second question", 3),
        ("a2", AgentRole::Assistant, "second answer", 4),
    ] {
        append(state, &message(&made.id, id, role, text, at));
    }
    made
}

fn tool_message_ids(detail: &AgentSessionDetail) -> Vec<&str> {
    detail.messages.iter().map(|m| m.id.as_str()).collect()
}

#[test]
fn create_and_update_apply_defaults_overrides_workspaces_and_validations() {
    let state = state();
    let root = TempDir::new("ws-session");
    let canonical =
        crate::platform::normalize_display_path(&fs::canonicalize(root.path()).expect("c"));
    settings_update_impl(
        &state,
        SettingsSection::Ai,
        &json!({"agent": {
            "default_target": {"kind": "named", "name": "fast"},
            "default_effort": "high",
            "default_permission_mode": "ask",
            "default_workspace": root.path().to_string_lossy(),
        }}),
    )
    .expect("settings");

    let defaulted = agent_session_create_impl(&state, AgentSessionInput::default()).expect("ok");
    assert_eq!(defaulted.title, "New chat");
    assert!(!defaulted.title_locked);
    assert_eq!(defaulted.permission_mode, ToolApproval::Ask);
    assert_eq!(defaulted.reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(defaulted.workspace_dir.as_deref(), Some(canonical.as_str()));

    let custom = agent_session_create_impl(
        &state,
        AgentSessionInput {
            title: Some("  Spaced \n title  ".to_owned()),
            workspace_dir: Some("   ".to_owned()),
            permission_mode: Some(ToolApproval::AutoAll),
            instructions: Some("Answer in French.".to_owned()),
            web_enabled: Some(true),
            ..AgentSessionInput::default()
        },
    )
    .expect("ok");
    assert_eq!(custom.title, "Spaced title");
    assert!(custom.title_locked && custom.workspace_dir.is_none());
    assert_eq!(custom.instructions.as_deref(), Some("Answer in French."));

    for bad_ws in ["relative/dir", "missing/path"] {
        assert!(matches!(
            agent_session_create_impl(
                &state,
                AgentSessionInput {
                    workspace_dir: Some(bad_ws.to_owned()),
                    ..AgentSessionInput::default()
                }
            ),
            Err(AppError::Validation { .. })
        ));
    }

    let todos = vec![todo("1", "write tests", AgentTodoStatus::Completed)];
    let updated = agent_session_update_impl(
        &state,
        &custom.id,
        AgentSessionPatch {
            title: Some("Renamed".to_owned()),
            workspace_dir: Some(root.path().to_string_lossy().into_owned()),
            todos: Some(todos.clone()),
            instructions: Some(String::new()),
            reasoning_effort: Some(None),
            pinned: Some(true),
            archived: Some(true),
            ..AgentSessionPatch::default()
        },
    )
    .expect("updates");
    assert_eq!(updated.workspace_dir.as_deref(), Some(canonical.as_str()));
    assert_eq!(updated.todos, todos);
    assert_eq!(updated.instructions, None);
    assert_eq!(updated.reasoning_effort, None);
    assert!(updated.pinned_at_ms.is_some() && updated.archived_at_ms.is_some());

    // Duplicate todo IDs are rejected.
    assert!(matches!(
        agent_session_update_impl(
            &state,
            &custom.id,
            AgentSessionPatch {
                todos: Some(vec![
                    todo("1", "a", AgentTodoStatus::Pending),
                    todo("1", "b", AgentTodoStatus::Pending),
                ]),
                ..AgentSessionPatch::default()
            }
        ),
        Err(AppError::Validation { .. })
    ));
    assert!(matches!(
        agent_session_get_impl(&state, "missing", AgentPageRequest::default()),
        Err(AppError::NotFound { .. })
    ));
}

#[test]
fn list_search_and_get_page_turns_live_run_and_attachments() {
    use crate::agent::model::AgentEvent;
    use crate::agent::registry::ActiveRun;
    use crate::cancel::CancelToken;

    let state = state();
    let made = session(&state, "Long");
    put_attachment(&state, "att1", "one.png");
    let mut first = message(&made.id, "u1", AgentRole::User, "deploy the service", 1);
    first.parts.push(image_ref("att1"));
    append(&state, &first);
    append(
        &state,
        &message(&made.id, "a1", AgentRole::Assistant, "r", 1),
    );
    for i in 2..=55 {
        append(
            &state,
            &message(&made.id, &format!("u{i}"), AgentRole::User, "q", i),
        );
        append(
            &state,
            &message(&made.id, &format!("a{i}"), AgentRole::Assistant, "r", i),
        );
    }
    state
        .store()
        .insert_agent_run(&run_row("live", &made.id, AgentRunStatus::WaitingApproval))
        .expect("run");

    let hits = agent_session_search_impl(&state, "deploy", None).expect("search");
    assert_eq!(hits[0].message_id, "u1");
    assert!(matches!(
        agent_session_list_impl(&state, None, None, Some("junk".to_owned())),
        Err(AppError::Validation { .. })
    ));

    let newest = get(&state, &made.id);
    assert_eq!(newest.messages.len(), 100);
    assert!(newest.has_more);
    assert_eq!(newest.runs[0].run_id, "live");

    let older = agent_session_get_impl(
        &state,
        &made.id,
        AgentPageRequest {
            before_seq: Some(newest.messages[0].seq),
            limit: None,
            ..AgentPageRequest::default()
        },
    )
    .expect("older");
    assert_eq!(older.messages[0].attachments[0].name, "one.png");

    let run = ActiveRun::new(
        run_row("main", &made.id, AgentRunStatus::Running),
        CancelToken::new(),
    );
    state.agent.runs.register(&run).expect("register");
    run.emit(AgentEvent::TextDelta {
        text: "so far".into(),
    });
    assert_eq!(get(&state, &made.id).live.expect("live").run_id, "main");
    state.agent.runs.finish("main");
}

#[test]
fn delete_is_a_tombstone_until_restored_or_purged() {
    let state = state();
    let made = chat(&state, "Doomed");
    agent_session_delete_impl(&state, &made.id).expect("deletes");
    assert!(matches!(
        agent_session_get_impl(&state, &made.id, AgentPageRequest::default()),
        Err(AppError::NotFound { .. })
    ));
    assert!(
        agent_session_list_impl(&state, Some(AgentSessionFilter::default()), None, None)
            .unwrap()
            .sessions
            .is_empty()
    );

    let back = agent_session_restore_impl(&state, &made.id).expect("restores");
    assert_eq!(back.deleted_at_ms, None);
    assert_eq!(get(&state, &made.id).messages.len(), 4);

    agent_session_delete_impl(&state, &made.id).expect("deletes again");
    state
        .store()
        .purge_deleted_sessions(i64::MAX)
        .expect("purge");
    assert!(matches!(
        agent_session_restore_impl(&state, &made.id),
        Err(AppError::NotFound { .. })
    ));
}

#[test]
fn tree_edits_switch_head_delete_subtrees_and_branch_chats() {
    let state = state();
    let made = chat(&state, "Branches");
    agent_session_update_impl(
        &state,
        &made.id,
        AgentSessionPatch {
            permission_mode: Some(ToolApproval::AutoAll),
            ..AgentSessionPatch::default()
        },
    )
    .expect("auto all");
    agent_message_prepare_retry_impl(&state, &made.id, "a2").expect("retry");
    append(
        &state,
        &message(&made.id, "a2b", AgentRole::Assistant, "alt", 9),
    );

    let detail = agent_session_set_head_impl(&state, &made.id, "a2").expect("switches");
    assert_eq!(tool_message_ids(&detail), ["u1", "a1", "u2", "a2"]);
    assert_eq!(detail.messages[3].branch_siblings, ["a2", "a2b"]);

    // Branching tones down AutoAll -> AutoReadOnly and copies up to target message.
    let branch = agent_session_branch_impl(&state, &made.id, "a1", None).expect("branches");
    assert_eq!(branch.title, "Branches");
    assert_eq!(branch.forked_from.as_deref(), Some(made.id.as_str()));
    assert_eq!(branch.permission_mode, ToolApproval::AutoReadOnly);
    assert_eq!(get(&state, &branch.id).messages.len(), 2);
    // A branch of a branch keeps the title too; the list row says which chat is a branch.
    let tip = get(&state, &branch.id).messages[1].id.clone();
    let again = agent_session_branch_impl(&state, &branch.id, &tip, None).expect("branches");
    assert_eq!(again.title, "Branches");
    let row = |id: &str| {
        agent_session_list_impl(&state, None, None, None)
            .expect("lists")
            .sessions
            .into_iter()
            .find(|s| s.id == id)
            .expect("listed")
    };
    assert_eq!(
        row(&again.id).forked_from.as_deref(),
        Some(branch.id.as_str())
    );
    assert_eq!(row(&made.id).forked_from, None);
    // Titles older branches were given lose their stacked suffixes.
    assert_eq!(
        super::super::sessions::branch_title("Why? (branch) (branch)"),
        "Why?"
    );
    assert_eq!(
        super::super::sessions::branch_title(" (branch)"),
        " (branch)"
    );

    let preview =
        agent_session_delete_message_preview_impl(&state, &made.id, "u2").expect("preview");
    assert_eq!(preview.messages, 3);
    let after_del = agent_session_delete_message_impl(&state, &made.id, "u2").expect("deletes");
    assert_eq!(tool_message_ids(&after_del), ["u1", "a1"]);

    // Active run blocks head moves and message deletion.
    state
        .store()
        .insert_agent_run(&run_row("live", &made.id, AgentRunStatus::Running))
        .expect("run");
    assert!(matches!(
        agent_session_set_head_impl(&state, &made.id, "u1"),
        Err(AppError::Conflict { .. })
    ));
    assert!(matches!(
        agent_session_delete_message_impl(&state, &made.id, "u1"),
        Err(AppError::Conflict { .. })
    ));
}

#[test]
fn prepare_edit_and_retry_branch_messages_and_validate_parts() {
    let state = state();
    let made = chat(&state, "Editable");
    put_attachment(&state, "att", "pic.png");
    let mut original = message(&made.id, "u3", AgentRole::User, "look at this", 5);
    original.parts.push(image_ref("att"));
    original.meta.skills = vec!["pdf".to_owned()];
    append(&state, &original);
    append(
        &state,
        &message(&made.id, "a3", AgentRole::Assistant, "seen", 6),
    );

    let edited = agent_message_prepare_edit_impl(
        &state,
        &made.id,
        "u3",
        vec![HubPart::text("look at that"), image_ref("att")],
    )
    .expect("edits");
    assert_eq!(edited.parent_id.as_deref(), Some("a2"));
    assert_eq!(edited.branch_siblings, ["u3", edited.id.as_str()]);
    assert_eq!(edited.meta.skills, ["pdf"]);
    assert_eq!(edited.attachments.len(), 1);

    for bad_parts in [
        vec![],
        text_part("   "),
        vec![HubPart::Document {
            document: HubDocument {
                media_type: "text/plain".to_owned(),
                name: None,
                source: HubDocumentSource::Text {
                    text: "x".to_owned(),
                },
            },
        }],
    ] {
        assert!(matches!(
            agent_message_prepare_edit_impl(&state, &made.id, "u2", bad_parts),
            Err(AppError::Validation { .. })
        ));
    }
    assert!(matches!(
        agent_message_prepare_edit_impl(&state, &made.id, "a2", text_part("x")),
        Err(AppError::Validation { .. })
    ));
}

#[test]
fn export_renders_markdown_of_active_path_and_json_of_full_tree() {
    let state = state();
    let made = session(&state, "Export me");
    put_attachment(&state, "att", "diagram.png");
    let mut question = message(&made.id, "u1", AgentRole::User, "Describe the diagram", 1);
    question.parts.push(image_ref("att"));
    append(&state, &question);
    let mut answer = message(&made.id, "a1", AgentRole::Assistant, "It shows a graph.", 2);
    if let HubPart::Text { citations, .. } = &mut answer.parts[0] {
        citations.push(HubCitation {
            url: "https://example.test/src".to_owned(),
            title: "Source".to_owned(),
            snippet: None,
            start_index: None,
            end_index: None,
        });
    }
    append(&state, &answer);
    agent_message_prepare_retry_impl(&state, &made.id, "a1").expect("retry");
    append(
        &state,
        &message(&made.id, "a1b", AgentRole::Assistant, "Second opinion", 9),
    );
    agent_session_update_impl(
        &state,
        &made.id,
        AgentSessionPatch {
            todos: Some(vec![todo("1", "done thing", AgentTodoStatus::Completed)]),
            ..AgentSessionPatch::default()
        },
    )
    .expect("todos");

    let markdown =
        agent_session_export_impl(&state, &made.id, AgentExportFormat::Markdown).expect("md");
    assert!(markdown.contains("Describe the diagram") && markdown.contains("Second opinion"));
    assert!(!markdown.contains("It shows a graph") && markdown.contains("- [x] done thing"));

    let json = agent_session_export_impl(&state, &made.id, AgentExportFormat::Json).expect("json");
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
    assert_eq!(value["active_path"], json!(["u1", "a1b"]));
    assert_eq!(value["messages"].as_array().unwrap().len(), 3);
}

// An export hides secrets unless the user turns redaction off, and the preview counts
// exactly what the export replaces.
#[test]
fn an_export_is_redacted_by_default_and_the_preview_counts_what_it_replaces() {
    let state = state();
    let made = session(&state, "Keys");
    let key = "sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    append(
        &state,
        &message(
            &made.id,
            "u1",
            AgentRole::User,
            &format!("use {key} please"),
            1,
        ),
    );
    for format in [AgentExportFormat::Markdown, AgentExportFormat::Json] {
        let default = agent_session_export_impl(&state, &made.id, format).expect("export");
        assert!(!default.contains(key), "{default}");
        let preview = agent_session_export_preview_impl(&state, &made.id, format).expect("preview");
        assert!(preview.redactions >= 1);
        let plain = agent_session_export_redacted_impl(&state, &made.id, format, Some(false))
            .expect("plain");
        assert!(plain.contains(key));
        let (_, replaced) = crate::llm::capture::redact_text(&plain, &[]);
        assert_eq!(preview.redactions, replaced);
    }
}

#[test]
fn an_idle_chat_rebinds_to_another_workspace_and_a_running_one_is_refused() {
    use crate::agent_service::agent_session_bind_spanbay_impl;
    use crate::workspace::Environment;
    use crate::workspace::models::Workspace;
    let state = state();
    {
        let store = state.store();
        for (id, name) in [("ws_pay", "Payments platform"), ("ws_ops", "Ops")] {
            store
                .save_workspace(&Workspace::new(id, name).expect("ws"))
                .expect("save ws");
        }
        for (id, ws, name) in [
            ("env_stg", "ws_pay", "Staging"),
            ("env_prod", "ws_ops", "Production"),
        ] {
            store
                .save_environment(&Environment::new(id, ws, name).expect("env"))
                .expect("save env");
        }
    }
    let made = session(&state, "Bound");
    let bound = agent_session_bind_spanbay_impl(&state, &made.id, "ws_pay", Some("env_stg".into()))
        .expect("first bind");
    assert_eq!(
        bound.spanbay_workspace_name.as_deref(),
        Some("Payments platform")
    );

    // An environment of another workspace is not part of the target workspace.
    assert!(matches!(
        agent_session_bind_spanbay_impl(&state, &made.id, "ws_pay", Some("env_prod".into())),
        Err(AppError::NotFound { .. })
    ));
    let moved =
        agent_session_bind_spanbay_impl(&state, &made.id, "ws_ops", Some("env_prod".into()))
            .expect("idle rebind");
    assert_eq!(moved.spanbay_workspace_id.as_deref(), Some("ws_ops"));
    assert_eq!(moved.spanbay_environment_id.as_deref(), Some("env_prod"));

    state
        .store()
        .insert_agent_run(&run_row("live", &made.id, AgentRunStatus::Running))
        .expect("run");
    let refused = agent_session_bind_spanbay_impl(&state, &made.id, "ws_pay", None);
    assert!(
        matches!(&refused, Err(AppError::Conflict { .. }))
            && refused.as_ref().err().map(AppError::message)
                == Some("this chat is already answering"),
        "{refused:?}"
    );
    // The same pair again is a no-op even mid-run.
    agent_session_bind_spanbay_impl(&state, &made.id, "ws_ops", Some("env_prod".into()))
        .expect("no-op");
}
