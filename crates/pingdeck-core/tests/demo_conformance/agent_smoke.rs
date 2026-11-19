//! End-to-end smoke test of the agent harness through the public `pingdeck_core::app` API: a
//! stdio MCP server, a session targeting a gateway alias, a fake OpenAI-compatible upstream that
//! first asks for the MCP `echo` tool and then answers, and the approval round trip in between.
//! Run with `--nocapture` to see the events and the stored messages.
//!
//! Golden path G6 (`golden_g6_*`): the agent edits a file in its bound temp folder after an
//! approval.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::common;

use std::sync::Arc;
use std::time::Duration;

use common::{
    ALIAS, FAKE_MCP_SERVER, FINAL_TEXT, TempDir, UPSTREAM_SECRET, Upstream, configure_llm,
    openai_chat, python_available,
};
use pingdeck_core::AppLifecycleExt as _;
use pingdeck_core::agent::approvals::ApprovalAnswer;
use pingdeck_core::agent::dto::{AgentRunInput, AgentSubscribeTarget};
use pingdeck_core::agent::mcp::McpTransportConfig;
use pingdeck_core::agent::model::{
    AgentEnvelope, AgentEvent, AgentMcpServer, AgentRole, AgentSessionInput, AgentStopReason,
    AgentTarget, ApprovalDecision, ApprovalScope, ToolCallStatus,
};
use pingdeck_core::app::{
    AppState, agent_approval_answer_impl, agent_mcp_server_save_impl, agent_run_start_impl,
    agent_run_subscribe_impl, agent_run_unsubscribe_impl, agent_session_create_impl,
    agent_session_get_impl, agent_tool_list_impl,
};
use pingdeck_core::llm::hub::{HubPart, text_of};
use pingdeck_core::model::settings::ToolApproval;
use tokio::sync::mpsc::unbounded_channel;

const ECHO_TOOL: &str = "mcp__smoke__echo";
const ECHOED: &str = r#"{"msg": "hello from the model"}"#;

/// `text` is `inner` inside the `<external_data>` envelope of an MCP result.
fn enveloped(text: &str, inner: &str) -> bool {
    text.starts_with(&format!("<external_data source=\"{ECHO_TOOL}\" id=\""))
        && text.contains(&format!("\">\n{inner}\n</external_data id=\""))
}

/// How a run ended.
struct Outcome {
    stop_reason: AgentStopReason,
    iterations: u32,
}

/// Runs one turn in a fresh `Ask` session with `disabled_tools` switched off (the fake upstream
/// calls the first tool it is offered); see [`run_session`].
async fn run_turn(
    state: &Arc<AppState>,
    disabled_tools: &[String],
    decision: ApprovalDecision,
) -> (String, Vec<AgentEvent>, Outcome) {
    let input = AgentSessionInput {
        permission_mode: Some(ToolApproval::Ask),
        disabled_tools: Some(disabled_tools.to_vec()),
        ..AgentSessionInput::default()
    };
    run_session(state, input, decision).await
}

