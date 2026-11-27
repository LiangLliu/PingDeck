//! Storage tests for mock servers, flows, and TLS certificates.

use crate::storage::StorageError;
use crate::storage::flow_runs::FLOW_RUN_RETENTION;
use pingdeck_model::items::{
    Flow, FlowExtraction, FlowExtractionSource, FlowRetryOn, FlowRunSummary, FlowStep,
    MockResponse, MockResponseMode, MockRoute, MockRule, MockRuleOperator, MockRuleTarget,
    MockRulesOperator, MockServer,
};
use pingdeck_model::models::{Assertion, TlsCertificate, TlsCertificateId, TlsCertificateKind};
use pingdeck_model::{HttpMethod, KeyValue, Workspace, WorkspaceId};

use super::super::WorkspaceStore;

fn mock_server_fixture(workspace_id: &WorkspaceId) -> MockServer {
    MockServer {
        id: "mock_1".into(),
        workspace_id: workspace_id.clone(),
        name: "Mock API".into(),
        port: 3001,
        endpoint_prefix: "api".into(),
        latency_ms: 25,
        cors_enabled: true,
        headers: vec![KeyValue {
            key: "x-env".into(),
            value: "dev".into(),
            enabled: true,
            description: None,
        }],
        routes: vec![MockRoute {
            id: "route_1".into(),
            method: "GET".into(),
            path: "/users/:id".into(),
            enabled: true,
            description: String::new(),
            route_type: pingdeck_model::items::MockRouteType::Http,
            data_bucket_id: String::new(),
            crud_id_key: "id".into(),
            response_mode: MockResponseMode::Sequential,
            responses: vec![MockResponse {
                id: "response_1".into(),
                label: "OK".into(),
                status_code: 200,
                headers: vec![],
                body: r#"{"id":"{{params.id}}"}"#.into(),
                body_source: None,
                fault: None,
                latency_ms: 0,
                is_default: false,
                rules_operator: MockRulesOperator::And,
                rules: vec![MockRule {
                    id: "rule_1".into(),
                    target: MockRuleTarget::Query,
                    modifier: "page".into(),
                    operator: MockRuleOperator::Equals,
                    value: "1".into(),
                    invert: false,
                    position: 0,
                }],
                callback_ids: vec!["callback_1".into()],
                position: 0,
                stream: None,
            }],
            position: 0,
        }],
        data_buckets: vec![pingdeck_model::items::MockDataBucket {
            id: "bucket_1".into(),
            name: "Users".into(),
            documentation: String::new(),
            value_json: r#"[{"id":1}]"#.into(),
        }],
        callbacks: vec![pingdeck_model::items::MockCallback {
            id: "callback_1".into(),
            name: "notify".into(),
            documentation: String::new(),
            method: HttpMethod::Post,
            url: "http://example.test/hook".into(),
            delay_ms: 10,
            headers: vec![],
            body: r#"{"ok":true}"#.into(),
        }],
        proxy_enabled: true,
        proxy_base_url: "https://httpbin.org".into(),
        proxy_remove_prefix: true,
        proxy_req_headers: vec![KeyValue {
            key: "x-from".into(),
            value: "pingdeck".into(),
            enabled: true,
            description: None,
        }],
        proxy_res_headers: vec![KeyValue {
            key: "x-proxied".into(),
            value: "true".into(),
            enabled: true,
            description: None,
        }],
        updated_at_ms: 1_000,
        environment_id: None,
    }
}

fn tls_certificate_fixture(workspace_id: &WorkspaceId) -> TlsCertificate {
    TlsCertificate {
        id: TlsCertificateId::new("cert_1").unwrap(),
        workspace_id: workspace_id.clone(),
        kind: TlsCertificateKind::Client,
        host_pattern: "api.example.test".into(),
        cert_path: "/tmp/client.pem".into(),
        key_path: Some("/tmp/client.key".into()),
        passphrase_ref: None,
        enabled: true,
    }
}

fn flow_run_summary(flow_id: &str, run: i64, passed: bool) -> FlowRunSummary {
    FlowRunSummary {
        run_id: format!("{flow_id}_run_{run}"),
        flow_id: flow_id.into(),
        started_at_ms: run,
        finished_at_ms: run + 5,
        passed,
        cancelled: false,
        failed_step_id: (!passed).then(|| "step_1".to_owned()),
        failed_step_name: (!passed).then(|| "Login".to_owned()),
        steps_passed: usize::from(passed),
        steps_total: 1,
        elapsed_ms: 5,
        steps: Vec::new(),
        scope: Default::default(),
        environment_id: None,
        variables: None,
        withheld_variables: Vec::new(),
    }
}

