//! Unsaved default entities (`*_new`) and the assertion catalog.

use std::collections::HashSet;
use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AppError;
use crate::workspace::items::{
    Flow, FlowStep, MockCallback, MockDataBucket, MockResponse, MockResponseMode, MockRoute,
    MockRouteType, MockRule, MockRuleOperator, MockRuleTarget, MockRulesOperator, MockServer,
};
use crate::workspace::models::{
    Assertion, AssertionOp, AssertionTarget, Environment, HttpMethod, KeyValue, WorkspaceId,
};

use super::super::{AppState, command_error, ensure_workspace_exists};
use super::new_entity_id;

/// Route kinds the workbench offers: the HTTP methods, `ANY` and the `CRUD` route type.
const ROUTE_KINDS: [&str; 9] = [
    "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "ANY", "CRUD",
];

/// Assertion catalog: stable kind id (the `Assertion` variant name), display label and whether
/// the kind only applies to AI API requests.
const ASSERTION_KINDS: [(&str, &str, bool); 17] = [
    ("StatusEquals", "Status equals", false),
    ("StatusIn", "Status in", false),
    ("HeaderExists", "Header exists", false),
    ("HeaderEquals", "Header equals", false),
    ("JsonPathExists", "JSON path exists", false),
    ("JsonPathEquals", "JSON path equals", false),
    ("BodyContains", "Body contains", false),
    ("ResponseTimeLessThanMs", "Response time < ms", false),
    ("BodySizeLessThanBytes", "Body size < bytes", false),
    ("Check", "Compare a value", false),
    ("JsonSchemaValid", "Body matches JSON schema", false),
    ("AiReplyContains", "Reply contains", true),
    ("AiReplyMatches", "Reply matches regex", true),
    ("AiJsonSchemaValid", "JSON schema valid", true),
    ("AiTtftLessThanMs", "TTFT less than N ms", true),
    ("AiTokensLessThan", "Tokens less than N", true),
    ("AiNoError", "No error", true),
];

/// One entry of the assertion catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssertionKindInfo {
    /// Kind id accepted by [`assertion_new`] (the `Assertion` variant name).
    pub id: String,
    /// Label to show in a kind picker.
    pub label: String,
    /// The kind checks an AI API run: list it only for AI API requests.
    pub ai_only: bool,
}

/// Lists every assertion kind with its label, in picker order. A picker for an HTTP request
/// skips the kinds with `ai_only`.
#[must_use]
pub fn assertion_kinds() -> Vec<AssertionKindInfo> {
    ASSERTION_KINDS
        .iter()
        .map(|(id, label, ai_only)| AssertionKindInfo {
            id: (*id).to_owned(),
            label: (*label).to_owned(),
            ai_only: *ai_only,
        })
        .collect()
}

/// Builds an assertion of `kind` (an id from [`assertion_kinds`]) with sensible defaults.
pub fn assertion_new(kind: &str) -> Result<Assertion, AppError> {
    Ok(match kind {
        "StatusEquals" => Assertion::StatusEquals { value: 200 },
        "StatusIn" => Assertion::StatusIn { values: vec![200] },
        "HeaderExists" => Assertion::HeaderExists {
            key: "content-type".to_owned(),
        },
        "HeaderEquals" => Assertion::HeaderEquals {
            key: "content-type".to_owned(),
            value: "application/json".to_owned(),
        },
        "JsonPathExists" => Assertion::JsonPathExists {
            path: "$.id".to_owned(),
        },
        "JsonPathEquals" => Assertion::JsonPathEquals {
            path: "$.id".to_owned(),
            value: Value::String(String::new()),
        },
        "BodyContains" => Assertion::BodyContains {
            value: String::new(),
        },
        "ResponseTimeLessThanMs" => Assertion::ResponseTimeLessThanMs { value: 1000 },
        "BodySizeLessThanBytes" => Assertion::BodySizeLessThanBytes { value: 1_000_000 },
        "Check" => Assertion::Check {
            target: AssertionTarget::Status,
            op: AssertionOp::LessThan,
            value: Value::from(400),
        },
        "JsonSchemaValid" => Assertion::JsonSchemaValid {
            schema: serde_json::json!({ "type": "object" }),
            path: None,
        },
        "AiReplyContains" => Assertion::AiReplyContains {
            value: String::new(),
        },
        "AiReplyMatches" => Assertion::AiReplyMatches {
            pattern: String::new(),
        },
        "AiJsonSchemaValid" => Assertion::AiJsonSchemaValid {
            schema: serde_json::json!({ "type": "object" }),
        },
        "AiTtftLessThanMs" => Assertion::AiTtftLessThanMs { value: 1000 },
        "AiTokensLessThan" => Assertion::AiTokensLessThan { value: 1000 },
        "AiNoError" => Assertion::AiNoError {},
        other => {
            return Err(AppError::validation(format!(
                "unknown assertion kind {other:?}"
            )));
        }
    })
}