/// Runs one turn in a fresh session made from `input` on [`ALIAS`], subscribed to the whole
/// chat before the run starts; every approval request is answered with `decision` as its event
/// arrives. Returns the session id, the events up to the end of the run and how it ended.
async fn run_session(
    state: &Arc<AppState>,
    input: AgentSessionInput,
    decision: ApprovalDecision,
) -> (String, Vec<AgentEvent>, Outcome) {
    let session = agent_session_create_impl(
        state,
        AgentSessionInput {
            target: Some(AgentTarget::Named { name: ALIAS.into() }),
            ..input
        },
    )
    .unwrap();
    let (sender, mut received) = unbounded_channel::<AgentEnvelope>();
    let subscription = agent_run_subscribe_impl(
        state,
        &AgentSubscribeTarget::Session {
            session_id: session.id.clone(),
        },
        None,
        Arc::new(move |envelope: AgentEnvelope| sender.send(envelope).is_ok()),
    )
    .unwrap();
    agent_run_start_impl(
        state,
        AgentRunInput {
            session_id: session.id.clone(),
            text: "Please echo hello".into(),
            ..AgentRunInput::default()
        },
    )
    .await
    .unwrap();

    let mut events = Vec::new();
    let outcome = loop {
        let envelope = tokio::time::timeout(Duration::from_secs(60), received.recv())
            .await
            .expect("the run went quiet for 60 s")
            .expect("the event stream closed before the run ended");
        println!(
            "event #{}: {}",
            envelope.seq,
            serde_json::to_string(&envelope.event).unwrap()
        );
        let event = envelope.event;
        if let AgentEvent::ToolApprovalRequired {
            run_id, call_id, ..
        } = &event
        {
            let answered = agent_approval_answer_impl(
                state,
                run_id,
                call_id,
                ApprovalAnswer {
                    decision,
                    scope: ApprovalScope::Once,
                    message: None,
                    edited_arguments: None,
                    allow_network_once: false,
                },
            )
            .unwrap();
            assert!(answered, "nothing was waiting for call {call_id}");
        }
        let ended = match &event {
            AgentEvent::Done { run, stop_reason } => Some(Outcome {
                stop_reason: *stop_reason,
                iterations: run.iterations,
            }),
            AgentEvent::Error {
                error_kind,
                message,
                ..
            } => {
                panic!("the run failed: {error_kind}: {message}")
            }
            _ => None,
        };
        events.push(event);
        if let Some(outcome) = ended {
            break outcome;
        }
    };
    assert!(agent_run_unsubscribe_impl(state, subscription));
    (session.id, events, outcome)
}