#[test]
fn workspace_items_round_trip_isolate_and_cascade_with_secrets() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let owner = Workspace::new("ws_owner", "Owner").unwrap();
    let other = Workspace::new("ws_other", "Other").unwrap();
    store.save_workspace(&owner).unwrap();
    store.save_workspace(&other).unwrap();

    let server = mock_server_fixture(&owner.id);
    store.save_mock_server(&server).unwrap();
    assert_eq!(
        store.get_mock_server("mock_1").unwrap(),
        Some(server.clone())
    );
    assert_eq!(
        store.list_mock_servers("ws_owner").unwrap(),
        vec![server.clone()]
    );

    let flow = Flow {
        id: "flow_1".into(),
        workspace_id: owner.id.clone(),
        name: "Login".into(),
        description: "Logs in".into(),
        steps: vec![FlowStep {
            id: "step_1".into(),
            name: "Login".into(),
            request_id: "req_login".into(),
            delay_ms: 100,
            extractions: vec![FlowExtraction {
                variable_name: "token".into(),
                source: FlowExtractionSource::BodyJsonPath,
                expression: "token".into(),
                secret: Some(true),
            }],
            assertions: vec![Assertion::StatusEquals { value: 200 }],
            ignore_request_assertions: true,
            continue_on_error: false,
            position: 0,
            method_override: Some(HttpMethod::Post),
            url_override: Some("http://example.test/login".into()),
            headers_override: vec![KeyValue {
                key: "x-step".into(),
                value: "1".into(),
                enabled: true,
                description: None,
            }],
            query_override: Vec::new(),
            body_override: Some(r#"{"user":"ada"}"#.into()),
            body_override_kind: Some(pingdeck_model::items::FlowBodyKind::Json),
            condition_expression: "{{status}} == 200".into(),
            retries: 2,
            retry_wait_ms: 500,
            retry_on: vec![FlowRetryOn::Network, FlowRetryOn::Status429],
            timeout_ms: Some(8_000),
            step_kind: pingdeck_model::items::FlowStepKind::Request,
            depends_on: Vec::new(),
            branches: Vec::new(),
            lane: None,
        }],
        updated_at_ms: 1_300,
        options: pingdeck_model::items::FlowOptions {
            initial_variables: [("region".to_owned(), "eu".to_owned())].into(),
            cookie_session: false,
            ..Default::default()
        },
    };
    store.save_flow(&flow).unwrap();
    assert_eq!(store.get_flow("flow_1").unwrap(), Some(flow.clone()));
    assert_eq!(store.list_flows("ws_owner").unwrap(), vec![flow]);

    let mut cert = tls_certificate_fixture(&owner.id);
    cert.passphrase_ref = Some("tls.ws_owner.cert_1.passphrase".into());
    store.save_tls_certificate(&cert).unwrap();
    store
        .secret_put("tls.ws_owner.cert_1.passphrase", "hunter2", 1)
        .unwrap();
    store
        .secret_put("tls.ws_other.cert_2.passphrase", "keep", 1)
        .unwrap();
    assert_eq!(store.list_tls_certificates("ws_owner").unwrap(), vec![cert]);

    // Cross-workspace isolation
    assert!(!store.delete_mock_server("mock_1", "ws_other").unwrap());
    assert!(!store.delete_tls_certificate("cert_1", "ws_other").unwrap());
    let mut stolen = server;
    stolen.workspace_id = other.id.clone();
    assert!(matches!(
        store.save_mock_server(&stolen),
        Err(StorageError::ItemWorkspaceMismatch { .. })
    ));
    let mut updated = mock_server_fixture(&owner.id);
    updated.name = "Renamed Mock".into();
    updated.latency_ms = 50;
    updated.routes.clear();
    store.save_mock_server(&updated).unwrap();
    assert_eq!(
        store.get_mock_server("mock_1").unwrap(),
        Some(updated.clone())
    );

    // The environment link keeps an environment of the server's workspace and drops any other.
    for (id, workspace) in [("env_own", "ws_owner"), ("env_foreign", "ws_other")] {
        let environment = pingdeck_model::models::Environment::new(id, workspace, id).unwrap();
        store.save_environment(&environment).unwrap();
    }
    updated.environment_id = Some("env_own".into());
    store.save_mock_server(&updated).unwrap();
    assert_eq!(
        store.get_mock_server("mock_1").unwrap(),
        Some(updated.clone())
    );
    updated.environment_id = Some("env_foreign".into());
    store.save_mock_server(&updated).unwrap();
    let stored = store.get_mock_server("mock_1").unwrap().unwrap();
    assert_eq!(stored.environment_id, None);
    assert!(store.delete_mock_server("mock_1", "ws_owner").unwrap());
    assert!(!store.delete_mock_server("mock_1", "ws_owner").unwrap());
    store
        .save_mock_server(&mock_server_fixture(&owner.id))
        .unwrap();

    // Cascade on workspace delete drops items and only that workspace's TLS secrets
    assert!(store.delete_workspace(&owner.id).unwrap());
    assert!(store.list_mock_servers("ws_owner").unwrap().is_empty());
    assert!(store.list_flows("ws_owner").unwrap().is_empty());
    assert!(store.list_tls_certificates("ws_owner").unwrap().is_empty());
    assert_eq!(
        store.secret_get("tls.ws_owner.cert_1.passphrase").unwrap(),
        None
    );
    assert_eq!(
        store
            .secret_get("tls.ws_other.cert_2.passphrase")
            .unwrap()
            .as_deref(),
        Some("keep")
    );
}

