//! Shared test fixtures: temp files, the scripted TCP test server (shared with the executor
//! tests), request builders and the environment fixtures shared by `environments` and
//! `secret_hosts`.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::app::{
    AppState, EnvironmentDto, EnvironmentVariableInput, SaveEnvironmentInput,
    environment_save_impl, workspace_create_impl,
};
use crate::error::AppError;
use crate::workspace::{
    MockResponse, MockResponseMode, MockRoute, MockRulesOperator, MockServer, Request, WorkspaceId,
};

/// Placeholder secret value shared by the environment and secret-host tests.
pub(super) const SECRET: &str = "s3cr3t-value-aaa";

/// One page of a workspace's history through the shipped `history_page_impl` (newest first);
/// `cursor` is a previous page's `next_cursor`.
pub(super) fn history_list(
    state: &crate::app::AppState,
    workspace_id: String,
    limit: Option<u32>,
    cursor: Option<String>,
) -> Result<Vec<crate::workspace::ExecutionHistoryEntry>, crate::error::AppError> {
    crate::app::misc_logic_service::history_page_impl(
        state,
        workspace_id,
        crate::workspace::HistoryFilter::default(),
        limit,
        cursor,
    )
    .map(|page| page.entries)
}

pub(super) fn temp_path(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("pingdeck-{nanos}-{name}"))
}

pub(super) fn draft_request_input(
    id: &str,
    workspace_id: &str,
    url: String,
) -> crate::app::SaveRequestInput {
    crate::app::SaveRequestInput {
        id: id.into(),
        workspace_id: workspace_id.into(),
        name: "Draft".into(),
        url,
        ..Default::default()
    }
}

pub(super) use pingdeck_executor::test_server::{
    TestServer, TestServerAction, read_complete_request,
};

pub(super) fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("free port probe should bind")
        .local_addr()
        .expect("free port probe should have an address")
        .port()
}

pub(super) fn http_get(port: u16, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("mock server should accept");
    write!(stream, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .expect("request should be written");
    stream.flush().expect("request should flush");
    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .expect("response should be readable");
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .expect("response should have a body");
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .expect("response should have a status");
    (status, body.to_owned())
}

pub(super) fn mock_server_fixture(workspace_id: &str, port: u16) -> MockServer {
    MockServer {
        id: "mock_cmd".into(),
        workspace_id: WorkspaceId::new(workspace_id).expect("workspace id should be valid"),
        name: "Mock".into(),
        port,
        endpoint_prefix: String::new(),
        latency_ms: 0,
        cors_enabled: true,
        headers: Vec::new(),
        data_buckets: Vec::new(),
        callbacks: Vec::new(),
        proxy_enabled: false,
        proxy_base_url: String::new(),
        proxy_remove_prefix: false,
        proxy_req_headers: Vec::new(),
        proxy_res_headers: Vec::new(),
        routes: vec![MockRoute {
            id: "route_hello".into(),
            method: "GET".into(),
            path: "/hello".into(),
            enabled: true,
            description: String::new(),
            route_type: crate::workspace::MockRouteType::Http,
            data_bucket_id: String::new(),
            crud_id_key: "id".into(),
            response_mode: MockResponseMode::FirstMatch,
            responses: vec![MockResponse {
                id: "resp_hello".into(),
                label: "OK".into(),
                status_code: 200,
                headers: Vec::new(),
                body: r#"{"hello":"{{query.who}}"}"#.into(),
                body_source: None,
                fault: None,
                latency_ms: 0,
                is_default: true,
                rules_operator: MockRulesOperator::And,
                rules: Vec::new(),
                callback_ids: Vec::new(),
                position: 0,
                stream: None,
            }],
            position: 0,
        }],
        updated_at_ms: 0,
        environment_id: None,
    }
}

/// In-memory state with two workspaces (`ws1`, `ws2`).
pub(super) fn state() -> AppState {
    let state = AppState::open_in_memory().unwrap();
    workspace_create_impl(&state, "ws1".into(), "One".into()).unwrap();
    workspace_create_impl(&state, "ws2".into(), "Two".into()).unwrap();
    state
}

/// One variable input row (no id, enabled, no note).
pub(super) fn row(key: &str, value: Option<&str>, secret: bool) -> EnvironmentVariableInput {
    EnvironmentVariableInput {
        key: key.into(),
        value: value.map(Into::into),
        is_secret: secret,
        ..Default::default()
    }
}

/// A full environment save input with defaults for every other field.
pub(super) fn env_input(
    id: &str,
    workspace_id: &str,
    name: &str,
    variables: Vec<EnvironmentVariableInput>,
) -> SaveEnvironmentInput {
    SaveEnvironmentInput {
        id: id.into(),
        workspace_id: workspace_id.into(),
        name: name.into(),
        variables,
        ..Default::default()
    }
}

/// Saves environment `id` in `ws1` through the shipped `environment_save_impl`.
pub(super) fn save_env(
    state: &AppState,
    id: &str,
    name: &str,
    variables: Vec<EnvironmentVariableInput>,
) -> Result<EnvironmentDto, AppError> {
    environment_save_impl(state, env_input(id, "ws1", name, variables))
}

/// A minimal request in `ws1` whose name equals its id.
pub(super) fn request(id: &str, url: &str) -> Request {
    Request::new(id, "ws1", id, url).unwrap()
}