fn print_messages(state: &AppState, session_id: &str) -> Vec<(AgentRole, Vec<HubPart>)> {
    let detail = agent_session_get_impl(state, session_id, Default::default()).unwrap();
    println!("session title: {:?}", detail.session.title);
    detail
        .messages
        .into_iter()
        .map(|message| {
            println!(
                "stored #{} {:?} model={:?} error={:?}: {}",
                message.seq,
                message.role,
                message.model,
                message.error,
                serde_json::to_string(&message.parts).unwrap()
            );
            (message.role, message.parts)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_runs_an_mcp_tool_after_approval_and_stores_the_whole_turn() {
    if !python_available() {
        println!("python3 is unavailable: skipping the agent + MCP smoke test");
        return;
    }
    let root = TempDir::new("agent");
    let upstream = Upstream::start(openai_chat);
    let state = Arc::new(AppState::open(root.join("pingdeck.sqlite3")).unwrap());
    configure_llm(&state, upstream.port);

    let mut server = agent_mcp_server_save_impl(
        &state,
        AgentMcpServer {
            id: String::new(),
            name: "smoke".into(),
            transport: McpTransportConfig::Stdio {
                command: "python3".into(),
                args: vec!["-c".into(), FAKE_MCP_SERVER.into()],
                env: Default::default(),
                env_allow: Vec::new(),
                cwd: None,
            },
            enabled: true,
            trusted: false,
            updated_at_ms: 0,
        },
        true,
    )
    .unwrap();
    // Reading the tool list never starts a server: "Refresh tools" connects it.
    pingdeck_core::app::agent_mcp_server_restart_impl(&state, &server.id)
        .await
        .unwrap();
    let tools = agent_tool_list_impl(&state, None).await.unwrap();
    let echo = tools.iter().find(|tool| tool.name == ECHO_TOOL).unwrap();
    println!("tool: {echo:?}");
    assert!(echo.available);
    let others: Vec<String> = tools
        .iter()
        .filter(|tool| tool.name != ECHO_TOOL)
        .map(|tool| tool.name.clone())
        .collect();

    // --- 1. Untrusted server, approval allowed: the hints are ignored, so it asks as a
    // destructive, not read-only tool, and the whole turn is stored.
    let (session_id, events, outcome) = run_turn(&state, &others, ApprovalDecision::Allow).await;
    assert_eq!(outcome.stop_reason, AgentStopReason::Completed);
    assert_eq!(outcome.iterations, 2);
    let approval = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolApprovalRequired {
                name,
                arguments,
                read_only,
                destructive,
                ..
            } => Some((name.as_str(), arguments.as_str(), *read_only, *destructive)),
            _ => None,
        })
        .expect("no approval was requested");
    assert_eq!(
        approval,
        (ECHO_TOOL, r#"{"msg":"hello from the model"}"#, false, true)
    );
    assert!(events.iter().any(|event| matches!(event,
        AgentEvent::ToolResult { status: ToolCallStatus::Ok, parts, .. } if enveloped(&text_of(parts), ECHOED))));
    let streamed: String = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(streamed, FINAL_TEXT);
    assert!(matches!(events.last(), Some(AgentEvent::Done { .. })));
    assert!(matches!(
        events.first(),
        Some(AgentEvent::RunStarted { .. })
    ));

    let messages = print_messages(&state, &session_id);
    assert_eq!(
        messages.iter().map(|(role, _)| *role).collect::<Vec<_>>(),
        [
            AgentRole::User,
            AgentRole::Assistant,
            AgentRole::Tool,
            AgentRole::Assistant
        ]
    );
    assert!(
        matches!(&messages[0].1[..], [HubPart::Text { text, .. }] if text == "Please echo hello")
    );
    assert!(matches!(&messages[1].1[..],
        [HubPart::ToolCall { id, name, arguments, .. }]
            if id == "tc_1" && name == ECHO_TOOL && arguments == r#"{"msg":"hello from the model"}"#));
    assert!(matches!(&messages[2].1[..],
        [HubPart::ToolResult { call_id, is_error: false, content, .. }]
            if call_id == "tc_1"
                && matches!(&content[..], [HubPart::Text { text, .. }] if enveloped(text, ECHOED))));
    assert!(matches!(&messages[3].1[..], [HubPart::Text { text, .. }] if text == FINAL_TEXT));

    // What the model saw: the tool offered first, then the tool result fed back.
    let seen = upstream.requests();
    assert_eq!(seen.len(), 2);
    assert!(
        seen[0].body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == ECHO_TOOL),
        "the MCP tool is offered to the model"
    );
    let fed_back = seen[1].body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .expect("the second request carries the tool result");
    assert!(
        fed_back["content"]
            .to_string()
            .contains("hello from the model")
    );
    for request in &seen {
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some(format!("Bearer {UPSTREAM_SECRET}").as_str())
        );
    }

    // --- 2. Trusted server: its read-only hint is believed, yet `ask` still asks.
    server.trusted = true;
    let server = agent_mcp_server_save_impl(&state, server, true).unwrap();
    // A saved change drops the connection; "Refresh tools" lists and caches them again.
    pingdeck_core::app::agent_mcp_server_restart_impl(&state, &server.id)
        .await
        .unwrap();
    let tools = agent_tool_list_impl(&state, None).await.unwrap();
    assert!(
        tools
            .iter()
            .any(|tool| tool.name == ECHO_TOOL && tool.available)
    );
    let (_, events, outcome) = run_turn(&state, &others, ApprovalDecision::Allow).await;
    assert_eq!(outcome.stop_reason, AgentStopReason::Completed);
    let asked_read_only = events.iter().find_map(|event| match event {
        AgentEvent::ToolApprovalRequired { read_only, .. } => Some(*read_only),
        _ => None,
    });
    assert_eq!(
        asked_read_only,
        Some(true),
        "a trusted server's hint counts"
    );

    // --- 3. Denied: the model gets an error result and still answers; nothing was echoed.
    let (session_id, events, outcome) = run_turn(&state, &others, ApprovalDecision::Deny).await;
    assert_eq!(outcome.stop_reason, AgentStopReason::Completed);
    let denied = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolResult { status, parts, .. } => Some((*status, text_of(parts))),
            _ => None,
        })
        .unwrap();
    println!("denied tool result: {denied:?}");
    assert!(denied.0 == ToolCallStatus::Denied && !denied.1.contains("hello from the model"));
    let messages = print_messages(&state, &session_id);
    assert_eq!(messages.len(), 4);
    assert!(matches!(
        &messages[2].1[..],
        [HubPart::ToolResult { is_error: true, .. }]
    ));
    assert!(matches!(&messages[3].1[..], [HubPart::Text { text, .. }] if text == FINAL_TEXT));

    state.shutdown().await;
}