/// Converts a list index into the `position` stored on an entity.
pub(super) fn to_position(index: usize) -> i64 {
    i64::try_from(index).unwrap_or(i64::MAX)
}

fn json_content_type() -> KeyValue {
    KeyValue {
        key: "Content-Type".to_owned(),
        value: "application/json".to_owned(),
        enabled: true,
        description: None,
    }
}

/// Picks the first name from the index-based sequence (`first`, then `<prefix> 2`, `<prefix> 3`,
/// ...) that no entity in `existing` uses, starting at `existing.len()` so the first entity of a
/// workspace gets `first` and later ones get their ordinal.
pub(super) fn unused_name(first: &str, prefix: &str, existing: &[String]) -> String {
    let taken: HashSet<&str> = existing.iter().map(String::as_str).collect();
    let mut index = existing.len();
    loop {
        let name = if index == 0 {
            first.to_owned()
        } else {
            format!("{prefix} {}", index + 1)
        };
        if !taken.contains(name.as_str()) {
            return name;
        }
        index += 1;
    }
}

/// Lowest port from `first` upward that none of `used` occupies and `is_bindable` accepts.
pub(crate) fn first_free_port(
    first: u16,
    used: &[u16],
    is_bindable: impl Fn(u16) -> bool,
) -> Result<u16, AppError> {
    let used: HashSet<u16> = used.iter().copied().collect();
    (first..=u16::MAX)
        .find(|port| !used.contains(port) && is_bindable(*port))
        .ok_or_else(|| {
            AppError::conflict(format!("every mock server port from {first} up is taken"))
        })
}

/// Whether `port` can be bound on loopback right now (the probe listeners are dropped at once):
/// free on `127.0.0.1` and not in use on `[::1]`, matching what starting a mock server needs. A
/// machine without IPv6 loopback counts as free there, as at start.
pub(crate) fn port_is_bindable(port: u16) -> bool {
    let Ok(_ipv4) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) else {
        return false;
    };
    !matches!(
        TcpListener::bind((Ipv6Addr::LOCALHOST, port)),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse
    )
}

/// Builds an unsaved mock server for `workspace_id`.
///
/// The port is the first one from `mock.first_port` (3001) upward that no *saved* mock server of any workspace
/// uses (servers share one machine, so another workspace's port counts) and that a probe can
/// bind on loopback right now ([`port_is_bindable`]). The probe is a hint only: the port may be taken before the
/// server starts, and starting then reports a port in use. The name is
/// `Local mock` for the first server, then `Mock server N`, never repeating an existing name.
pub fn mock_server_new(state: &AppState, workspace_id: &str) -> Result<MockServer, AppError> {
    mock_server_new_with(state, workspace_id, port_is_bindable)
}

/// [`mock_server_new`] with the loopback probe injected, so tests do not depend on which ports
/// the machine has free.
pub(super) fn mock_server_new_with(
    state: &AppState,
    workspace_id: &str,
    is_bindable: impl Fn(u16) -> bool,
) -> Result<MockServer, AppError> {
    let (names, ports) = {
        let store = state.store();
        ensure_workspace_exists(&store, workspace_id)?;
        let names: Vec<String> = store
            .list_mock_servers(workspace_id)
            .map_err(command_error)?
            .into_iter()
            .map(|server| server.name)
            .collect();
        (
            names,
            store.list_mock_server_ports().map_err(command_error)?,
        )
    };
    let defaults = state.settings().mock.clone();
    Ok(MockServer {
        id: new_entity_id("mock"),
        workspace_id: WorkspaceId::new(workspace_id).map_err(command_error)?,
        name: unused_name("Local mock", "Mock server", &names),
        port: first_free_port(defaults.first_port, &ports, is_bindable)?,
        endpoint_prefix: String::new(),
        latency_ms: defaults.default_latency_ms,
        cors_enabled: defaults.cors_default,
        headers: Vec::new(),
        routes: Vec::new(),
        data_buckets: Vec::new(),
        callbacks: Vec::new(),
        proxy_enabled: false,
        proxy_base_url: String::new(),
        proxy_remove_prefix: true,
        proxy_req_headers: Vec::new(),
        proxy_res_headers: Vec::new(),
        updated_at_ms: 0,
        environment_id: None,
    })
}

/// Builds a default mock response at list `position` (the first one is the default).
#[must_use]
pub fn mock_response_new(position: usize) -> MockResponse {
    MockResponse {
        id: new_entity_id("resp"),
        label: if position == 0 {
            "200 OK".to_owned()
        } else {
            format!("Response {}", position + 1)
        },
        status_code: 200,
        headers: vec![json_content_type()],
        body: "{\n  \"ok\": true\n}".to_owned(),
        body_source: None,
        fault: None,
        latency_ms: 0,
        is_default: position == 0,
        rules_operator: MockRulesOperator::And,
        rules: Vec::new(),
        callback_ids: Vec::new(),
        position: to_position(position),
        stream: None,
    }
}

