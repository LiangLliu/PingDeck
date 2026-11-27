//! Mock server, certificate and flow use-case tests.

use super::support::*;
use crate::app::{
    AppState, ExecuteDraftRequestInput, SaveCollectionInput, TlsCertificateDto,
    certificate_delete_impl, certificate_list_impl, certificate_save_impl, collection_save_impl,
    cookie_jar_list_impl, execution_cancel_impl, flow_delete_impl, flow_list_impl, flow_run_impl,
    flow_save_impl, mock_server_clear_logs_impl, mock_server_delete_impl,
    mock_server_import_mockoon_impl, mock_server_list_impl, mock_server_logs_impl,
    mock_server_save_impl, mock_server_start_impl, mock_server_status_list_impl,
    mock_server_stop_impl, request_execute_draft_impl, workspace_create_impl,
    workspace_delete_impl,
};
use crate::error::AppError;
use crate::workspace::models::{TlsCertificateId, TlsCertificateKind};
use crate::workspace::{
    Assertion, CollectionId, Environment, EnvironmentId, Flow, FlowExtraction,
    FlowExtractionSource, FlowStep, HistorySource, HttpMethod, KeyValue, Request, WorkspaceId,
};

const OK_RESPONSE: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";

fn state_with_workspaces(ids: &[&str]) -> AppState {
    let state = AppState::open_in_memory().expect("state should open");
    for id in ids {
        workspace_create_impl(&state, (*id).into(), (*id).into()).unwrap();
    }
    state
}

fn request_step(id: &str, position: i64, request_id: &str) -> FlowStep {
    FlowStep {
        request_id: request_id.into(),
        ..serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "delay_ms": 0, "continue_on_error": false, "position": position
        }))
        .unwrap()
    }
}

fn inline_step(id: &str, position: i64, method: HttpMethod, url: &str) -> FlowStep {
    FlowStep {
        method_override: Some(method),
        url_override: Some(url.into()),
        ..request_step(id, position, "")
    }
}

fn flow_of(workspace_id: &str, id: &str, name: &str, steps: Vec<FlowStep>) -> Flow {
    Flow {
        id: id.into(),
        workspace_id: WorkspaceId::new(workspace_id).unwrap(),
        name: name.into(),
        description: String::new(),
        steps,
        updated_at_ms: 0,
        options: Default::default(),
    }
}

fn scripted_flow(workspace_id: &str, request_ids: &[&str]) -> Flow {
    let steps = request_ids
        .iter()
        .enumerate()
        .map(|(i, req_id)| request_step(&format!("step_{i}"), i as i64, req_id))
        .collect();
    flow_of(workspace_id, "flow_scripted", "Scripted flow", steps)
}

async fn run_flow_with(
    state: &AppState,
    workspace_id: &str,
    flow_id: &str,
    environment_id: Option<&str>,
    max_steps: Option<usize>,
    confirmed: Option<bool>,
) -> Result<crate::workspace::FlowRunResult, AppError> {
    flow_run_impl(
        state,
        workspace_id.into(),
        flow_id.into(),
        environment_id.map(str::to_owned),
        None,
        max_steps,
        None,
        None,
        None,
        confirmed,
        &crate::events::NoEvents,
    )
    .await
}

