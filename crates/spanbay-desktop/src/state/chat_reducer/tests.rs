use spanbay_core::agent::model::{
    AgentRunKind, AgentTarget, AgentTodoStatus, HumanGateBody, HumanGateKind,
    LiveToolCall as CoreToolCall, RiskLevel, SandboxKind, ToolClass,
};

use std::ops::Not;

use super::*;

fn env_on(run_id: &str, first_seq: u64, seq: u64, event: AgentEvent) -> AgentEnvelope {
    AgentEnvelope {
        seq,
        first_seq,
        ts_ms: 1000,
        run_id: run_id.into(),
        event,
    }
}

fn env(seq: u64, event: AgentEvent) -> AgentEnvelope {
    env_on("r1", seq, seq, event)
}

fn text(t: &str) -> AgentEvent {
    AgentEvent::TextDelta { text: t.into() }
}

fn apply(state: &mut ChatThreadState, envelope: AgentEnvelope) -> BatchEffects {
    state.apply_batch([envelope])
}

fn run_summary(run_id: &str, kind: AgentRunKind) -> AgentRunSummary {
    AgentRunSummary {
        run_id: run_id.into(),
        session_id: "s".into(),
        kind,
        parent_run_id: None,
        parent_call_id: None,
        status: AgentRunStatus::Running,
        model_label: "fast".into(),
        target: Some(AgentTarget::Named {
            name: "fast".into(),
        }),
        iterations: 0,
        usage: HubUsage::default(),
        cost_micro_usd: 0,
        started_at_ms: 1000,
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

fn approval_event(run_id: &str, call_id: &str, arguments: &str) -> AgentEvent {
    AgentEvent::ToolApprovalRequired {
        run_id: run_id.into(),
        call_id: call_id.into(),
        name: "shell".into(),
        arguments: arguments.into(),
        class: ToolClass::Exec,
        read_only: false,
        destructive: true,
        sandbox: SandboxKind::None,
        reason: "runs a command".into(),
        preview: None,
        network: true,
        risk: "low".into(),
        summary: "rm a".into(),
        full_text: Some("rm a".into()),
        card: None,
    }
}

fn snapshot(seq: u64, parts: Vec<HubPart>, tools: Vec<CoreToolCall>) -> LiveRunSnapshot {
    LiveRunSnapshot {
        run_id: "r1".into(),
        seq,
        parts,
        tools,
        usage: None,
        truncated: false,
        gates: Vec::new(),
        cost_limit: None,
    }
}

fn message(id: &str, seq: i64, role: AgentRole, parts: Vec<HubPart>) -> AgentMessage {
    AgentMessage {
        id: id.into(),
        session_id: "s1".into(),
        seq,
        role,
        parts,
        usage: None,
        model: None,
        error: None,
        run_id: Some("r1".into()),
        parent_id: None,
        branch_index: 0,
        partial: false,
        cost_micro_usd: 0,
        cost_currency: None,
        meta: Default::default(),
        branch_siblings: Vec::new(),
        attachments: Vec::new(),
        created_at_ms: seq * 1000,
    }
}

#[test]
fn a_skipped_seq_buffers_until_the_snapshot_then_replays_in_order_once() {
    let mut state = ChatThreadState::new("s1", "t");
    apply(&mut state, env(1, text("a")));
    assert!(apply(&mut state, env(1, text("DUP"))).changed.not());

    let fx = apply(&mut state, env(4, text("d")));
    assert!(fx.needs_resync);
    assert!(!fx.changed);
    apply(&mut state, env(5, text("e")));
    assert_eq!(
        state.streaming_text, "a",
        "nothing applies while the resync is open"
    );

    // The snapshot covers 1..=3 (its seq); 4 and 5 replay on top of it.
    let fx = state.finish_resync(
        "t".into(),
        Vec::new(),
        Some(&snapshot(3, vec![HubPart::text("abc")], Vec::new())),
    );
    assert!(!fx.needs_resync);
    assert!(fx.scroll_worthy, "the replay's effects reach the view");
    assert_eq!(state.streaming_text, "abcde");
    assert_eq!(state.last_seq("r1"), Some(5));
    apply(&mut state, env(6, text("f")));
    assert_eq!(state.streaming_text, "abcdef");
}

#[test]
fn a_merged_envelope_across_the_baseline_is_a_gap_and_an_unknown_run_starts_anywhere() {
    let mut state = ChatThreadState::new("s1", "t");
    // Unknown run: its first envelope is the baseline, whatever it merged.
    assert!(apply(&mut state, env_on("r1", 7, 9, text("x"))).changed);
    assert_eq!(state.last_seq("r1"), Some(9));

    let fx = apply(&mut state, env_on("r1", 9, 11, text("y")));
    assert!(
        fx.needs_resync,
        "9 was applied already: replaying 9..=11 would repeat it"
    );
    assert_eq!(state.streaming_text, "x");
}

#[test]
fn a_gap_event_resets_the_baseline_and_an_overflowing_buffer_asks_for_another_snapshot() {
    let mut state = ChatThreadState::new("s1", "t");
    apply(&mut state, env(1, text("a")));
    let fx = apply(&mut state, env(9, AgentEvent::Gap { first_seq: 10 }));
    assert!(fx.needs_resync);
    assert_eq!(state.last_seq("r1"), Some(9));

    state.apply_batch((10..=10 + RESYNC_BUFFER_MAX as u64).map(|seq| env(seq, text("z"))));
    assert!(
        state
            .finish_resync("t".into(), Vec::new(), None)
            .needs_resync
    );
    // Still open: new envelopes wait for the next round.
    let next = 11 + RESYNC_BUFFER_MAX as u64;
    apply(&mut state, env(next, text("q")));
    assert!(!state.streaming_text.contains('q'));
    assert!(
        !state
            .finish_resync(
                "t".into(),
                Vec::new(),
                Some(&snapshot(next - 1, Vec::new(), Vec::new())),
            )
            .needs_resync
    );
    assert_eq!(state.streaming_text, "q");
}

#[test]
fn a_resync_without_a_live_run_ends_the_stale_run_and_drops_stale_buffered_deltas() {
    let mut state = ChatThreadState::new("s1", "t");
    state.apply_batch([
        env(
            1,
            AgentEvent::RunStarted {
                run: Box::new(run_summary("r1", AgentRunKind::Main)),
            },
        ),
        env(2, text("stale")),
    ]);
    assert!(apply(&mut state, env(4, text("d"))).needs_resync);
    apply(&mut state, env(5, text("e")));

    // Nothing runs now: r1 finished into persisted history, so its stale buffered deltas are
    // dropped instead of reviving streaming_text, while a subsequent run r2 starts cleanly.
    let fx = state.finish_resync("t".into(), Vec::new(), None);
    assert!(!fx.needs_resync);
    assert!(state.active_run.is_none());
    assert!(state.streaming_text.is_empty());
    assert_eq!(state.last_seq("r1"), Some(u64::MAX));

    let fx2 = apply(
        &mut state,
        env_on(
            "r2",
            1,
            1,
            AgentEvent::RunStarted {
                run: Box::new(run_summary("r2", AgentRunKind::Main)),
            },
        ),
    );
    assert!(fx2.run_started.as_deref() == Some("r2"));
}

#[test]
fn event_and_reload_paths_build_the_same_approval_and_a_child_card_keeps_its_arguments() {
    let event = approval_event("r1", "c1", "{\"cmd\":\"rm a\"}");
    let mut live_state = ChatThreadState::new("s1", "t");
    apply(&mut live_state, env(1, event.clone()));

    let request = LiveApprovalRequest::from_event(&event, 1000).map(Box::new);
    assert_eq!(request.as_ref().map(|r| r.risk), Some(RiskLevel::Low));
    let child_request =
        LiveApprovalRequest::from_event(&approval_event("r_child", "k1", "{\"cmd\":\"ls\"}"), 1000)
            .map(Box::new);
    let tool = |call_id: &str, child: Option<&str>, approval| CoreToolCall {
        call_id: call_id.into(),
        name: "shell".into(),
        source: ToolSource::Builtin,
        state: LiveToolState::WaitingApproval,
        child_run_id: child.map(str::to_owned),
        result: None,
        approval,
    };
    let mut reloaded = ChatThreadState::new("s1", "t");
    reloaded.hydrate_live(&snapshot(
        5,
        vec![HubPart::ToolCall {
            id: "c1".into(),
            name: "shell".into(),
            arguments: "{\"cmd\":\"rm a\"}".into(),
            signature: None,
            origin: None,
        }],
        vec![
            tool("c1", None, request),
            tool("k1", Some("r_child"), child_request),
        ],
    ));

    assert_eq!(
        reloaded.pending_approvals[0],
        live_state.pending_approvals[0]
    );
    assert_eq!(live_state.pending_approvals[0].requested_at_ms(), 1000);
    let child = &reloaded.pending_approvals[1];
    assert_eq!(child.run_id, "r_child");
    assert_eq!(child.request.arguments, "{\"cmd\":\"ls\"}");
    assert_eq!(child.offered_scopes(), &DEFAULT_SCOPES[..]);
    // The child's call is not a row of the parent's turn.
    assert_eq!(reloaded.tool_calls.len(), 1);
}

#[test]
fn cancellation_is_not_a_failure_and_the_run_end_closes_gates() {
    let gate = HumanGateDto {
        id: "g1".into(),
        run_id: "r1".into(),
        call_id: "c1".into(),
        kind: HumanGateKind::Plan,
        created_at_ms: 0,
        body: HumanGateBody::Plan {
            markdown: "plan".into(),
            title: None,
            steps: 1,
            tasks: Vec::new(),
        },
    };
    let mut state = ChatThreadState::new("s1", "t");
    apply(
        &mut state,
        env(
            1,
            AgentEvent::RunStarted {
                run: Box::new(run_summary("r1", AgentRunKind::Main)),
            },
        ),
    );
    apply(
        &mut state,
        env(
            2,
            AgentEvent::GateOpened {
                run_id: "r1".into(),
                gate: gate.clone(),
            },
        ),
    );
    let fx = apply(
        &mut state,
        env(
            3,
            AgentEvent::Error {
                run_id: "r1".into(),
                error_kind: "Cancelled".into(),
                message: "stopped".into(),
                failure: None,
            },
        ),
    );
    assert!(fx.finished);
    assert_eq!(
        state.active_run.as_ref().map(|r| r.status),
        Some(AgentRunStatus::Cancelled)
    );
    assert_eq!(state.last_error, None);
    assert!(state.open_gates.is_empty());

    // A real failure is kept; Done closes gates too.
    let mut failed = ChatThreadState::new("s1", "t");
    apply(
        &mut failed,
        env(
            1,
            AgentEvent::Error {
                run_id: "r1".into(),
                error_kind: "Network".into(),
                message: "boom".into(),
                failure: None,
            },
        ),
    );
    assert_eq!(
        failed.last_error.as_ref().map(|e| e.message.as_str()),
        Some("boom")
    );
    apply(
        &mut failed,
        env(
            2,
            AgentEvent::GateOpened {
                run_id: "r1".into(),
                gate,
            },
        ),
    );
    apply(
        &mut failed,
        env(
            3,
            AgentEvent::Done {
                run: Box::new(run_summary("r1", AgentRunKind::Main)),
                stop_reason: AgentStopReason::Completed,
            },
        ),
    );
    assert!(failed.open_gates.is_empty());
}

#[test]
fn child_todos_resolutions_and_output_streams_are_kept() {
    let mut state = ChatThreadState::new("s1", "t");
    apply(
        &mut state,
        env(
            1,
            AgentEvent::SubagentStarted {
                parent_call_id: "tc".into(),
                child_run: Box::new(run_summary("r_child", AgentRunKind::Subagent)),
                child_session_id: "s_child".into(),
            },
        ),
    );
    let todo = AgentTodoItem {
        id: "t1".into(),
        content: "audit".into(),
        status: AgentTodoStatus::InProgress,
        depends_on: Vec::new(),
        kind: None,
        role: None,
        group: None,
        run_id: None,
        attempt: 0,
        summary: None,
        error: None,
    };
    apply(
        &mut state,
        env(
            2,
            AgentEvent::SubagentEvent {
                parent_call_id: "tc".into(),
                child_run_id: "r_child".into(),
                event: Box::new(AgentEvent::TodosUpdated {
                    todos: vec![todo.clone()],
                }),
            },
        ),
    );
    assert_eq!(state.subagents["r_child"].todos, vec![todo]);

    apply(
        &mut state,
        env(
            3,
            AgentEvent::ToolCallStarted {
                call_id: "c1".into(),
                name: "shell".into(),
                source: ToolSource::Builtin,
            },
        ),
    );
    apply(&mut state, env(4, approval_event("r1", "c1", "{}")));
    apply(
        &mut state,
        env(
            5,
            AgentEvent::ToolApprovalResolved {
                call_id: "c1".into(),
                decision: ApprovalDecision::Allow,
                scope: ApprovalScope::Session,
                timed_out: false,
            },
        ),
    );
    assert!(state.pending_approvals.is_empty());
    assert_eq!(
        state.tool_calls[0].resolution,
        Some(ApprovalResolution {
            decision: ApprovalDecision::Allow,
            scope: ApprovalScope::Session,
            timed_out: false,
        })
    );

    let delta = |seq, stream, t: &str| {
        env(
            seq,
            AgentEvent::ToolOutputDelta {
                call_id: "c1".into(),
                stream,
                text: t.into(),
            },
        )
    };
    state.apply_batch([
        delta(6, OutputStream::Stdout, "a"),
        delta(7, OutputStream::Stdout, "b"),
        delta(8, OutputStream::Stderr, "warn"),
    ]);
    assert_eq!(
        state.tool_calls[0].output,
        vec![
            (OutputStream::Stdout, "ab".to_owned()),
            (OutputStream::Stderr, "warn".to_owned()),
        ]
    );
    apply(
        &mut state,
        delta(
            9,
            OutputStream::Stdout,
            &"x".repeat(MAX_LIVE_TOOL_OUTPUT_BYTES),
        ),
    );
    let output = &state.tool_calls[0].output;
    assert_eq!(
        output.iter().map(|(_, t)| t.len()).sum::<usize>(),
        MAX_LIVE_TOOL_OUTPUT_BYTES
    );
    assert_eq!(output.len(), 1, "the oldest chunks go first");
}

#[test]
fn subagent_approvals_cost_pause_file_changes_and_cleanup_reach_parent_state() {
    let mut state = ChatThreadState::new("s_parent", "Parent");
    let child_run = run_summary("r_child", AgentRunKind::Subagent);
    apply(
        &mut state,
        env(
            1,
            AgentEvent::SubagentStarted {
                parent_call_id: "tc_task".into(),
                child_session_id: "s_child".into(),
                child_run: Box::new(child_run.clone()),
            },
        ),
    );
    let child = |seq, event| {
        env(
            seq,
            AgentEvent::SubagentEvent {
                parent_call_id: "tc_task".into(),
                child_run_id: "r_child".into(),
                event: Box::new(event),
            },
        )
    };
    apply(
        &mut state,
        child(2, approval_event("r_child", "c_write", "{}")),
    );
    assert_eq!(state.pending_approvals[0].run_id, "r_child");
    apply(
        &mut state,
        child(
            3,
            AgentEvent::CostLimitReached {
                run_id: "r_child".into(),
                cost_micro: 2_000_000,
                cap_micro: 1_000_000,
                currency: "USD".into(),
                breakdown: Some(vec![CostShare {
                    part: "subagent".into(),
                    micro: 2_000_000,
                }]),
                next_cap_micro: Some(3_000_000),
                unpriced_models: None,
                tokens: None,
            },
        ),
    );
    let pause = state.cost_pause.clone().unwrap();
    assert_eq!(pause.breakdown.len(), 1);
    assert_eq!(pause.next_cap_micro, Some(3_000_000));
    apply(
        &mut state,
        child(
            4,
            AgentEvent::FileChanged {
                call_id: "c_write".into(),
                path: "out.txt".into(),
                change: FileChangeKind::Create,
                added: 3,
                removed: 0,
                change_id: Some("fc1".into()),
            },
        ),
    );
    assert_eq!(state.file_changes[0].path, "out.txt");

    // SubagentFinished clears any lingering approval or cost pause belonging to that child.
    apply(
        &mut state,
        env(
            5,
            AgentEvent::SubagentFinished {
                parent_call_id: "tc_task".into(),
                child_run: Box::new(AgentRunSummary {
                    status: AgentRunStatus::Completed,
                    ..child_run
                }),
            },
        ),
    );
    assert!(state.cost_pause.is_none());
    assert!(state.pending_approvals.is_empty());
    assert!(state.subagents["r_child"].finished);
}

#[test]
fn subagent_captures_brief_metadata_and_tool_steps_from_parent_call() {
    let mut state = ChatThreadState::new("s_p", "Parent");
    state.apply_batch([
        env(
            1,
            AgentEvent::ToolCallStarted {
                call_id: "tc_1".into(),
                name: "task".into(),
                source: ToolSource::Builtin,
            },
        ),
        env(
            2,
            AgentEvent::ToolCallArgsDelta {
                call_id: "tc_1".into(),
                arguments: r#"{"description":"Audit auth routes","kind":"explore","resume":"as_0"}"#
                    .into(),
            },
        ),
        env(
            3,
            AgentEvent::SubagentStarted {
                parent_call_id: "tc_1".into(),
                child_session_id: "s_c1".into(),
                child_run: Box::new(run_summary("r_c1", AgentRunKind::Subagent)),
            },
        ),
        env(
            4,
            AgentEvent::SubagentEvent {
                parent_call_id: "tc_1".into(),
                child_run_id: "r_c1".into(),
                event: Box::new(AgentEvent::ToolCallStarted {
                    call_id: "c_grep".into(),
                    name: "grep".into(),
                    source: ToolSource::Builtin,
                }),
            },
        ),
    ]);
    let sub = &state.subagents["r_c1"];
    assert_eq!(sub.description, "Audit auth routes");
    assert_eq!(sub.subagent_kind, "explore");
    assert!(sub.resumed);
    assert_eq!(sub.current_tool(), Some("grep"));

    apply(
        &mut state,
        env(
            5,
            AgentEvent::SubagentEvent {
                parent_call_id: "tc_1".into(),
                child_run_id: "r_c1".into(),
                event: Box::new(AgentEvent::ToolResult {
                    call_id: "c_grep".into(),
                    name: "grep".into(),
                    status: ToolCallStatus::Ok,
                    parts: Vec::new(),
                    duration_ms: 85,
                    meta: None,
                    exit_code: None,
                }),
            },
        ),
    );
    let sub = &state.subagents["r_c1"];
    assert_eq!(sub.current_tool(), None);
    assert_eq!(sub.tool_steps[0].duration_ms, Some(85));
}

#[test]
fn prepend_earlier_and_merge_latest_snapshot_preserve_loaded_history() {
    let mk = |id: &str, seq: i64| message(id, seq, AgentRole::User, Vec::new());
    let mut state = ChatThreadState::new("s1", "Init");
    state.replace_snapshot("Init".into(), vec![mk("m3", 3), mk("m4", 4)]);

    // Prepending earlier messages deduplicates overlap and preserves chronological order.
    state.prepend_earlier_messages(vec![mk("m1", 1), mk("m2", 2), mk("m3", 3)]);
    let ids: Vec<&str> = state.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["m1", "m2", "m3", "m4"]);

    // Merging a refreshed latest tail keeps earlier loaded turns (< first_latest_seq) intact.
    state.merge_latest_snapshot(
        "Renamed".into(),
        vec![mk("m3", 3), mk("m4", 4), mk("m5", 5)],
    );
    assert_eq!(state.title, "Renamed");
    let ids: Vec<&str> = state.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["m1", "m2", "m3", "m4", "m5"]);
}

