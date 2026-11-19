//! Headless platform smoke test through the public `pingdeck_core` API: settings persistence
//! across a reopen, file permissions, MCP stdio
//! discovery and calls, and a config-sync round trip. Run with `--nocapture` to see the
//! observed values.
//!
//! Golden paths (`golden_*`): G1 send and history, G2 request assertions in a flow, G3 mock rule
//! selection. They pin today's behaviour.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{FAKE_MCP_SERVER, TempDir, python_available};
use pingdeck_core::agent::mcp::{McpConnection, McpTransportConfig};
use pingdeck_core::agent::model::AgentMcpServer;
use pingdeck_core::app::{
    AppState, EnvironmentVariableInput, SaveEnvironmentInput, agent_mcp_server_restart_impl,
    agent_mcp_server_save_impl, agent_mcp_server_test_impl, agent_tool_list_impl,
    environment_list_impl, environment_save_impl, settings_get_impl, settings_update_impl,
    sync_export_impl, sync_import_impl, sync_preview_impl, workspace_create_impl,
    workspace_list_impl,
};
use pingdeck_core::model::settings::SettingsSection;
use pingdeck_core::platform::create_private_dir_all;
use pingdeck_core::sync::{SyncChange, SyncImportOptions, SyncKind};
use serde_json::{Value, json};

/// Golden path G8 (skeleton): settings, a workspace and a vault secret survive a reopen, and a
/// database already at the current schema gets no pre-migration snapshot.
#[test]
fn golden_g8_data_survives_a_reopen_and_the_current_schema_takes_no_snapshot() {
    let root = TempDir::new("g8");
    let db = root.join("pingdeck.sqlite3");
    let state = AppState::open(&db).unwrap();
    workspace_create_impl(&state, "ws_g8".to_owned(), "Kept".to_owned()).unwrap();
    settings_update_impl(&state, SettingsSection::Editor, &json!({ "tab_size": 8 })).unwrap();
    state.secrets().put("g8.token", "g8-plaintext").unwrap();
    drop(state);

    let reopened = AppState::open(&db).unwrap();
    assert_eq!(settings_get_impl(&reopened).editor.tab_size, 8);
    let workspaces = workspace_list_impl(&reopened).unwrap();
    assert!(workspaces.iter().any(|workspace| workspace.name == "Kept"));
    let token = reopened.secrets().get("g8.token").unwrap();
    assert_eq!(token.as_deref(), Some("g8-plaintext"));
    drop(reopened);
}

/// Octal permission bits of `path`.
#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
#[test]
fn files_and_directories_are_created_owner_only() {
    let root = TempDir::new("perms");

    // A fresh database file and its WAL sidecars, while the state is open.
    let db = root.join("data/pingdeck.sqlite3");
    create_private_dir_all(db.parent().unwrap()).unwrap();
    let state = AppState::open(&db).unwrap();
    workspace_create_impl(&state, "ws1".to_owned(), "Main".to_owned()).unwrap();
    for entry in fs::read_dir(db.parent().unwrap()).unwrap() {
        let path = entry.unwrap().path();
        println!("{} mode {:o}", path.display(), mode_of(&path));
        assert_eq!(
            mode_of(&path) & 0o077,
            0,
            "{} is group/world accessible",
            path.display()
        );
    }
    assert_eq!(mode_of(&db), 0o600);
    drop(state);
}