#[tokio::test]
async fn mock_server_commands_start_hot_reload_log_stop_all_and_delete() {
    let mut bad_latency = mock_server_fixture("ws_mock_cmd", 3000);
    bad_latency.routes[0].responses[0].latency_ms = 60_001;
    let err = crate::app::mock_service::validate_mock_server(&bad_latency).unwrap_err();
    assert!(
        matches!(err, AppError::Validation { .. })
            && err.message().contains("latency cannot exceed 60000 ms")
    );

    let state = state_with_workspaces(&["ws_mock_cmd", "ws_mock_delete"]);
    let port = free_port();
    let saved = mock_server_save_impl(&state, mock_server_fixture("ws_mock_cmd", port)).unwrap();
    assert!(saved.updated_at_ms > 0);
    assert_eq!(
        mock_server_list_impl(&state, "ws_mock_cmd".into())
            .unwrap()
            .len(),
        1
    );

    let started = mock_server_start_impl(&state, "ws_mock_cmd".into(), "mock_cmd".into()).unwrap();
    assert!(started.running && started.port == port);
    assert_eq!(started.bound_url, format!("http://127.0.0.1:{port}"));

    let (status, body) = http_get(port, "/hello?who=ada");
    assert_eq!((status, body.as_str()), (200, r#"{"hello":"ada"}"#));

    let logs = mock_server_logs_impl(&state, "ws_mock_cmd".into(), "mock_cmd".into()).unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].path, "/hello");
    assert!(mock_server_status_list_impl(&state, "ws_mock_cmd".into()).unwrap()[0].running);

    let mut reloaded = saved;
    reloaded.routes[0].responses[0].body = r#"{"hello":"reloaded"}"#.into();
    mock_server_save_impl(&state, reloaded.clone()).unwrap();
    let (status, body) = http_get(port, "/hello?who=ignored");
    assert_eq!((status, body.as_str()), (200, r#"{"hello":"reloaded"}"#));

    mock_server_clear_logs_impl(&state, "ws_mock_cmd".into(), "mock_cmd".into()).unwrap();
    assert!(
        mock_server_logs_impl(&state, "ws_mock_cmd".into(), "mock_cmd".into())
            .unwrap()
            .is_empty()
    );

    reloaded.routes[0].method = "POST".into();
    reloaded.routes[0].responses[0].body = r#"{"hello":"{{query.who}}"}"#.into();
    mock_server_save_impl(&state, reloaded).unwrap();
    for url in [
        format!("localhost:{port}/hello"),
        format!("127.0.0.1:{port}/hello"),
        format!("http://localhost:{port}/hello"),
    ] {
        let mut input = draft_request_input("req_mock_post", "ws_mock_cmd", url.clone());
        input.method = HttpMethod::Post;
        let response = request_execute_draft_impl(
            &state,
            ExecuteDraftRequestInput {
                request: input,
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            (response.status_code, response.body_text.as_str()),
            (200, r#"{"hello":""}"#)
        );
    }
    let post_logs = mock_server_logs_impl(&state, "ws_mock_cmd".into(), "mock_cmd".into()).unwrap();
    assert_eq!(post_logs.len(), 3);
    assert!(
        post_logs
            .iter()
            .all(|log| log.method == "POST" && log.request_body.is_empty())
    );

    let stopped = mock_server_stop_impl(&state, "ws_mock_cmd".into(), "mock_cmd".into()).unwrap();
    assert!(!stopped.running);
    mock_server_delete_impl(&state, "ws_mock_cmd".into(), "mock_cmd".into()).unwrap();

    let mockoon = serde_json::json!({
        "name": "Imported",
        "port": free_port(),
        "routes": [{
            "type": "http",
            "method": "get",
            "endpoint": "ping",
            "responses": [{ "statusCode": 200, "body": "pong" }]
        }]
    });
    let imported =
        mock_server_import_mockoon_impl(&state, "ws_mock_cmd".into(), mockoon.to_string()).unwrap();
    assert_eq!(imported.server.name, "Imported");
    assert_eq!(
        mock_server_list_impl(&state, "ws_mock_cmd".into()).unwrap()[0].id,
        imported.server.id
    );
    let imported_id = imported.server.id.clone();
    mock_server_start_impl(&state, "ws_mock_cmd".into(), imported_id.clone()).unwrap();
    let stopped = mock_server_stop_impl(&state, "ws_mock_cmd".into(), imported_id).unwrap();
    assert!(!stopped.running);

    let del_port = free_port();
    mock_server_save_impl(&state, mock_server_fixture("ws_mock_delete", del_port)).unwrap();
    mock_server_start_impl(&state, "ws_mock_delete".into(), "mock_cmd".into()).unwrap();
    assert!(state.mocks.is_running("mock_cmd"));
    assert_eq!(http_get(del_port, "/hello?who=ada").0, 200);
    workspace_delete_impl(&state, "ws_mock_delete".into()).unwrap();
    assert!(!state.mocks.is_running("mock_cmd"));
    let refused = (0..50).any(|_| {
        let closed = std::net::TcpStream::connect(("127.0.0.1", del_port)).is_err();
        if !closed {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        closed
    });
    assert!(refused);
}

#[tokio::test]
async fn mock_and_flow_commands_reject_missing_or_foreign_workspaces_without_side_effects() {
    let state = state_with_workspaces(&["ws_owner", "ws_other"]);
    let port = free_port();
    mock_server_save_impl(&state, mock_server_fixture("ws_owner", port)).unwrap();

    let absent_start =
        mock_server_start_impl(&state, "ws_owner".into(), "absent".into()).unwrap_err();
    assert!(
        matches!(absent_start, AppError::NotFound { .. })
            && absent_start.message() == "mock server absent not found"
    );
    let foreign_start =
        mock_server_start_impl(&state, "ws_other".into(), "mock_cmd".into()).unwrap_err();
    assert!(
        matches!(foreign_start, AppError::NotFound { .. })
            && foreign_start.message() == "mock server mock_cmd not found"
    );
    let stopped_clear =
        mock_server_clear_logs_impl(&state, "ws_owner".into(), "mock_cmd".into()).unwrap_err();
    assert!(
        matches!(stopped_clear, AppError::NotFound { .. })
            && stopped_clear.message() == "mock server mock_cmd is not running"
    );
    let absent_flow = run_flow_with(&state, "ws_owner", "absent_flow", None, None, None)
        .await
        .unwrap_err();
    assert!(
        matches!(absent_flow, AppError::NotFound { .. })
            && absent_flow.message() == "flow absent_flow not found"
    );

    mock_server_start_impl(&state, "ws_owner".into(), "mock_cmd".into()).unwrap();
    assert_eq!(http_get(port, "/hello?who=ada").0, 200);

    let del_err =
        mock_server_delete_impl(&state, "ws_other".into(), "mock_cmd".into()).unwrap_err();
    assert!(
        matches!(del_err, AppError::NotFound { .. })
            && del_err
                .message()
                .contains("not found in workspace ws_other")
    );
    assert!(state.mocks.is_running("mock_cmd"));
    assert!(matches!(
        mock_server_stop_impl(&state, "ws_other".into(), "mock_cmd".into()),
        Err(AppError::NotFound { .. })
    ));
    assert!(matches!(
        mock_server_logs_impl(&state, "ws_other".into(), "mock_cmd".into()),
        Err(AppError::NotFound { .. })
    ));
    assert!(matches!(
        mock_server_clear_logs_impl(&state, "ws_other".into(), "mock_cmd".into()),
        Err(AppError::NotFound { .. })
    ));
    assert!(matches!(
        mock_server_stop_impl(&state, "ws_owner".into(), "absent".into()),
        Err(AppError::NotFound { .. })
    ));

    assert_eq!(http_get(port, "/hello?who=ada").0, 200);
    assert_eq!(
        mock_server_logs_impl(&state, "ws_owner".into(), "mock_cmd".into())
            .unwrap()
            .len(),
        2
    );
    mock_server_delete_impl(&state, "ws_owner".into(), "mock_cmd".into()).unwrap();
    assert!(!state.mocks.is_running("mock_cmd"));
}

#[test]
fn certificate_and_item_saves_validate_ids_and_manage_passphrase_secrets() {
    let state = state_with_workspaces(&["ws_pass", "ws_other"]);
    let cert = |id: &str, ws: &str, kind: TlsCertificateKind, passphrase: Option<&str>| {
        TlsCertificateDto {
            id: TlsCertificateId::new(id).unwrap(),
            workspace_id: WorkspaceId::new(ws).unwrap(),
            kind,
            host_pattern: "api.example.com".into(),
            cert_path: "/tmp/cert.pem".into(),
            key_path: (kind == TlsCertificateKind::Client).then(|| "/tmp/client.key".into()),
            has_passphrase: false,
            passphrase: passphrase.map(str::to_owned),
            enabled: true,
        }
    };
    let ca = cert("cert_ca", "ws_pass", TlsCertificateKind::Ca, None);
    certificate_save_impl(&state, ca.clone()).unwrap();
    assert_eq!(
        certificate_list_impl(&state, "ws_pass".into()).unwrap(),
        vec![ca]
    );
    certificate_delete_impl(&state, "ws_pass".into(), "cert_ca".into()).unwrap();
    assert!(
        certificate_list_impl(&state, "ws_pass".into())
            .unwrap()
            .is_empty()
    );

    let secret_ref = "tls.ws_pass.cert_p.passphrase";
    let stored = || state.secrets().get(secret_ref).unwrap();
    let saved = certificate_save_impl(
        &state,
        cert(
            "cert_p",
            "ws_pass",
            TlsCertificateKind::Client,
            Some("correct horse"),
        ),
    )
    .unwrap();
    assert!(saved.has_passphrase && saved.passphrase.is_none());
    assert_eq!(stored().as_deref(), Some("correct horse"));
    let json = serde_json::to_string(&saved).unwrap();
    assert!(
        !json.contains("correct horse")
            && !json.contains("passphrase_ref")
            && json.contains("\"has_passphrase\":true")
    );

    certificate_save_impl(
        &state,
        cert(
            "cert_p",
            "ws_other",
            TlsCertificateKind::Client,
            Some("orphan me"),
        ),
    )
    .unwrap_err();
    assert_eq!(
        state
            .secrets()
            .get("tls.ws_other.cert_p.passphrase")
            .unwrap(),
        None
    );

    let mut edited = cert("cert_p", "ws_pass", TlsCertificateKind::Client, None);
    edited.host_pattern = "api2.example.com".into();
    assert!(
        certificate_save_impl(&state, edited)
            .unwrap()
            .has_passphrase
    );
    assert_eq!(stored().as_deref(), Some("correct horse"));

    certificate_save_impl(
        &state,
        cert(
            "cert_p",
            "ws_pass",
            TlsCertificateKind::Client,
            Some("another one"),
        ),
    )
    .unwrap();
    assert_eq!(stored().as_deref(), Some("another one"));
    let cleared = certificate_save_impl(
        &state,
        cert("cert_p", "ws_pass", TlsCertificateKind::Client, Some("")),
    )
    .unwrap();
    assert!(!cleared.has_passphrase && stored().is_none());

    certificate_save_impl(
        &state,
        cert(
            "cert_p",
            "ws_pass",
            TlsCertificateKind::Client,
            Some("again please"),
        ),
    )
    .unwrap();
    certificate_delete_impl(&state, "ws_pass".into(), "cert_p".into()).unwrap();
    assert_eq!(stored(), None);

    let raw_cert = |id: &str, ws: &str| -> TlsCertificateDto {
        serde_json::from_value(serde_json::json!({
            "id": id, "workspace_id": ws, "kind": "Ca", "host_pattern": "",
            "cert_path": "/tmp/ca.pem",
        }))
        .unwrap()
    };
    for (id, ws) in [
        ("", "ws_pass"),
        ("  ", "ws_pass"),
        ("cert_1", ""),
        ("cert_1", "  "),
    ] {
        assert!(matches!(
            certificate_save_impl(&state, raw_cert(id, ws)),
            Err(AppError::Validation { .. })
        ));
    }
    let trimmed = certificate_save_impl(&state, raw_cert(" cert_1 ", " ws_pass ")).unwrap();
    assert_eq!(
        (trimmed.id.as_str(), trimmed.workspace_id.as_str()),
        ("cert_1", "ws_pass")
    );

    let fixture_flow = || {
        flow_of(
            "ws_pass",
            "flow_ids",
            "Ids",
            vec![
                inline_step("step_a", 0, HttpMethod::Get, "http://127.0.0.1:3000/ping"),
                inline_step("step_b", 1, HttpMethod::Get, "http://127.0.0.1:3000/ping"),
            ],
        )
    };
    let mut blank_flow = fixture_flow();
    blank_flow.id = "  ".into();
    let mut blank_step = fixture_flow();
    blank_step.steps[1].id = String::new();
    let mut duplicate_step = fixture_flow();
    duplicate_step.steps[1].id = "step_a".into();
    let mut bad_ws_flow = fixture_flow();
    bad_ws_flow.workspace_id = serde_json::from_value(serde_json::json!("")).unwrap();

    for (flow, expected) in [
        (blank_flow, "flow id cannot be blank"),
        (blank_step, "step id cannot be blank"),
        (duplicate_step, "duplicate step id step_a"),
        (bad_ws_flow, "workspace id cannot be blank"),
    ] {
        let error = flow_save_impl(&state, flow).unwrap_err();
        assert!(
            matches!(error, AppError::Validation { .. }) && error.message().contains(expected),
            "{error}"
        );
    }
    assert!(flow_list_impl(&state, "ws_pass".into()).unwrap().is_empty());
    flow_save_impl(&state, fixture_flow()).unwrap();

    let mut server = mock_server_fixture("ws_pass", free_port());
    server.workspace_id = serde_json::from_value(serde_json::json!("  ")).unwrap();
    assert!(matches!(
        mock_server_save_impl(&state, server),
        Err(AppError::Validation { .. })
    ));
}

#[tokio::test]
async fn flow_run_command_extracts_variables_runs_scripts_streams_events_and_persists_no_cookies() {
    let server = TestServer::spawn_sequence(vec![
        TestServerAction::Respond(
            "HTTP/1.1 200 OK\r\nSet-Cookie: sid=abc; Path=/\r\nContent-Type: application/json\r\nContent-Length: 19\r\n\r\n{\"token\":\"tok-cmd\"}".into(),
        ),
        TestServerAction::Respond(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 14\r\n\r\n{\"name\":\"Ada\"}".into(),
        ),
    ]);
    let state = state_with_workspaces(&["ws_flow_cmd"]);
    collection_save_impl(
        &state,
        SaveCollectionInput {
            id: "col_scripted".into(),
            workspace_id: "ws_flow_cmd".into(),
            name: "Scripted".into(),
            pre_request_script:
                r#"pm.request.headers.add({ key: "X-Script", value: "collection" });"#.into(),
            ..Default::default()
        },
    )
    .unwrap();
    {
        let store = state.store();
        let mut login = Request::new(
            "req_flow_login",
            "ws_flow_cmd",
            "Login",
            format!("{}/login", server.base_url),
        )
        .unwrap();
        login.collection_id = Some(CollectionId::new("col_scripted").unwrap());
        login.test_script = r#"
            console.log("step-script-ran");
            pm.test("flow step ok", () => { pm.response.to.have.status(200); });
            pm.environment.set("session", "sess-1");
            pm.variables.set("page", "7");
        "#
        .into();
        store.save_request(&login).unwrap();

        let mut profile = Request::new(
            "req_flow_profile",
            "ws_flow_cmd",
            "Profile",
            format!("{}/profile", server.base_url),
        )
        .unwrap();
        profile.collection_id = Some(CollectionId::new("col_scripted").unwrap());
        profile.headers = vec![
            KeyValue {
                key: "x-token".into(),
                value: "{{token}}".into(),
                enabled: true,
                description: None,
            },
            KeyValue {
                key: "x-session".into(),
                value: "{{session}}".into(),
                enabled: true,
                description: None,
            },
            KeyValue {
                key: "x-page".into(),
                value: "{{page}}".into(),
                enabled: true,
                description: None,
            },
            KeyValue {
                key: "x-ref".into(),
                value: "ref {{token}}".into(),
                enabled: true,
                description: None,
            },
        ];
        store.save_request(&profile).unwrap();
        let env = Environment::new("env_flow", "ws_flow_cmd", "Flow env").unwrap();
        store.save_environment(&env).unwrap();
    }

    let mut step_1 = request_step("step_1", 0, "req_flow_login");
    step_1.extractions = vec![FlowExtraction {
        variable_name: "token".into(),
        source: FlowExtractionSource::BodyJsonPath,
        expression: "token".into(),
        secret: None,
    }];
    step_1.assertions = vec![Assertion::StatusEquals { value: 200 }];

    let mut step_2 = request_step("step_2", 1, "req_flow_profile");
    step_2.assertions = vec![
        Assertion::StatusEquals { value: 200 },
        Assertion::JsonPathEquals {
            path: "name".into(),
            value: serde_json::Value::String("Ada".into()),
        },
    ];
    flow_save_impl(
        &state,
        flow_of(
            "ws_flow_cmd",
            "flow_cmd",
            "Login then profile",
            vec![step_1, step_2],
        ),
    )
    .unwrap();

    let events = std::sync::Mutex::new(Vec::new());
    let result = flow_run_impl(
        &state,
        "ws_flow_cmd".into(),
        "flow_cmd".into(),
        Some("env_flow".into()),
        None,
        None,
        None,
        None,
        None,
        None,
        &|event: crate::flows::FlowEvent| {
            events.lock().unwrap().push(event);
            true
        },
    )
    .await
    .unwrap();

    let requests = server.received_requests(2);
    assert!(requests[0].contains("x-script: collection"));
    assert!(requests[1].contains("x-script: collection"));
    assert!(requests[1].contains("x-token: tok-cmd") && requests[1].contains("x-session: sess-1"));
    // A value step 1's script set with `pm.variables` reaches step 2 of the same run.
    assert!(requests[1].contains("x-page: 7"));
    // The extracted token is a secret: step 2's record of what it sent masks it.
    let sent = result.steps[1].executed_request.as_ref().unwrap();
    let reference = sent.headers.iter().find(|h| h.key == "x-ref").unwrap();
    assert!(requests[1].contains("x-ref: ref tok-cmd"));
    assert!(!reference.value.contains("tok-cmd"), "{reference:?}");
    assert_eq!(result.secret_variables, ["token"]);
    // The run's cookie session carries step 1's Set-Cookie to step 2 (the jar stays empty).
    assert!(requests[1].to_ascii_lowercase().contains("cookie: sid=abc"));
    assert!(result.passed && result.steps.len() == 2);
    let first = &result.steps[0];
    assert!(first.script_tests.len() == 1 && first.script_tests[0].passed);
    assert!(
        first
            .console_logs
            .iter()
            .any(|entry| entry.message == "step-script-ran")
    );
    assert!(
        result
            .variables
            .iter()
            .any(|v| v.key == "token" && v.value == "tok-cmd")
    );
    let environment = state
        .store()
        .get_environment(
            &WorkspaceId::new("ws_flow_cmd").unwrap(),
            &EnvironmentId::new("env_flow").unwrap(),
        )
        .unwrap()
        .unwrap();
    assert!(
        environment
            .variables
            .iter()
            .any(|v| { v.key == "session" && v.value.as_deref() == Some("sess-1") })
    );
    assert!(
        cookie_jar_list_impl(&state, "ws_flow_cmd".into())
            .unwrap()
            .is_empty()
    );

    let events = events.into_inner().unwrap();
    assert_eq!(
        events.last(),
        Some(&crate::flows::FlowEvent::RunFinished {
            run_id: result.run_id.clone(),
            passed: true,
            cancelled: false,
        })
    );
    let streamed: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            crate::flows::FlowEvent::StepsFinished { run_id, steps, .. } => {
                assert_eq!(run_id, &result.run_id);
                Some(steps.iter().map(|step| step.step_id.clone()))
            }
            crate::flows::FlowEvent::RunFinished { .. } => None,
        })
        .flatten()
        .collect();
    assert_eq!(streamed, ["step_1", "step_2"]);

    let listed = flow_list_impl(&state, "ws_flow_cmd".into()).unwrap();
    let last_run = listed[0].last_run.as_ref().unwrap();
    assert_eq!(last_run.run_id, result.run_id);
    assert!(last_run.passed && !last_run.cancelled);
    assert_eq!((last_run.steps_passed, last_run.steps_total), (2, 2));
    // What a restarted "only this step" seeds from: the environment and the safe variables.
    assert_eq!(last_run.environment_id.as_deref(), Some("env_flow"));
    assert!(
        last_run
            .variables
            .iter()
            .flatten()
            .any(|v| v.key == "page" && v.value == "7")
    );
    assert_eq!(last_run.withheld_variables, ["token"]);

    let history = history_list(&state, "ws_flow_cmd".into(), None, None).unwrap();
    assert_eq!(history.len(), 2);
    // The stored run (what a restart reads) keeps each step and its history row.
    let stored = &state.store().list_flow_runs("flow_cmd").unwrap()[0];
    let mut stored_ids: Vec<_> = stored
        .steps
        .iter()
        .map(|step| step.history_entry_id.clone().unwrap())
        .collect();
    let mut history_ids: Vec<_> = history
        .iter()
        .map(|row| row.id.as_str().to_owned())
        .collect();
    stored_ids.sort();
    history_ids.sort();
    assert_eq!(stored_ids, history_ids);
    assert_eq!(stored.steps[1].assertions_passed, 2);
    for row in &history {
        assert_eq!(row.source, HistorySource::Flow);
        assert_eq!(row.source_name, "Login then profile");
        assert_eq!(row.source_id, "flow_cmd");
        assert_eq!(row.run_id.as_deref(), Some(result.run_id.as_str()));
    }

    flow_delete_impl(&state, "ws_flow_cmd".into(), "flow_cmd".into()).unwrap();
    assert!(
        flow_list_impl(&state, "ws_flow_cmd".into())
            .unwrap()
            .is_empty()
    );
    assert!(state.store().list_flow_runs("flow_cmd").unwrap().is_empty());
}

/// The pre-run review sees the saved request with the environment only; each step is checked
/// again as it is sent, so a step's URL override cannot carry an agent-written credential to a
/// host the environment does not declare.
#[tokio::test]
async fn a_flow_step_aiming_an_agent_written_credential_at_an_undeclared_host_sends_nothing() {
    let state = state_with_workspaces(&["ws_flow_ag"]);
    let closed = format!("127.0.0.1:{}", free_port());
    {
        let store = state.store();
        let mut environment = Environment::new("env_flow_ag", "ws_flow_ag", "Dev").unwrap();
        environment.variables =
            vec![crate::workspace::EnvironmentVariable::masked("token", "tok_ag_1").unwrap()];
        environment.declared_hosts = vec!["api.example.test".to_owned()];
        store.save_environment(&environment).unwrap();
        let mut request = Request::new(
            "r_flow_ag",
            "ws_flow_ag",
            "Agent request",
            "https://api.example.test/x",
        )
        .unwrap();
        request.headers = vec![KeyValue {
            key: "Authorization".into(),
            value: "Bearer {{token}}".into(),
            enabled: true,
            description: None,
        }];
        store.save_request(&request).unwrap();
        store
            .mark_request_agent_touched("ws_flow_ag", "r_flow_ag", 1)
            .unwrap();
    }
    let mut step = request_step("step_1", 0, "r_flow_ag");
    step.url_override = Some(format!("http://{closed}/x"));
    flow_save_impl(
        &state,
        flow_of("ws_flow_ag", "flow_ag", "Agent flow", vec![step]),
    )
    .unwrap();
    let result = run_flow_with(
        &state,
        "ws_flow_ag",
        "flow_ag",
        Some("env_flow_ag"),
        None,
        None,
    )
    .await
    .unwrap();
    let step = &result.steps[0];
    assert!(!step.passed);
    assert!(
        step.error
            .as_deref()
            .is_some_and(|error| error.contains("does not declare") && error.contains(&closed)),
        "{step:?}"
    );
    assert!(step.executed_request.is_none() && step.history_entry_id.is_none());
}

#[tokio::test]
async fn flow_step_retries_a_503_until_it_succeeds_and_every_sent_attempt_leaves_a_history_row() {
    let unavailable = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n";
    let mut actions = vec![
        TestServerAction::Respond(unavailable.into()),
        TestServerAction::Respond(unavailable.into()),
        TestServerAction::Respond(OK_RESPONSE.into()),
    ];
    actions.extend((0..21).map(|_| TestServerAction::Respond(OK_RESPONSE.into())));
    let server = TestServer::spawn_sequence(actions);
    let state = state_with_workspaces(&["ws_flow_retry"]);
    let mut step = inline_step(
        "poll",
        0,
        HttpMethod::Get,
        &format!("{}/status", server.base_url),
    );
    step.retries = 2;
    step.retry_on = vec![crate::workspace::items::FlowRetryOn::Status5xx];
    step.assertions = vec![Assertion::StatusEquals { value: 200 }];
    flow_save_impl(
        &state,
        flow_of(
            "ws_flow_retry",
            "flow_retry",
            "Poll until ready",
            vec![step],
        ),
    )
    .unwrap();

    let result = run_flow_with(&state, "ws_flow_retry", "flow_retry", None, None, None)
        .await
        .unwrap();
    assert!(result.passed && result.steps[0].attempts == 3);
    let history = history_list(&state, "ws_flow_retry".into(), None, None).unwrap();
    assert_eq!(history.len(), 3);
    assert!(history.iter().all(|row| {
        row.source == HistorySource::Flow
            && row.source_name == "Poll until ready"
            && row.run_id.as_deref() == Some(result.run_id.as_str())
    }));

    let mut last_run_id = String::new();
    for _ in 0..21 {
        let run = run_flow_with(&state, "ws_flow_retry", "flow_retry", None, None, None)
            .await
            .unwrap();
        last_run_id = run.run_id;
    }
    assert_eq!(server.received_requests(24).len(), 24);
    assert_eq!(
        state.store().list_flow_runs("flow_retry").unwrap().len(),
        20
    );
    let listed = flow_list_impl(&state, "ws_flow_retry".into()).unwrap();
    assert_eq!(listed[0].last_run.as_ref().unwrap().run_id, last_run_id);
    let json = serde_json::to_value(&listed[0]).unwrap();
    assert_eq!(json["name"], "Poll until ready");
    assert_eq!(json["last_run"]["run_id"], last_run_id);
}

/// Pre-run production guard, post-script method widening re-check, and step-delay cancellation.
#[tokio::test]
async fn flow_run_asks_the_production_guard_rechecks_widened_steps_and_cancels_delays() {
    let server = TestServer::spawn_sequence(vec![
        TestServerAction::Respond(OK_RESPONSE.into()),
        TestServerAction::Respond(OK_RESPONSE.into()),
        TestServerAction::Respond(OK_RESPONSE.into()),
        TestServerAction::Respond(OK_RESPONSE.into()),
    ]);
    let state = state_with_workspaces(&["ws_flow_guard", "ws_flow_cancel"]);
    let mut slow = inline_step("step_slow", 0, HttpMethod::Get, "http://127.0.0.1:9/never");
    slow.delay_ms = 2_000;
    flow_save_impl(
        &state,
        flow_of("ws_flow_cancel", "flow_cancel", "Slow flow", vec![slow]),
    )
    .unwrap();
    let started = std::time::Instant::now();
    let (cancel_res, cancelled) = tokio::join!(
        flow_run_impl(
            &state,
            "ws_flow_cancel".into(),
            "flow_cancel".into(),
            None,
            None,
            None,
            None,
            None,
            Some("tok-flow".into()),
            None,
            &crate::events::NoEvents,
        ),
        async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            execution_cancel_impl(&state, "tok-flow".into())
        }
    );
    let cancel_res = cancel_res.unwrap();
    assert!(cancelled && cancel_res.cancelled && !cancel_res.passed && cancel_res.steps.is_empty());
    assert!(
        flow_list_impl(&state, "ws_flow_cancel".into()).unwrap()[0]
            .last_run
            .as_ref()
            .is_some_and(|run| run.cancelled && !run.passed)
    );
    assert!(started.elapsed() < std::time::Duration::from_millis(1_500));

    let mut production = Environment::new("env_prod", "ws_flow_guard", "Production").unwrap();
    production.confirm_before_send = true;
    state.store().save_environment(&production).unwrap();
    let url = format!("{}/refunds", server.base_url);
    flow_save_impl(
        &state,
        flow_of(
            "ws_flow_guard",
            "flow_guard",
            "Create invoice then refund",
            vec![
                inline_step("read", 0, HttpMethod::Get, &url),
                inline_step("refund", 1, HttpMethod::Post, &url),
            ],
        ),
    )
    .unwrap();

    let error = run_flow_with(
        &state,
        "ws_flow_guard",
        "flow_guard",
        Some("env_prod"),
        None,
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AppError::ConfirmationRequired { .. }));
    assert!(
        history_list(&state, "ws_flow_guard".into(), None, None)
            .unwrap()
            .is_empty()
    );
    assert!(
        flow_list_impl(&state, "ws_flow_guard".into()).unwrap()[0]
            .last_run
            .is_none()
    );

    let partial = run_flow_with(
        &state,
        "ws_flow_guard",
        "flow_guard",
        Some("env_prod"),
        Some(1),
        None,
    )
    .await
    .unwrap();
    assert_eq!(partial.steps.len(), 1);

    let confirmed = run_flow_with(
        &state,
        "ws_flow_guard",
        "flow_guard",
        Some("env_prod"),
        None,
        Some(true),
    )
    .await
    .unwrap();
    assert!(confirmed.passed && confirmed.steps.len() == 2);

    let mut widen_req = Request::new("req_widen", "ws_flow_guard", "Widen", &url).unwrap();
    widen_req.pre_request_script = r#"pm.request.method = "DELETE";"#.into();
    state.store().save_request(&widen_req).unwrap();
    flow_save_impl(&state, scripted_flow("ws_flow_guard", &["req_widen"])).unwrap();

    let refused = run_flow_with(
        &state,
        "ws_flow_guard",
        "flow_scripted",
        Some("env_prod"),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(!refused.passed);
    assert!(
        refused.steps[0]
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("confirm before sending")
    );

    let widen_ok = run_flow_with(
        &state,
        "ws_flow_guard",
        "flow_scripted",
        Some("env_prod"),
        None,
        Some(true),
    )
    .await
    .unwrap();
    assert!(widen_ok.passed && widen_ok.steps[0].error.is_none());
    assert_eq!(server.received_requests(4).len(), 4);
}

#[tokio::test]
async fn a_flow_with_an_incomplete_step_sends_nothing_and_names_the_step_by_number() {
    let server = TestServer::spawn_sequence(vec![TestServerAction::Respond(OK_RESPONSE.into())]);
    let state = state_with_workspaces(&["ws_flow_check"]);
    let first = inline_step(
        "first",
        0,
        HttpMethod::Get,
        &format!("{}/ok", server.base_url),
    );
    let mut empty = request_step("step_9f3", 1, "");
    empty.name = "Refund".into();
    flow_save_impl(
        &state,
        flow_of("ws_flow_check", "flow_check", "Check", vec![first, empty]),
    )
    .unwrap();

    let error = run_flow_with(&state, "ws_flow_check", "flow_check", None, None, None)
        .await
        .unwrap_err();

    assert!(matches!(error, AppError::Validation { .. }));
    assert_eq!(
        error.message(),
        "step 2 \"Refund\" has neither a saved request nor a URL"
    );
    assert!(
        history_list(&state, "ws_flow_check".into(), None, None)
            .unwrap()
            .is_empty()
    );
    assert!(
        state
            .store()
            .list_flow_runs("flow_check")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn mock_runtime_config_expands_plain_environment_variables_and_never_secrets() {
    let state = state_with_workspaces(&["ws_bridge"]);
    let mut staging = Environment::new("env_staging", "ws_bridge", "Staging").unwrap();
    staging.variables = vec![
        crate::workspace::EnvironmentVariable::plain("host", "staging.example.com").unwrap(),
        crate::workspace::EnvironmentVariable::masked("token", "s3cr3t").unwrap(),
    ];
    state.store().save_environment(&staging).unwrap();
    let mut server = mock_server_fixture("ws_bridge", 4999);
    server.environment_id = Some("env_staging".into());
    server.proxy_base_url = "https://{{host}}/v1".into();
    server.proxy_req_headers = vec![KeyValue {
        key: "Authorization".into(),
        value: "Bearer {{token}} {{header 'x-id'}}".into(),
        enabled: true,
        description: None,
    }];
    let saved = mock_server_save_impl(&state, server).unwrap();
    assert_eq!(saved.environment_id.as_deref(), Some("env_staging"));

    let runtime = crate::app::mock_service::mock_runtime_config(&state, &saved).unwrap();
    assert_eq!(runtime.proxy_base_url, "https://staging.example.com/v1");
    assert_eq!(
        runtime.proxy_req_headers[0].value, "Bearer {{token}} {{header 'x-id'}}",
        "a secret and a template helper stay as written"
    );
    assert_eq!(
        saved.proxy_base_url, "https://{{host}}/v1",
        "the saved server keeps the token"
    );
}
