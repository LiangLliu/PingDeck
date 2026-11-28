use std::collections::HashMap;
use std::sync::Arc;

use spanbay_core::agent::model::{
    AgentMessage, AgentMessageMeta, AgentRole, FileChange, FileChangeKind, FileChangeStatus,
    ReasoningEffort,
};
use spanbay_core::model::hub::{HubPart, HubUsage};

use super::super::chat_reducer::ChatThreadState;
use super::super::tool_trace::ToolInlineMetric;
use super::{
    ThreadTurnEntry, TranscriptDensity, TurnActivityItem, TurnViewCache, build_thread_turn_entries,
};

fn mk_msg(id: &str, seq: i64, role: AgentRole, parts: Vec<HubPart>) -> AgentMessage {
    AgentMessage {
        id: id.to_owned(),
        session_id: "s-1".to_owned(),
        seq,
        role,
        parts,
        usage: None,
        model: Some("deepseek/deepseek-v4-pro".to_owned()),
        error: None,
        run_id: Some("r-1".to_owned()),
        parent_id: None,
        branch_index: 0,
        partial: false,
        cost_micro_usd: 0,
        cost_currency: Some("USD".to_owned()),
        meta: AgentMessageMeta::default(),
        branch_siblings: Vec::new(),
        attachments: Vec::new(),
        created_at_ms: seq * 1000,
    }
}