// `state.agent` is crate-private, so `tools/call` goes through the public `McpConnection` the
// agent runner uses; `tools/list` goes through the two public use-cases.
#[tokio::test(flavor = "multi_thread")]
async fn mcp_stdio_server_is_listed_tested_and_callable() {
    if !python_available() {
        println!("python3 is unavailable: skipping the MCP stdio smoke test");
        return;
    }
    let state = AppState::open_in_memory().unwrap();
    let transport = McpTransportConfig::Stdio {
        command: "python3".to_owned(),
        args: vec!["-c".to_owned(), FAKE_MCP_SERVER.to_owned()],
        env: Default::default(),
        env_allow: Vec::new(),
        cwd: None,
    };
    let server = agent_mcp_server_save_impl(
        &state,
        AgentMcpServer {
            id: String::new(),
            name: "smoke".to_owned(),
            transport: transport.clone(),
            enabled: true,
            trusted: false,
            updated_at_ms: 0,
        },
        true,
    )
    .unwrap();

    let server_id = server.id.clone();
    let tested = agent_mcp_server_test_impl(&state, server).await.unwrap();
    println!("mcp test: {tested:?}");
    assert!(tested.ok, "{tested:?}");
    assert_eq!(tested.server.as_deref(), Some("smoke-fake 0.1"));
    assert_eq!(tested.tool_names, ["echo"]);

    // Reading the tool list never starts a server: "Refresh tools" connects it.
    agent_mcp_server_restart_impl(&state, &server_id)
        .await
        .unwrap();
    let tools = agent_tool_list_impl(&state, None).await.unwrap();
    let mcp: Vec<_> = tools.iter().filter(|tool| tool.source == "smoke").collect();
    println!(
        "tool list: {} tools, mcp: {:?}",
        tools.len(),
        mcp.iter().map(|tool| &tool.name).collect::<Vec<_>>()
    );
    assert_eq!(mcp.len(), 1);
    assert_eq!(mcp[0].name, "mcp__smoke__echo");
    assert!(mcp[0].available);
    assert!(
        !mcp[0].read_only && mcp[0].destructive,
        "untrusted servers ask"
    );

    let connection = McpConnection::connect(&transport, None, None, Duration::from_secs(10))
        .await
        .unwrap();
    let called = connection
        .call_tool(
            "echo",
            json!({ "msg": "hi" }),
            Duration::from_secs(10),
            None,
        )
        .await
        .unwrap();
    println!("tools/call echo: {called:?}");
    assert!(!called.is_error);
    assert_eq!(called.text(), r#"{"msg": "hi"}"#);
}

/// The one exported file under `dir/workspaces/*/environments/`.
fn environment_file(dir: &Path) -> PathBuf {
    let workspaces = fs::read_dir(dir.join("workspaces")).unwrap();
    let workspace = workspaces
        .map(|entry| entry.unwrap().path())
        .next()
        .unwrap();
    let mut files: Vec<PathBuf> = fs::read_dir(workspace.join("environments"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    files.remove(0)
}

fn variable(key: &str, value: &str, secret: bool) -> EnvironmentVariableInput {
    EnvironmentVariableInput {
        id: None,
        key: key.to_owned(),
        value: Some(value.to_owned()),
        is_secret: secret,
        enabled: true,
        note: String::new(),
    }
}

#[test]
fn sync_export_edit_preview_import_round_trip() {
    let root = TempDir::new("sync");
    let state = AppState::open(root.join("pingdeck.sqlite3")).unwrap();
    workspace_create_impl(&state, "ws1".to_owned(), "Main".to_owned()).unwrap();
    environment_save_impl(
        &state,
        SaveEnvironmentInput {
            id: "env1".to_owned(),
            workspace_id: "ws1".to_owned(),
            name: "Dev".to_owned(),
            variables: vec![
                variable("BASE", "http://dev", false),
                variable("API_KEY", "super-secret-value", true),
            ],
            confirm_before_send: None,
            confirm_safe_methods: None,
            declared_hosts: None,
            base_revision: None,
        },
    )
    .unwrap();

    let sync_dir = root.join("synced");
    let exported = sync_export_impl(&state, Some(sync_dir.display().to_string())).unwrap();
    println!("export: {exported:?}");
    let file = environment_file(&sync_dir);
    let text = fs::read_to_string(&file).unwrap();
    assert!(!text.contains("super-secret-value"), "secrets are masked");

    let mut doc: Value = serde_json::from_str(&text).unwrap();
    let variables = doc["environment"]["variables"].as_array_mut().unwrap();
    for variable in variables {
        if variable["key"] == "BASE" {
            variable["value"] = json!("http://edited");
        }
    }
    fs::write(&file, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();

    let preview = sync_preview_impl(&state, Some(sync_dir.display().to_string())).unwrap();
    for entry in &preview.entries {
        println!("preview entry: {entry:?}");
    }
    assert_eq!(preview.entries.len(), 1, "{:?}", preview.entries);
    let entry = &preview.entries[0];
    assert_eq!(
        (entry.kind, entry.key.as_str()),
        (SyncKind::Environment, "env1")
    );
    assert_eq!(entry.change, SyncChange::Changed);

    let result = sync_import_impl(
        &state,
        Some(sync_dir.display().to_string()),
        &SyncImportOptions::default(),
    )
    .unwrap();
    println!("import: {result:?}");
    assert_eq!(result.updated, 1);

    let environments = environment_list_impl(&state, "ws1".to_owned()).unwrap();
    let value_of = |key: &str| {
        environments[0]
            .variables
            .iter()
            .find(|variable| variable.key == key)
            .and_then(|variable| variable.value.clone())
    };
    println!("stored BASE={:?}", value_of("BASE"));
    assert_eq!(value_of("BASE").as_deref(), Some("http://edited"));
    assert_eq!(value_of("API_KEY"), None, "a secret never leaves the core");
    assert_eq!(
        state.secrets().get("env.env1.API_KEY").unwrap().as_deref(),
        Some("super-secret-value"),
        "import refills the placeholder from the local secret"
    );
    drop(state);
}

// ------------------------------------------------------------------------- golden paths

use common::{Reply, Upstream, free_port, request_input};
use pingdeck_core::app::misc_logic_service::history_page_impl;
use pingdeck_core::app::{
    ExecuteDraftRequestInput, ExecuteRequestInput, flow_run_impl, flow_save_impl, history_get_impl,
    mock_server_logs_impl, mock_server_save_impl, mock_server_start_impl, mock_server_stop_impl,
    request_execute_draft_impl, request_execute_impl, request_save_impl,
};
use pingdeck_core::model::events::NoEvents;
use pingdeck_core::model::items::{
    FlowStep, MockResponse, MockResponseMode, MockRoute, MockRouteType, MockRule, MockRuleOperator,
    MockRuleTarget, MockRulesOperator,
};
use pingdeck_core::model::models::Assertion;
use pingdeck_core::model::{Flow, HistoryFilter, MockServer, WorkspaceId};

const GOLDEN_WS: &str = "ws_golden";

/// A state over a temp database with the golden workspace.
fn golden_state(root: &TempDir) -> AppState {
    let state = AppState::open(root.join("pingdeck.sqlite3")).unwrap();
    workspace_create_impl(&state, GOLDEN_WS.into(), "Golden".into()).unwrap();
    state
}

/// A saved GET request to `url`.
fn golden_request(state: &AppState, id: &str, url: String, assertions: Vec<Assertion>) {
    let mut input = request_input(GOLDEN_WS, id, url);
    input.assertions = assertions;
    request_save_impl(state, input).unwrap();
}

async fn send_saved(
    state: &AppState,
    request_id: &str,
) -> pingdeck_core::executor::ExecutionResult {
    request_execute_impl(
        state,
        ExecuteRequestInput {
            workspace_id: GOLDEN_WS.into(),
            request_id: request_id.into(),
            environment_id: None,
            confirmed: None,
        },
        None,
    )
    .await
    .unwrap()
}

/// G1: a saved request is sent and the history keeps a full record of the exchange.
#[tokio::test]
async fn golden_g1_send_writes_a_full_history_record() {
    let root = TempDir::new("golden-g1");
    let upstream = Upstream::start(|_, _| Reply::Json(200, json!({"id": 7})));
    let state = golden_state(&root);
    golden_request(
        &state,
        "req_g1",
        format!("http://127.0.0.1:{}/users/7?expand=1", upstream.port),
        Vec::new(),
    );

    let response = send_saved(&state, "req_g1").await;
    assert_eq!(
        (response.status_code, response.body_text.as_str()),
        (200, r#"{"id":7}"#)
    );
    let seen = upstream.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        (seen[0].method.as_str(), seen[0].path.as_str()),
        ("GET", "/users/7?expand=1")
    );

    let page = history_page_impl(
        &state,
        GOLDEN_WS.into(),
        HistoryFilter::default(),
        Some(10),
        None,
    )
    .unwrap();
    assert_eq!(page.entries.len(), 1);
    let entry = &page.entries[0];
    assert_eq!(
        (
            entry.request_id.as_ref().map(|id| id.as_str()),
            entry.status_code,
            entry.response_body_preview.as_str(),
        ),
        (Some("req_g1"), 200, r#"{"id":7}"#)
    );
    let detail = history_get_impl(&state, GOLDEN_WS.into(), entry.id.as_str().to_owned()).unwrap();
    assert_eq!(detail.request_snapshot.unwrap().id.as_str(), "req_g1");
    assert_eq!(
        (response.history_id.as_deref(), response.history_saved),
        (Some(entry.id.as_str()), true)
    );
}

/// One flow step running the saved request `request_id`, with its own `assertions`.
fn golden_step(id: &str, position: i64, request_id: &str, assertions: Vec<Assertion>) -> FlowStep {
    FlowStep {
        request_id: request_id.into(),
        assertions,
        ..serde_json::from_value(json!({
            "id": id, "name": id, "delay_ms": 0, "continue_on_error": false, "position": position
        }))
        .unwrap()
    }
}

/// G2: a saved request with assertions is sent once, then run in a flow. A step without
/// assertions of its own is judged by the request's and fails like the single send; a step
/// that ignores the request's assertions and carries the same ones matches it verdict for verdict.
#[tokio::test]
async fn golden_g2_request_assertions_single_send_and_flow() {
    let root = TempDir::new("golden-g2");
    let upstream = Upstream::start(|_, _| Reply::Json(200, json!({"name": "Ada"})));
    let state = golden_state(&root);
    let assertions = vec![
        Assertion::StatusEquals { value: 200 },
        Assertion::JsonPathEquals {
            path: "name".into(),
            value: json!("Bob"),
        },
    ];
    golden_request(
        &state,
        "req_g2",
        format!("http://127.0.0.1:{}/profile", upstream.port),
        assertions.clone(),
    );

    let single = send_saved(&state, "req_g2").await;
    let verdicts = |results: &[pingdeck_core::model::assertion::AssertionResult]| -> Vec<bool> {
        results.iter().map(|result| result.passed).collect()
    };
    assert_eq!(verdicts(&single.assertion_results), [true, false]);

    flow_save_impl(
        &state,
        Flow {
            id: "flow_g2".into(),
            workspace_id: WorkspaceId::new(GOLDEN_WS).unwrap(),
            name: "Golden G2".into(),
            description: String::new(),
            steps: vec![
                FlowStep {
                    continue_on_error: true,
                    ..golden_step("step_plain", 0, "req_g2", Vec::new())
                },
                FlowStep {
                    ignore_request_assertions: true,
                    ..golden_step("step_checked", 1, "req_g2", assertions)
                },
            ],
            updated_at_ms: 0,
            options: Default::default(),
        },
    )
    .unwrap();
    let run = flow_run_impl(
        &state,
        GOLDEN_WS.into(),
        "flow_g2".into(),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        &NoEvents,
    )
    .await
    .unwrap();
    println!("flow run: passed={} steps={:?}", run.passed, run.steps);
    assert_eq!(run.steps.len(), 2);
    // A step without assertions of its own is judged by the request's.
    let plain = &run.steps[0];
    assert!(!plain.passed && plain.request_assertion_count == 2);
    assert_eq!(
        verdicts(&plain.assertion_results),
        verdicts(&single.assertion_results)
    );
    let checked = &run.steps[1];
    assert_eq!(checked.status_code, Some(200));
    assert_eq!(
        verdicts(&checked.assertion_results),
        verdicts(&single.assertion_results)
    );
    assert!(!checked.passed && !run.passed);
    assert_eq!(upstream.requests().len(), 3);
}

/// A response of the golden mock route: `rules` on the query key `tier`.
fn golden_response(id: &str, position: i64, status: u16, rule: Option<&str>) -> MockResponse {
    MockResponse {
        id: id.into(),
        label: id.into(),
        status_code: status,
        headers: Vec::new(),
        body: format!(r#"{{"plan":"{id}"}}"#),
        body_source: None,
        fault: None,
        latency_ms: 0,
        is_default: rule.is_none(),
        rules_operator: MockRulesOperator::And,
        rules: rule
            .map(|value| MockRule {
                id: format!("rule_{id}"),
                target: MockRuleTarget::Query,
                modifier: "tier".into(),
                operator: MockRuleOperator::Equals,
                value: value.into(),
                invert: false,
                position: 0,
            })
            .into_iter()
            .collect(),
        callback_ids: Vec::new(),
        position,
        stream: None,
    }
}

/// G3: a mock route with a rule-bound response and a default is started, and requests the
/// app sends to it get the response their query selects; the traffic log names it.
#[tokio::test]
async fn golden_g3_mock_route_selects_the_response_by_rule() {
    let root = TempDir::new("golden-g3");
    let state = golden_state(&root);
    let port = free_port(state.settings().mock.first_port);
    let server: MockServer = MockServer {
        id: "mock_g3".into(),
        workspace_id: WorkspaceId::new(GOLDEN_WS).unwrap(),
        name: "Golden mock".into(),
        port,
        endpoint_prefix: String::new(),
        latency_ms: 0,
        cors_enabled: false,
        headers: Vec::new(),
        routes: vec![MockRoute {
            id: "route_plans".into(),
            method: "GET".into(),
            path: "/plans".into(),
            enabled: true,
            description: String::new(),
            route_type: MockRouteType::Http,
            data_bucket_id: String::new(),
            crud_id_key: "id".into(),
            response_mode: MockResponseMode::FirstMatch,
            responses: vec![
                golden_response("gold", 0, 200, Some("gold")),
                golden_response("basic", 1, 202, None),
            ],
            position: 0,
        }],
        data_buckets: Vec::new(),
        callbacks: Vec::new(),
        proxy_enabled: false,
        proxy_base_url: String::new(),
        proxy_remove_prefix: false,
        proxy_req_headers: Vec::new(),
        proxy_res_headers: Vec::new(),
        updated_at_ms: 0,
        environment_id: None,
    };
    mock_server_save_impl(&state, server).unwrap();
    let started = mock_server_start_impl(&state, GOLDEN_WS.into(), "mock_g3".into()).unwrap();
    assert!(started.running && started.port == port);

    for (tier, status, plan) in [("gold", 200, "gold"), ("silver", 202, "basic")] {
        let response = request_execute_draft_impl(
            &state,
            ExecuteDraftRequestInput {
                request: request_input(
                    GOLDEN_WS,
                    &format!("draft_{tier}"),
                    format!("http://127.0.0.1:{port}/plans?tier={tier}"),
                ),
                environment_id: None,
                confirmed: None,
                save_to_history: None,
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            (response.status_code, response.body_text),
            (status, format!(r#"{{"plan":"{plan}"}}"#)),
            "tier={tier}"
        );
    }

    let logs = mock_server_logs_impl(&state, GOLDEN_WS.into(), "mock_g3".into()).unwrap();
    let mut matched: Vec<(String, Option<String>, Option<String>)> = logs
        .into_iter()
        .map(|log| (log.query, log.matched_route_id, log.matched_response_id))
        .collect();
    matched.sort();
    assert_eq!(
        matched,
        [
            (
                "tier=gold".to_owned(),
                Some("route_plans".to_owned()),
                Some("gold".to_owned())
            ),
            (
                "tier=silver".to_owned(),
                Some("route_plans".to_owned()),
                Some("basic".to_owned())
            ),
        ]
    );
    assert!(
        !mock_server_stop_impl(&state, GOLDEN_WS.into(), "mock_g3".into())
            .unwrap()
            .running
    );
}