/// G6: in a session bound to a temp folder, the model reads a file (a read runs without
/// asking), then edits it; the edit asks with a diff card, is allowed, and lands on disk.
#[tokio::test(flavor = "multi_thread")]
async fn golden_g6_agent_edits_a_file_in_its_folder_after_approval() {
    use common::openai_reply;
    use pingdeck_core::agent::model::ToolApprovalPreview;

    let root = TempDir::new("golden-g6");
    // Apart from the data folder: the agent may never touch PingDeck's own files.
    let project = TempDir::new("golden-g6-project");
    let folder = project.path().to_path_buf();
    std::fs::write(folder.join("notes.txt"), "status: draft\n").unwrap();
    // Read first (the edit guard wants it), then edit, then answer.
    let upstream = Upstream::start(|_, request| {
        let results = request.body["messages"].as_array().map_or(0, |messages| {
            messages.iter().filter(|m| m["role"] == "tool").count()
        });
        let call = match results {
            0 => Some(("tc_read", "read_file", r#"{"path":"notes.txt"}"#)),
            1 => Some((
                "tc_edit",
                "edit_file",
                r#"{"path":"notes.txt","old_string":"draft","new_string":"final"}"#,
            )),
            _ => None,
        };
        openai_reply(&request.body, call)
    });
    let state = Arc::new(AppState::open(root.join("pingdeck.sqlite3")).unwrap());
    configure_llm(&state, upstream.port);

    let input = AgentSessionInput {
        workspace_dir: Some(folder.display().to_string()),
        permission_mode: Some(ToolApproval::AutoReadOnly),
        ..AgentSessionInput::default()
    };
    let (session_id, events, outcome) = run_session(&state, input, ApprovalDecision::Allow).await;
    assert_eq!(outcome.stop_reason, AgentStopReason::Completed);
    assert_eq!(outcome.iterations, 3);

    let asked: Vec<(&str, Option<&ToolApprovalPreview>)> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolApprovalRequired { name, preview, .. } => {
                Some((name.as_str(), preview.as_ref()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(asked.len(), 1, "only the edit asks: {asked:?}");
    let (name, preview) = asked[0];
    assert_eq!(name, "edit_file");
    let Some(ToolApprovalPreview::Diff { path, unified_diff }) = preview else {
        panic!("the edit card is not a diff: {preview:?}");
    };
    assert_eq!(path, "notes.txt");
    assert!(
        unified_diff.contains("-status: draft") && unified_diff.contains("+status: final"),
        "{unified_diff}"
    );
    let results: Vec<ToolCallStatus> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolResult { status, .. } => Some(*status),
            _ => None,
        })
        .collect();
    assert_eq!(results, [ToolCallStatus::Ok, ToolCallStatus::Ok]);
    assert_eq!(
        std::fs::read_to_string(folder.join("notes.txt")).unwrap(),
        "status: final\n"
    );

    let roles: Vec<AgentRole> = print_messages(&state, &session_id)
        .into_iter()
        .map(|(role, _)| role)
        .collect();
    assert_eq!(
        roles,
        [
            AgentRole::User,
            AgentRole::Assistant,
            AgentRole::Tool,
            AgentRole::Assistant,
            AgentRole::Tool,
            AgentRole::Assistant
        ]
    );
    assert_eq!(upstream.requests().len(), 3);
    state.shutdown().await;
}