/// Builds a default mock route at list `position` for `kind`: an HTTP method name, `ANY`, or
/// `CRUD` (an `ANY` route of type `Crud`). The kind is case-insensitive; anything else is a
/// validation error. The route starts with one default response.
pub fn mock_route_new(kind: &str, position: usize) -> Result<MockRoute, AppError> {
    let kind = kind.trim().to_ascii_uppercase();
    if !ROUTE_KINDS.contains(&kind.as_str()) {
        return Err(AppError::validation(format!(
            "unknown mock route kind {kind:?}"
        )));
    }
    let crud = kind == "CRUD";
    Ok(MockRoute {
        id: new_entity_id("route"),
        method: if crud { "ANY".to_owned() } else { kind },
        path: if crud {
            format!("/crud-{}", position + 1)
        } else {
            format!("/route-{}", position + 1)
        },
        enabled: true,
        description: String::new(),
        route_type: if crud {
            MockRouteType::Crud
        } else {
            MockRouteType::Http
        },
        data_bucket_id: String::new(),
        crud_id_key: "id".to_owned(),
        response_mode: MockResponseMode::FirstMatch,
        responses: vec![mock_response_new(0)],
        position: to_position(position),
    })
}

/// Builds an empty JSON-array data bucket called `name`.
#[must_use]
pub fn mock_bucket_new(name: &str) -> MockDataBucket {
    MockDataBucket {
        id: new_entity_id("bucket"),
        name: name.to_owned(),
        documentation: String::new(),
        value_json: "[]".to_owned(),
    }
}

/// Builds a default outbound webhook callback at list `position`.
#[must_use]
pub fn mock_callback_new(position: usize) -> MockCallback {
    MockCallback {
        id: new_entity_id("callback"),
        name: format!("Callback {}", position + 1),
        documentation: String::new(),
        method: HttpMethod::Post,
        url: "https://example.com/webhook".to_owned(),
        delay_ms: 0,
        headers: vec![json_content_type()],
        body: "{\n  \"event\": \"delivered\"\n}".to_owned(),
    }
}

/// Builds a default response rule (query param `page` equals empty) at list `position`.
#[must_use]
pub fn mock_rule_new(position: usize) -> MockRule {
    MockRule {
        id: new_entity_id("rule"),
        target: MockRuleTarget::Query,
        modifier: "page".to_owned(),
        operator: MockRuleOperator::Equals,
        value: String::new(),
        invert: false,
        position: to_position(position),
    }
}

/// Builds an unsaved empty flow; the name is `Smoke flow` for a workspace's first flow, then
/// `Flow N`, never repeating an existing name.
pub fn flow_new(state: &AppState, workspace_id: &str) -> Result<Flow, AppError> {
    let names: Vec<String> = {
        let store = state.store();
        ensure_workspace_exists(&store, workspace_id)?;
        store
            .list_flows(workspace_id)
            .map_err(command_error)?
            .into_iter()
            .map(|flow| flow.name)
            .collect()
    };
    Ok(Flow {
        id: new_entity_id("flow"),
        workspace_id: WorkspaceId::new(workspace_id).map_err(command_error)?,
        name: unused_name("Smoke flow", "Flow", &names),
        description: String::new(),
        steps: Vec::new(),
        updated_at_ms: 0,
        options: Default::default(),
    })
}

/// Builds an unsaved empty step appended after `flow`'s last step.
#[must_use]
pub fn flow_step_new(flow: &Flow) -> FlowStep {
    let index = flow.steps.len();
    FlowStep {
        id: new_entity_id("step"),
        name: format!("Step {}", index + 1),
        request_id: String::new(),
        delay_ms: 0,
        extractions: Vec::new(),
        assertions: Vec::new(),
        ignore_request_assertions: false,
        continue_on_error: false,
        position: to_position(index),
        method_override: None,
        url_override: None,
        headers_override: Vec::new(),
        query_override: Vec::new(),
        body_override: None,
        body_override_kind: None,
        condition_expression: String::new(),
        retries: 0,
        retry_wait_ms: 0,
        retry_on: Vec::new(),
        timeout_ms: None,
        step_kind: crate::workspace::items::FlowStepKind::Request,
        depends_on: Vec::new(),
        branches: Vec::new(),
        lane: None,
    }
}

/// Builds a blank, unsaved environment for `workspace_id`.
pub fn environment_new(workspace_id: &str) -> Result<Environment, AppError> {
    Environment::new(new_entity_id("env"), workspace_id, "Untitled Environment")
        .map_err(command_error)
}