#[test]
fn groups_multi_step_turn_merges_explores_silences_todo_and_computes_metrics() {
    let mut thread = ChatThreadState::new("s-1", "Test");
    thread.context_limit_tokens = Some(128_000);

    let u1 = mk_msg("u-1", 1, AgentRole::User, vec![HubPart::text("Fix 429")]);

    let mut a1 = mk_msg(
        "a-1",
        2,
        AgentRole::Assistant,
        vec![
            HubPart::text("<think>Check router</think>Let me inspect the files first."),
            HubPart::ToolCall {
                id: "c-todo".to_owned(),
                name: "todo_write".to_owned(),
                arguments:
                    r#"{"todos":[{"id":"1","content":"Read router","status":"in_progress"}]}"#
                        .to_owned(),
                signature: None,
                origin: None,
            },
            HubPart::ToolCall {
                id: "c-read-1".to_owned(),
                name: "read_file".to_owned(),
                arguments: r#"{"path":"src/router.rs"}"#.to_owned(),
                signature: None,
                origin: None,
            },
            HubPart::ToolCall {
                id: "c-read-1-dup".to_owned(),
                name: "read_file".to_owned(),
                arguments: r#"{"path":"src/router.rs"}"#.to_owned(),
                signature: None,
                origin: None,
            },
            HubPart::ToolCall {
                id: "c-read-2".to_owned(),
                name: "read_file".to_owned(),
                arguments: r#"{"path":"src/limits.rs"}"#.to_owned(),
                signature: None,
                origin: None,
            },
            HubPart::ToolCall {
                id: "c-grep-1".to_owned(),
                name: "grep".to_owned(),
                arguments: r#"{"pattern":"429"}"#.to_owned(),
                signature: None,
                origin: None,
            },
        ],
    );
    a1.usage = Some(HubUsage {
        input_tokens: Some(10_000),
        output_tokens: Some(400),
        reasoning_tokens: Some(150),
        cached_input_tokens: Some(8_000),
        ..HubUsage::default()
    });
    a1.meta.ttft_ms = Some(500);
    a1.meta.stream_ms = Some(1_500);
    a1.meta.reasoning_effort = Some(ReasoningEffort::Low);
    a1.cost_micro_usd = 10_000;

    let t1 = mk_msg(
        "t-1",
        3,
        AgentRole::Tool,
        vec![
            HubPart::ToolResult {
                call_id: "c-todo".to_owned(),
                name: Some("todo_write".to_owned()),
                content: vec![HubPart::text("ok")],
                is_error: false,
            },
            HubPart::ToolResult {
                call_id: "c-read-1".to_owned(),
                name: Some("read_file".to_owned()),
                content: vec![HubPart::text("1\tfn route() {}")],
                is_error: false,
            },
            HubPart::ToolResult {
                call_id: "c-read-1-dup".to_owned(),
                name: Some("read_file".to_owned()),
                content: vec![HubPart::text("1\tfn route() {}")],
                is_error: false,
            },
            HubPart::ToolResult {
                call_id: "c-read-2".to_owned(),
                name: Some("read_file".to_owned()),
                content: vec![HubPart::text("1\tpub const MAX: u32 = 15;")],
                is_error: false,
            },
            HubPart::ToolResult {
                call_id: "c-grep-1".to_owned(),
                name: Some("grep".to_owned()),
                content: vec![HubPart::text("src/router.rs:10: 429")],
                is_error: false,
            },
        ],
    );

    let mut a2 = mk_msg(
        "a-2",
        4,
        AgentRole::Assistant,
        vec![
            HubPart::ToolCall {
                id: "c-edit-1".to_owned(),
                name: "edit_file".to_owned(),
                arguments: r#"{"path":"src/router.rs"}"#.to_owned(),
                signature: None,
                origin: None,
            },
            HubPart::ToolCall {
                id: "c-edit-2".to_owned(),
                name: "edit_file".to_owned(),
                arguments: r#"{"path":"src/router.rs"}"#.to_owned(),
                signature: None,
                origin: None,
            },
            HubPart::text("Fixed the 429 backoff in `src/router.rs`."),
        ],
    );
    a2.meta.served_model = Some("moonshot/kimi-k2.6".to_owned());
    a2.usage = Some(HubUsage {
        input_tokens: Some(12_000),
        output_tokens: Some(600),
        ..HubUsage::default()
    });
    a2.meta.ttft_ms = Some(400);
    a2.meta.stream_ms = Some(2_000);
    a2.cost_micro_usd = 8_000;

    let t2 = mk_msg(
        "t-2",
        5,
        AgentRole::Tool,
        vec![
            HubPart::ToolResult {
                call_id: "c-edit-1".to_owned(),
                name: Some("edit_file".to_owned()),
                content: vec![HubPart::text("Edited")],
                is_error: false,
            },
            HubPart::ToolResult {
                call_id: "c-edit-2".to_owned(),
                name: Some("edit_file".to_owned()),
                content: vec![HubPart::text("Edited")],
                is_error: false,
            },
        ],
    );

    thread.messages = vec![u1, a1, t1, a2, t2];
    let changes = vec![
        FileChange {
            id: "fc-1".to_owned(),
            session_id: "s-1".to_owned(),
            run_id: "r-1".to_owned(),
            call_id: "c-edit-1".to_owned(),
            path: "src/router.rs".to_owned(),
            folder: None,
            kind: FileChangeKind::Edit,
            before_sha256: None,
            after_sha256: None,
            added: 12,
            removed: 3,
            status: FileChangeStatus::Pending,
            created_at_ms: 1000,
        },
        FileChange {
            id: "fc-2".to_owned(),
            session_id: "s-1".to_owned(),
            run_id: "r-1".to_owned(),
            call_id: "c-edit-2".to_owned(),
            path: "src/router.rs".to_owned(),
            folder: None,
            kind: FileChangeKind::Edit,
            before_sha256: None,
            after_sha256: None,
            added: 6,
            removed: 1,
            status: FileChangeStatus::Pending,
            created_at_ms: 2000,
        },
    ];

    let entries = build_thread_turn_entries(
        &thread,
        &[],
        &changes,
        &HashMap::new(),
        None,
        false,
        TranscriptDensity::Normal,
    );
    assert_eq!(entries.len(), 2);
    let ThreadTurnEntry::AssistantTurn(turn) = &entries[1] else {
        panic!("expected AssistantTurn");
    };

    // Stable turn_id and turn_key_seq keyed on user message `u-1` (seq 1).
    assert_eq!(turn.turn_id, "turn-u-1");
    assert_eq!(turn.turn_key_seq, 1);

    assert_eq!(
        turn.models_used,
        vec!["deepseek/deepseek-v4-pro", "moonshot/kimi-k2.6"]
    );
    assert_eq!(turn.totals.hidden_todo_count, 1);
    assert_eq!(turn.totals.step_count, 2);

    // ExploreGroup deduplicates `src/router.rs` read twice + `src/limits.rs` = 2 distinct files,
    // and merges consecutive reads into 1 sub-row `"router.rs, limits.rs"` + 1 grep sub-row = 2 items.
    match &turn.spine[2] {
        TurnActivityItem::ExploreGroup(eg) => {
            assert_eq!(eg.files_read, 2);
            assert_eq!(eg.searches_run, 1);
            assert_eq!(eg.items.len(), 2);
            assert_eq!(eg.items[0].pres.target, "router.rs, limits.rs");
        }
        other => panic!("expected ExploreGroup, got {other:?}"),
    }
    match &turn.spine[3] {
        TurnActivityItem::ToolCall(tc) => {
            assert_eq!(tc.tool_name, "edit_file");
            assert_eq!(tc.edit_count, 2);
            assert_eq!(
                tc.pres.metric,
                Some(ToolInlineMetric::DiffStats {
                    added: 18,
                    removed: 4
                })
            );
        }
        other => panic!("expected coalesced ToolCall, got {other:?}"),
    }

    assert_eq!(
        turn.final_markdown,
        "Fixed the 429 backoff in `src/router.rs`."
    );
    assert_eq!(turn.turn_changes.len(), 1);
    assert_eq!(turn.turn_changes[0].added, 18);
    assert_eq!(turn.turn_changes[0].removed, 4);
}