#[test]
fn flow_runs_prune_per_flow_and_cascade_on_flow_delete() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let workspace = Workspace::new("ws_runs", "Runs").unwrap();
    store.save_workspace(&workspace).unwrap();
    for id in ["flow_a", "flow_b"] {
        store
            .save_flow(&Flow {
                id: id.into(),
                workspace_id: workspace.id.clone(),
                name: id.into(),
                description: String::new(),
                steps: Vec::new(),
                updated_at_ms: 1,
                options: Default::default(),
            })
            .unwrap();
    }
    for run in 1..=25 {
        assert!(
            store
                .save_flow_run("ws_runs", &flow_run_summary("flow_a", run, run % 2 == 0))
                .unwrap()
        );
    }
    store
        .save_flow_run("ws_runs", &flow_run_summary("flow_b", 7, false))
        .unwrap();

    let kept = store.list_flow_runs("flow_a").unwrap();
    assert_eq!(kept.len(), FLOW_RUN_RETENTION);
    assert_eq!(
        (
            kept.first().unwrap().started_at_ms,
            kept.last().unwrap().started_at_ms
        ),
        (25, 6)
    );
    let last = store.last_flow_runs("ws_runs").unwrap();
    assert_eq!(last["flow_a"], flow_run_summary("flow_a", 25, false));
    assert_eq!(last["flow_b"], flow_run_summary("flow_b", 7, false));

    assert!(store.delete_flow("flow_a", "ws_runs").unwrap());
    assert!(store.list_flow_runs("flow_a").unwrap().is_empty());
    assert!(
        !store
            .save_flow_run("ws_runs", &flow_run_summary("flow_a", 26, true))
            .unwrap()
    );
    assert!(
        !store
            .save_flow_run("ws_other", &flow_run_summary("flow_b", 8, true))
            .unwrap()
    );
}

#[test]
fn flows_and_certificates_reject_an_id_another_workspace_owns() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let owner = Workspace::new("ws_owner", "Owner").unwrap();
    let other = Workspace::new("ws_other", "Other").unwrap();
    store.save_workspace(&owner).unwrap();
    store.save_workspace(&other).unwrap();
    let flow = Flow {
        id: "flow_owned".into(),
        workspace_id: owner.id.clone(),
        name: "Owned".into(),
        description: String::new(),
        steps: Vec::new(),
        updated_at_ms: 1,
        options: pingdeck_model::items::FlowOptions::default(),
    };
    store.save_flow(&flow).unwrap();
    let certificate = tls_certificate_fixture(&owner.id);
    store.save_tls_certificate(&certificate).unwrap();

    let mut stolen_flow = flow.clone();
    stolen_flow.workspace_id = other.id.clone();
    stolen_flow.name = "Stolen".into();
    assert!(matches!(
        store.save_flow(&stolen_flow),
        Err(StorageError::ItemWorkspaceMismatch { kind: "flow", .. })
    ));
    let mut stolen_certificate = certificate.clone();
    stolen_certificate.workspace_id = other.id.clone();
    stolen_certificate.host_pattern = "stolen.test".into();
    assert!(matches!(
        store.save_tls_certificate(&stolen_certificate),
        Err(StorageError::ItemWorkspaceMismatch {
            kind: "certificate",
            ..
        })
    ));

    assert_eq!(store.get_flow("flow_owned").unwrap(), Some(flow));
    assert!(store.list_flows("ws_other").unwrap().is_empty());
    assert_eq!(
        store.list_tls_certificates("ws_owner").unwrap(),
        vec![certificate]
    );
    assert!(store.list_tls_certificates("ws_other").unwrap().is_empty());
}