#[test]
fn tool_result_replaces_pending_placeholder_in_saved_tool_message_mid_run() {
    let mut state = ChatThreadState::new("s1", "Live");
    let tool_msg = message(
        "m_tool_1",
        2,
        AgentRole::Tool,
        vec![HubPart::ToolResult {
            call_id: "c_read_1".into(),
            name: Some("read_file".into()),
            content: vec![HubPart::text(PENDING_TOOL_RESULT_MARKER)],
            is_error: true,
        }],
    );
    apply(
        &mut state,
        env(
            1,
            AgentEvent::MessageSaved {
                message: Box::new(tool_msg),
            },
        ),
    );
    assert!(state.pending_tool_calls.contains("c_read_1"));

    let body = vec![HubPart::text("     1\t[package]\n")];
    apply(
        &mut state,
        env(
            2,
            AgentEvent::ToolResult {
                call_id: "c_read_1".into(),
                name: "read_file".into(),
                status: ToolCallStatus::Ok,
                parts: body.clone(),
                duration_ms: 12,
                meta: None,
                exit_code: None,
            },
        ),
    );
    assert!(!state.pending_tool_calls.contains("c_read_1"));
    assert_eq!(state.live_tool_meta["c_read_1"].duration_ms, 12);
    match &state.messages[0].parts[0] {
        HubPart::ToolResult {
            content, is_error, ..
        } => {
            assert!(!is_error);
            assert_eq!(content, &body);
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[test]
fn inline_images_decode_and_remote_ones_do_not() {
    let inline = HubImage::Base64 {
        media_type: "image/png".into(),
        data: "aGk=".into(),
    };
    assert_eq!(
        decode_inline_image(&inline),
        Some((b"hi".to_vec(), "image/png".to_owned()))
    );
    let remote = HubImage::Url {
        url: "https://example.com/a.png".into(),
    };
    assert_eq!(decode_inline_image(&remote), None);
}

#[test]
fn merge_latest_snapshot_drops_abandoned_root_branch_and_handles_empty_clear() {
    let mut u1 = message("u1", 1, AgentRole::User, vec![HubPart::text("old")]);
    u1.branch_index = 0;
    let mut a1 = message("a1", 2, AgentRole::Assistant, vec![HubPart::text("ans1")]);
    a1.parent_id = Some("u1".into());

    let mut u1_edit = message("u1_edit", 3, AgentRole::User, vec![HubPart::text("new")]);
    u1_edit.parent_id = None;
    u1_edit.branch_index = 1;
    let mut a1_edit = message(
        "a1_edit",
        4,
        AgentRole::Assistant,
        vec![HubPart::text("ans2")],
    );
    a1_edit.parent_id = Some("u1_edit".into());

    let mut state = ChatThreadState::new("s1", "t");
    state.replace_snapshot("t".into(), vec![u1, a1]);
    state.merge_latest_snapshot("t".into(), vec![u1_edit, a1_edit]);
    let ids: Vec<&str> = state.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["u1_edit", "a1_edit"]);

    // Empty latest snapshot must clear stale messages instead of returning early.
    state.merge_latest_snapshot("t".into(), Vec::new());
    assert!(state.messages.is_empty());
}

#[test]
fn hydrate_live_sets_active_run_strips_partial_keeps_citations_and_clears_stale_subagents() {
    let mut partial = message("m_part", 2, AgentRole::Assistant, vec![HubPart::text("hi")]);
    partial.partial = true;
    partial.run_id = Some("r1".into());
    let mut state = ChatThreadState::new("s1", "t");
    state.messages.push(partial);
    state.subagents.insert(
        "r_old_child".into(),
        LiveSubagent {
            parent_call_id: "tc0".into(),
            child_run_id: "r_old_child".into(),
            child_session_id: "s_old".into(),
            description: String::new(),
            subagent_kind: "general".into(),
            resumed: false,
            summary: run_summary("r_old_child", AgentRunKind::Subagent),
            tool_steps: Vec::new(),
            todos: Vec::new(),
            finished: false,
        },
    );
    let cit = HubCitation {
        title: "Docs".into(),
        url: "https://example.com".into(),
        snippet: None,
        start_index: None,
        end_index: None,
    };
    let snap = snapshot(
        7,
        vec![
            HubPart::Text {
                text: "hi there".into(),
                signature: None,
                origin: None,
                citations: vec![cit.clone()],
            },
            HubPart::ToolCall {
                id: "c1".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
                signature: None,
                origin: None,
            },
        ],
        vec![CoreToolCall {
            call_id: "c1".into(),
            name: "read_file".into(),
            source: ToolSource::Builtin,
            state: LiveToolState::Running,
            child_run_id: None,
            result: None,
            approval: None,
        }],
    );
    state.hydrate_live(&snap);
    assert!(
        state.messages.is_empty(),
        "partial row superseded by live.parts"
    );
    assert_eq!(
        state.active_run.as_ref().map(|r| r.run_id.as_str()),
        Some("r1")
    );
    assert_eq!(
        state.active_run.as_ref().map(|r| r.status),
        Some(AgentRunStatus::Running)
    );
    assert_eq!(state.live_citations, vec![cit]);
    assert!(state.subagents.is_empty());
}

#[test]
fn reducer_handles_tool_result_parts_parallel_approvals_error_run_id_and_stale_gap() {
    let mut state = ChatThreadState::new("s1", "t");
    // Non-streaming tool result stays in `result_parts`, not stuffed into stdout `output`.
    state.apply_batch([
        env(
            1,
            AgentEvent::ToolCallStarted {
                call_id: "c1".into(),
                name: "read_file".into(),
                source: ToolSource::Builtin,
            },
        ),
        env(
            2,
            AgentEvent::ToolResult {
                call_id: "c1".into(),
                name: "read_file".into(),
                status: ToolCallStatus::Ok,
                parts: vec![HubPart::text("line 1")],
                duration_ms: 5,
                meta: None,
                exit_code: None,
            },
        ),
    ]);
    assert!(state.tool_calls[0].output.is_empty());
    assert_eq!(
        state.tool_calls[0].result_parts,
        vec![HubPart::text("line 1")]
    );

    // Parallel sub-agent approvals with identical call_id are keyed by (run_id, call_id).
    state.apply_batch([
        env(3, approval_event("r_sub_a", "same_call", "{}")),
        env(4, approval_event("r_sub_b", "same_call", "{}")),
    ]);
    assert_eq!(state.pending_approvals.len(), 2);
    apply(
        &mut state,
        env(
            5,
            AgentEvent::SubagentEvent {
                parent_call_id: "tc".into(),
                child_run_id: "r_sub_a".into(),
                event: Box::new(AgentEvent::ToolApprovalResolved {
                    call_id: "same_call".into(),
                    decision: ApprovalDecision::Allow,
                    scope: ApprovalScope::Once,
                    timed_out: false,
                }),
            },
        ),
    );
    assert_eq!(state.pending_approvals.len(), 1);
    assert_eq!(state.pending_approvals[0].run_id, "r_sub_b");

    // Stale Gap behind seen seq does not rewind last_seq or reopen resync.
    let fx_gap = apply(&mut state, env(2, AgentEvent::Gap { first_seq: 3 }));
    assert!(!fx_gap.needs_resync);
    assert_eq!(state.last_seq("r1"), Some(5));

    // Error for a different run_id does not overwrite a prior completed active_run,
    // and Done clears last_error.
    let mut done_run = run_summary("r1", AgentRunKind::Main);
    done_run.status = AgentRunStatus::Completed;
    apply(
        &mut state,
        env(
            6,
            AgentEvent::Done {
                run: Box::new(done_run),
                stop_reason: AgentStopReason::Completed,
            },
        ),
    );
    apply(
        &mut state,
        env_on(
            "r_other",
            1,
            1,
            AgentEvent::Error {
                run_id: "r_other".into(),
                error_kind: "Network".into(),
                message: "queued failed".into(),
                failure: None,
            },
        ),
    );
    assert_eq!(
        state.active_run.as_ref().map(|r| r.status),
        Some(AgentRunStatus::Completed)
    );
    assert!(state.last_error.is_some());
    let mut done2 = run_summary("r2", AgentRunKind::Main);
    done2.status = AgentRunStatus::Completed;
    apply(
        &mut state,
        env_on(
            "r2",
            1,
            1,
            AgentEvent::Done {
                run: Box::new(done2),
                stop_reason: AgentStopReason::Completed,
            },
        ),
    );
    assert!(state.last_error.is_none());
}