#[test]
fn live_to_saved_keeps_turn_id_and_seq_and_mid_turn_compaction_stays_in_spine() {
    let mut thread = ChatThreadState::new("s-live", "Live");
    let u1 = mk_msg("u-1", 7, AgentRole::User, vec![HubPart::text("Go")]);
    thread.messages = vec![u1.clone()];
    thread.streaming_text = "Working...".to_owned();

    let live_entries = build_thread_turn_entries(
        &thread,
        &[],
        &[],
        &HashMap::new(),
        None,
        true,
        TranscriptDensity::Normal,
    );
    let ThreadTurnEntry::AssistantTurn(live_turn) = &live_entries[1] else {
        panic!("expected live AssistantTurn");
    };
    assert_eq!(live_turn.turn_id, "turn-u-1");
    assert_eq!(live_turn.turn_key_seq, 7);

    // Now the step saves, a mid-turn Summary compaction occurs, and a second step finishes.
    thread.streaming_text.clear();
    let a1 = mk_msg(
        "a-1",
        8,
        AgentRole::Assistant,
        vec![HubPart::text("Step 1")],
    );
    let summary = mk_msg(
        "sum-1",
        9,
        AgentRole::Summary,
        vec![HubPart::text("Summary")],
    );
    let a2 = mk_msg(
        "a-2",
        10,
        AgentRole::Assistant,
        vec![HubPart::text("Final")],
    );
    thread.messages = vec![u1, a1, summary, a2];

    let saved_entries = build_thread_turn_entries(
        &thread,
        &[],
        &[],
        &HashMap::new(),
        None,
        false,
        TranscriptDensity::Normal,
    );
    assert_eq!(
        saved_entries.len(),
        2,
        "mid-turn Summary must not split the turn"
    );
    let ThreadTurnEntry::AssistantTurn(saved_turn) = &saved_entries[1] else {
        panic!("expected saved AssistantTurn");
    };
    assert_eq!(saved_turn.turn_id, live_turn.turn_id);
    assert_eq!(saved_turn.turn_key_seq, live_turn.turn_key_seq);
    assert!(
        saved_turn
            .spine
            .iter()
            .any(|i| matches!(i, TurnActivityItem::CompactionDivider { .. })),
        "mid-turn Summary must appear as CompactionDivider inside the spine"
    );
    assert_eq!(saved_turn.final_markdown, "Final");
}

#[test]
fn older_turns_auto_collapse_unless_failed_and_failed_todo_is_not_hidden() {
    let mut thread = ChatThreadState::new("s-2", "History");
    let u1 = mk_msg("u-1", 1, AgentRole::User, vec![HubPart::text("Turn 1")]);
    let a1 = mk_msg(
        "a-1",
        2,
        AgentRole::Assistant,
        vec![
            HubPart::ToolCall {
                id: "c-r1".to_owned(),
                name: "read_file".to_owned(),
                arguments: r#"{"path":"a.rs"}"#.to_owned(),
                signature: None,
                origin: None,
            },
            HubPart::text("Done 1"),
        ],
    );
    let t1 = mk_msg(
        "t-1",
        3,
        AgentRole::Tool,
        vec![HubPart::ToolResult {
            call_id: "c-r1".to_owned(),
            name: Some("read_file".to_owned()),
            content: vec![HubPart::text("1\tok")],
            is_error: false,
        }],
    );

    let u2 = mk_msg("u-2", 4, AgentRole::User, vec![HubPart::text("Turn 2")]);
    let a2 = mk_msg(
        "a-2",
        5,
        AgentRole::Assistant,
        vec![
            HubPart::ToolCall {
                id: "c-todo-err".to_owned(),
                name: "todo_write".to_owned(),
                arguments: "{}".to_owned(),
                signature: None,
                origin: None,
            },
            HubPart::text("Done 2"),
        ],
    );
    let t2 = mk_msg(
        "t-2",
        6,
        AgentRole::Tool,
        vec![HubPart::ToolResult {
            call_id: "c-todo-err".to_owned(),
            name: Some("todo_write".to_owned()),
            content: vec![HubPart::text("invalid todos")],
            is_error: true,
        }],
    );

    thread.messages = vec![u1, a1, t1, u2, a2, t2];
    let entries = build_thread_turn_entries(
        &thread,
        &[],
        &[],
        &HashMap::new(),
        None,
        false,
        TranscriptDensity::Normal,
    );
    assert_eq!(entries.len(), 4);
    let ThreadTurnEntry::AssistantTurn(turn1) = &entries[1] else {
        panic!("expected turn 1");
    };
    let ThreadTurnEntry::AssistantTurn(turn2) = &entries[3] else {
        panic!("expected turn 2");
    };
    assert!(turn1.auto_collapsed);
    assert!(!turn2.auto_collapsed);
    assert!(turn2.has_failure);
}

#[test]
fn shell_non_zero_exit_code_marks_failure_and_audit_upsert_with_same_len_updates_status() {
    use super::super::run_graph::GraphNodeStatus;
    use spanbay_core::agent::model::AgentToolCallRecord;

    let mut thread = ChatThreadState::new("s-3", "Shell");
    let u1 = mk_msg("u-1", 1, AgentRole::User, vec![HubPart::text("Build")]);
    let a1 = mk_msg(
        "a-1",
        2,
        AgentRole::Assistant,
        vec![HubPart::ToolCall {
            id: "c-sh".to_owned(),
            name: "shell".to_owned(),
            arguments: r#"{"command":"cargo build --offline"}"#.to_owned(),
            signature: None,
            origin: None,
        }],
    );
    thread.messages = vec![u1, a1];

    let mut audit = vec![AgentToolCallRecord {
        session_id: "s-3".to_owned(),
        call_id: "c-sh".to_owned(),
        run_id: "r-1".to_owned(),
        message_id: Some("a-1".to_owned()),
        name: "shell".to_owned(),
        class: "exec".to_owned(),
        state: "approval-requested".to_owned(),
        args_json: r#"{"command":"cargo build --offline"}"#.to_owned(),
        risk: "medium".to_owned(),
        decided_by: None,
        decision: None,
        decided_ms: None,
        started_ms: None,
        ended_ms: None,
        exit_code: None,
        sandbox: None,
        truncated: false,
        output_bytes: 0,
        spill_ref: None,
        requested_ms: Some(1_000),
    }];

    let mut cache = TurnViewCache::default();
    let mut turn_inputs_version = 1u64;
    let first = cache.get_or_build(
        &thread,
        turn_inputs_version,
        &audit,
        &[],
        &HashMap::new(),
        None,
        true,
        TranscriptDensity::Normal,
    );
    let second = cache.get_or_build(
        &thread,
        turn_inputs_version,
        &audit,
        &[],
        &HashMap::new(),
        None,
        true,
        TranscriptDensity::Normal,
    );
    assert!(Arc::ptr_eq(&first, &second));
    let ThreadTurnEntry::AssistantTurn(turn_before) = &first[1] else {
        panic!("expected AssistantTurn");
    };
    match &turn_before.spine[0] {
        TurnActivityItem::ToolCall(tc) => assert_eq!(tc.status, GraphNodeStatus::Waiting),
        other => panic!("expected ToolCall, got {other:?}"),
    }

    // In-place upsert of the audit record (same len = 1): `approval-requested` -> `output-available`
    // with exit_code = 101 and bumped `turn_inputs_version` must rebuild and update status!
    audit[0].state = "output-available".to_owned();
    audit[0].exit_code = Some(101);
    turn_inputs_version += 1;

    let third = cache.get_or_build(
        &thread,
        turn_inputs_version,
        &audit,
        &[],
        &HashMap::new(),
        None,
        false,
        TranscriptDensity::Compact,
    );
    assert!(!Arc::ptr_eq(&first, &third));
    let ThreadTurnEntry::AssistantTurn(turn_after) = &third[1] else {
        panic!("expected AssistantTurn");
    };
    assert!(turn_after.has_failure);
    assert!(!turn_after.auto_collapsed);
    match &turn_after.spine[0] {
        TurnActivityItem::ToolCall(tc) => {
            assert_eq!(tc.status, GraphNodeStatus::Completed);
            assert_eq!(tc.exit_code, Some(101));
            assert!(tc.should_auto_expand());
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

#[test]
fn replacing_prefix_message_parts_with_same_id_and_seq_invalidates_prefix_cache() {
    let mut thread = ChatThreadState::new("s-prefix", "Prefix");
    thread.messages = vec![
        mk_msg("u-1", 1, AgentRole::User, vec![HubPart::text("Turn 1")]),
        mk_msg(
            "a-1",
            2,
            AgentRole::Assistant,
            vec![HubPart::text("Initial partial reply")],
        ),
        mk_msg("u-2", 3, AgentRole::User, vec![HubPart::text("Turn 2")]),
        mk_msg(
            "a-2",
            4,
            AgentRole::Assistant,
            vec![HubPart::text("Latest")],
        ),
    ];
    thread.messages_version = 1;
    thread.version = 1;

    let mut cache = TurnViewCache::default();
    let first = cache.get_or_build(
        &thread,
        1,
        &[],
        &[],
        &HashMap::new(),
        None,
        false,
        TranscriptDensity::Normal,
    );
    let ThreadTurnEntry::AssistantTurn(t1_before) = &first[1] else {
        panic!("expected Turn 1");
    };
    assert_eq!(t1_before.final_markdown, "Initial partial reply");

    // Replace `a-1`'s parts in-place keeping the exact same `id` ("a-1"), `seq` (2), and `messages.len()` (4),
    // bumping `messages_version` and `version`.
    thread.messages[1].parts = vec![HubPart::text("Replaced final reply")];
    thread.messages_version = thread.messages_version.wrapping_add(1);
    thread.version = thread.version.wrapping_add(1);

    let second = cache.get_or_build(
        &thread,
        1,
        &[],
        &[],
        &HashMap::new(),
        None,
        false,
        TranscriptDensity::Normal,
    );
    let ThreadTurnEntry::AssistantTurn(t1_after) = &second[1] else {
        panic!("expected Turn 1");
    };
    assert_eq!(
        t1_after.final_markdown, "Replaced final reply",
        "same-id/seq prefix message update must invalidate PrefixCacheKey via messages_version"
    );
}
