//! Tests for ids, defaults, reorder/duplicate and the mock route helpers.

use std::collections::HashSet;

use crate::error::AppError;
use crate::workspace::items::{Flow, MockRoute, MockRule, MockServer, MockTransactionLog};
use crate::workspace::models::{Assertion, KeyValue};

use super::defaults::{first_free_port, mock_server_new_with, port_is_bindable, unused_name};
use super::*;
use crate::app::{
    AppState, collection_list_impl, environment_list_impl, flow_save_impl, mock_server_save_impl,
    request_list_impl, workspace_create_impl, workspace_list_impl,
};

fn state_with_workspaces(ids: &[&str]) -> AppState {
    let state = AppState::open_in_memory().expect("in-memory store opens");
    for id in ids {
        workspace_create_impl(&state, (*id).into(), format!("Workspace {id}"))
            .expect("workspace saves");
    }
    state
}

fn kind(error: AppError) -> &'static str {
    match error {
        AppError::NotFound { .. } => "NotFound",
        AppError::Validation { .. } => "Validation",
        AppError::Conflict { .. } => "Conflict",
        _ => "Other",
    }
}

// ---- ids ----

#[test]
fn entity_ids_carry_the_prefix_and_never_repeat() {
    let ids: HashSet<String> = (0..2000).map(|_| new_entity_id("mock")).collect();
    assert_eq!(ids.len(), 2000);
    assert!(
        ids.iter()
            .all(|id| id.starts_with("mock_") && id.len() == 5 + 36)
    );
}

// ---- starter workspace ----

#[test]
fn starter_workspace_creates_every_row() {
    let state = state_with_workspaces(&[]);
    let starter = workspace_create_starter(&state, "ws_starter", "Starter Workspace")
        .expect("starter is created");

    assert_eq!(workspace_list_impl(&state).expect("lists").len(), 1);
    let environments = environment_list_impl(&state, "ws_starter".into()).expect("lists");
    assert_eq!(
        environments,
        vec![crate::app::EnvironmentDto::from(&starter.environment)]
    );
    let token = &starter.environment.variables[1];
    assert_eq!(token.key, "apiToken");
    assert!(token.is_secret && !token.enabled);
    assert_eq!(token.value.as_deref(), Some("replace-with-your-token"));
    assert_eq!(
        starter.environment.variables[0].value.as_deref(),
        Some("https://api.example.com")
    );
    let collections = collection_list_impl(&state, "ws_starter".into()).expect("lists");
    assert_eq!(
        collections,
        vec![crate::app::CollectionDto::from(&starter.collection)]
    );
    let requests = request_list_impl(&state, "ws_starter".into()).expect("lists");
    assert_eq!(
        requests,
        vec![crate::workspace::Request {
            revision: Some(crate::app::request_revision(&starter.request)),
            ..starter.request.clone()
        }]
    );
    assert_eq!(starter.request.url, "{{baseUrl}}/health");
    assert_eq!(
        starter.request.collection_id.as_ref(),
        Some(&starter.collection.id)
    );
}

#[test]
fn starter_workspace_refuses_an_existing_id_and_a_blank_name() {
    let state = state_with_workspaces(&["ws_taken"]);
    let error = workspace_create_starter(&state, "ws_taken", "Again").expect_err("id is taken");
    assert_eq!(kind(error), "Conflict");
    let error = workspace_create_starter(&state, "ws_new", "  ").expect_err("name is blank");
    assert_eq!(kind(error), "Validation");
    assert_eq!(workspace_list_impl(&state).expect("lists").len(), 1);
}

// ---- store-aware defaults ----

#[test]
fn mock_server_port_skips_ports_of_every_workspace() {
    let state = state_with_workspaces(&["ws_a", "ws_b"]);
    let first = mock_server_new_with(&state, "ws_a", |_| true).expect("defaults");
    assert_eq!((first.port, first.name.as_str()), (3001, "Local mock"));
    assert!(first.cors_enabled && first.proxy_remove_prefix && first.routes.is_empty());
    mock_server_save_impl(&state, first).expect("saves");

    let other = mock_server_new_with(&state, "ws_b", |_| true).expect("defaults");
    assert_eq!((other.port, other.name.as_str()), (3002, "Local mock"));
    let mut other = other;
    other.port = 3003; // a gap at 3002 is reused only once nothing occupies it
    mock_server_save_impl(&state, other).expect("saves");

    let next = mock_server_new_with(&state, "ws_a", |_| true).expect("defaults");
    assert_eq!((next.port, next.name.as_str()), (3002, "Mock server 2"));
}

#[test]
fn mock_server_new_rejects_an_unknown_workspace() {
    let state = state_with_workspaces(&[]);
    assert_eq!(
        kind(mock_server_new(&state, "ws_missing").expect_err("no workspace")),
        "NotFound"
    );
    assert_eq!(
        kind(flow_new(&state, "ws_missing").expect_err("no workspace")),
        "NotFound"
    );
}

#[test]
fn free_port_exhaustion_is_a_conflict() {
    assert_eq!(first_free_port(3001, &[], |_| true).expect("free"), 3001);
    assert_eq!(
        first_free_port(3001, &[3001, 3002, 3004], |_| true).expect("free"),
        3003
    );
    assert_eq!(
        first_free_port(3001, &[1, 2, 3], |_| true).expect("free"),
        3001
    );
    let all: Vec<u16> = (3001..=u16::MAX).collect();
    assert_eq!(
        kind(first_free_port(3001, &all, |_| true).expect_err("all taken")),
        "Conflict"
    );
    assert_eq!(
        kind(first_free_port(3001, &[], |_| false).expect_err("nothing bindable")),
        "Conflict"
    );
}

#[test]
fn free_port_skips_ports_the_probe_rejects() {
    assert_eq!(
        first_free_port(3001, &[3002], |port| port != 3001 && port != 3003).expect("free"),
        3004
    );
}

#[test]
fn the_loopback_probe_sees_a_held_listener() {
    let held = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("binds");
    let port = held.local_addr().expect("addr").port();
    assert!(!port_is_bindable(port));
    drop(held);
    // Port 0 (any free port) always binds. Re-probing `port` itself would race with parallel
    // tests that take the just-freed ephemeral port.
    assert!(port_is_bindable(0));
    // A listener on `[::1]` alone (Node binds `localhost` there) also makes the port unusable,
    // because starting a mock server refuses it.
    if let Ok(held) = std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0)) {
        let port = held.local_addr().expect("addr").port();
        assert!(!port_is_bindable(port));
    }
}

#[test]
fn names_follow_the_ordinal_and_never_collide() {
    let names = |list: &[&str]| {
        list.iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(unused_name("First", "Item", &[]), "First");
    assert_eq!(unused_name("First", "Item", &names(&["First"])), "Item 2");
    assert_eq!(
        unused_name("First", "Item", &names(&["First", "Item 2"])),
        "Item 3"
    );
    // A deleted first entry does not make the next one reuse a taken ordinal name.
    assert_eq!(unused_name("First", "Item", &names(&["Item 2"])), "Item 3");
    assert_eq!(
        unused_name("First", "Item", &names(&["Other", "Item 3"])),
        "Item 4"
    );
}

#[test]
fn flows_get_unused_names() {
    let state = state_with_workspaces(&["ws_a"]);
    let flow = flow_new(&state, "ws_a").expect("defaults");
    assert_eq!(flow.name, "Smoke flow");
    assert!(flow.steps.is_empty() && flow.updated_at_ms == 0);
    flow_save_impl(&state, flow).expect("saves");
    assert_eq!(flow_new(&state, "ws_a").expect("defaults").name, "Flow 2");
}

// ---- pure defaults ----

#[test]
fn factory_unique_defaults_and_rejections() {
    let environment = environment_new("ws_a").expect("defaults");
    assert_eq!(environment.name, "Untitled Environment");
    assert_eq!(
        kind(environment_new(" ").expect_err("blank workspace")),
        "Validation"
    );
    assert_eq!(
        kind(mock_route_new("TRACE", 0).expect_err("unknown")),
        "Validation"
    );
}

// ---- assertions ----

/// Exhaustive on purpose: a new `Assertion` variant must be added to the catalog.
fn kind_id(assertion: &Assertion) -> &'static str {
    match assertion {
        Assertion::StatusEquals { .. } => "StatusEquals",
        Assertion::StatusIn { .. } => "StatusIn",
        Assertion::HeaderExists { .. } => "HeaderExists",
        Assertion::HeaderEquals { .. } => "HeaderEquals",
        Assertion::JsonPathExists { .. } => "JsonPathExists",
        Assertion::JsonPathEquals { .. } => "JsonPathEquals",
        Assertion::BodyContains { .. } => "BodyContains",
        Assertion::ResponseTimeLessThanMs { .. } => "ResponseTimeLessThanMs",
        Assertion::BodySizeLessThanBytes { .. } => "BodySizeLessThanBytes",
        Assertion::Check { .. } => "Check",
        Assertion::JsonSchemaValid { .. } => "JsonSchemaValid",
        Assertion::AiReplyContains { .. } => "AiReplyContains",
        Assertion::AiReplyMatches { .. } => "AiReplyMatches",
        Assertion::AiJsonSchemaValid { .. } => "AiJsonSchemaValid",
        Assertion::AiTtftLessThanMs { .. } => "AiTtftLessThanMs",
        Assertion::AiTokensLessThan { .. } => "AiTokensLessThan",
        Assertion::AiNoError { .. } => "AiNoError",
    }
}

#[test]
fn assertion_new_builds_every_cataloged_kind() {
    let kinds = assertion_kinds();
    assert_eq!(kinds.len(), 17);
    let mut seen = HashSet::new();
    for info in &kinds {
        assert!(!info.label.is_empty());
        let assertion = assertion_new(&info.id).expect("kind builds");
        assert_eq!(kind_id(&assertion), info.id);
        // These start blank on purpose: the user types the text before saving.
        let blank = ["BodyContains", "AiReplyContains", "AiReplyMatches"];
        assert_eq!(
            assertion.validate().is_ok(),
            !blank.contains(&info.id.as_str())
        );
        assert_eq!(info.ai_only, info.id.starts_with("Ai"), "{}", info.id);
        seen.insert(info.id.clone());
    }
    assert_eq!(seen.len(), 17);
    assert_eq!(
        kind(assertion_new("Nope").expect_err("unknown")),
        "Validation"
    );
}

// ---- reorder / duplicate / remove ----

fn flow_with_steps(names: &[&str]) -> Flow {
    let steps = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let mut step = flow_step_new(&empty_flow());
            step.id = format!("step_{name}");
            step.name = (*name).to_owned();
            step.position = (index as i64) * 10; // sparse on purpose
            step
        })
        .collect();
    Flow {
        steps,
        ..empty_flow()
    }
}

fn empty_flow() -> Flow {
    Flow {
        id: "flow_1".into(),
        workspace_id: crate::workspace::WorkspaceId::new("ws_a").expect("id"),
        name: "Flow".into(),
        description: String::new(),
        steps: Vec::new(),
        updated_at_ms: 9,
        options: Default::default(),
    }
}

fn route(id: &str, position: i64) -> MockRoute {
    let mut route = mock_route_new("GET", 0).expect("route");
    route.id = id.to_owned();
    route.position = position;
    route
}

#[test]
fn duplicate_validates_ids_and_nested_rules() {
    let mut with_rule = route("r1", 0);
    with_rule.responses.push(mock_response_new(1));
    with_rule.responses[1].rules.push(MockRule {
        id: "rule_old".into(),
        ..mock_rule_new(0)
    });
    let duplicated = mock_route_duplicate(vec![with_rule], "r1").expect("duplicates");
    assert_ne!(duplicated[1].responses[1].rules[0].id, "rule_old");
    assert_eq!(
        kind(mock_route_duplicate(duplicated, "nope").expect_err("missing route")),
        "NotFound"
    );
    assert_eq!(
        kind(mock_response_duplicate(route("r1", 0), "nope").expect_err("missing response")),
        "NotFound"
    );

    let steps = flow_with_steps(&["x"]).steps;
    assert_eq!(
        kind(flow_steps_reorder(steps.clone(), 0, 1).expect_err("to")),
        "Validation"
    );
    assert_eq!(
        kind(flow_step_duplicate(steps.clone(), "nope").expect_err("missing step")),
        "NotFound"
    );
    assert_eq!(
        kind(flow_step_remove(steps, "nope").expect_err("missing step")),
        "NotFound"
    );
}

#[test]
fn flow_duplicate_renews_every_id() {
    let flow = flow_with_steps(&["a", "b"]);
    let copy = flow_duplicate(flow.clone());
    assert_ne!(copy.id, flow.id);
    assert_eq!((copy.name.as_str(), copy.updated_at_ms), ("Flow copy", 0));
    assert!(
        copy.steps
            .iter()
            .all(|s| !flow.steps.iter().any(|o| o.id == s.id))
    );
    assert_eq!(
        copy.steps.iter().map(|s| s.position).collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(copy.steps[1].name, "b");
}

// ---- mock route helpers ----

fn server_with(prefix: &str, port: u16) -> MockServer {
    MockServer {
        id: "mock_1".into(),
        workspace_id: crate::workspace::WorkspaceId::new("ws_a").expect("id"),
        name: "Local mock".into(),
        port,
        endpoint_prefix: prefix.into(),
        latency_ms: 0,
        cors_enabled: true,
        headers: Vec::new(),
        routes: vec![route("r1", 0)],
        data_buckets: Vec::new(),
        callbacks: Vec::new(),
        proxy_enabled: false,
        proxy_base_url: String::new(),
        proxy_remove_prefix: true,
        proxy_req_headers: Vec::new(),
        proxy_res_headers: Vec::new(),
        updated_at_ms: 0,
        environment_id: None,
    }
}

fn log(method: &str, path: &str) -> MockTransactionLog {
    MockTransactionLog {
        id: "log_1".into(),
        server_id: "mock_1".into(),
        timestamp_ms: 3_723_000, // 01:02:03 UTC
        method: method.into(),
        path: path.into(),
        query: "a=1".into(),
        request_headers: Vec::new(),
        request_body: String::new(),
        request_body_truncated: false,
        matched_route_id: None,
        matched_response_id: None,
        status_code: 418,
        response_headers: vec![KeyValue {
            key: "X-Tea".into(),
            value: "pot".into(),
            enabled: true,
            description: None,
        }],
        response_body: "short and stout".into(),
        response_body_truncated: false,
        proxied: false,
        elapsed_ms: 3,
        ..MockTransactionLog::default()
    }
}

#[test]
fn route_from_log_formats_timestamp_and_keeps_similar_prefix_paths() {
    let server = server_with("/api", 3001);
    let route = mock_route_from_log(&server, &log("post", "/api/users/1"));
    assert_eq!(route.description, "Created from the log at 01:02:03 UTC");
    assert_eq!(
        mock_route_from_log(&server, &log("get", "/apix/y")).path,
        "/apix/y"
    );
}

#[test]
fn recorded_text_with_template_markup_replays_byte_for_byte() {
    use crate::mock::{MockRecording, decode_base64_body};
    use crate::workspace::items::MockBodySource;

    let server = server_with("", 3001);
    let exchange = |body: &str| MockRecording {
        log_id: "log_1".into(),
        method: "GET".into(),
        path: "/page".into(),
        query: String::new(),
        status: 200,
        headers: Vec::new(),
        body: bytes::Bytes::from(body.to_owned()),
        recorded_at_ms: 0,
    };
    let plain = mock_route_from_exchange(&server, &exchange("{\"ok\":true}"), 1);
    assert_eq!(
        (
            plain.responses[0].body.as_str(),
            plain.responses[0].body_source
        ),
        ("{\"ok\":true}", None)
    );
    let markup = "<li>{{ item.name }}</li>{{#each items}}{{name}}{{/each}}";
    let vue = mock_route_from_exchange(&server, &exchange(markup), 1);
    assert_eq!(vue.responses[0].body_source, Some(MockBodySource::Base64));
    assert_eq!(
        decode_base64_body(&vue.responses[0].body).expect("base64"),
        markup.as_bytes()
    );
}

#[test]
fn crud_endpoints_list_the_generated_routes() {
    let server = server_with("/api", 3001);
    let mut crud = mock_route_new("CRUD", 0).expect("route");
    crud.path = "/users".into();
    let endpoints = mock_crud_endpoints(&server, &crud).expect("endpoints");
    let listed: Vec<(&str, &str)> = endpoints
        .iter()
        .map(|e| (e.method.as_str(), e.path.as_str()))
        .collect();
    assert_eq!(
        listed,
        [
            ("GET", "/api/users"),
            ("GET", "/api/users/:id"),
            ("POST", "/api/users"),
            ("PUT", "/api/users/:id"),
            ("PATCH", "/api/users/:id"),
            ("DELETE", "/api/users/:id"),
        ]
    );
    assert!(endpoints.iter().all(|e| !e.description.is_empty()));
    crud.path = " ".into();
    assert_eq!(
        mock_crud_endpoints(&server, &crud).expect("endpoints")[0].path,
        "/api/resource"
    );
    let http = route("r1", 0);
    assert_eq!(
        kind(mock_crud_endpoints(&server, &http).expect_err("not crud")),
        "Validation"
    );
}

#[test]
fn adding_a_crud_route_links_or_creates_a_bucket() {
    let server = server_with("", 3001);
    let with_bucket = mock_server_add_route(server, "CRUD").expect("adds");
    assert_eq!(with_bucket.routes.len(), 2);
    assert_eq!(with_bucket.data_buckets.len(), 1);
    assert_eq!(with_bucket.data_buckets[0].name, "items");
    let added = &with_bucket.routes[1];
    assert_eq!((added.path.as_str(), added.position), ("/crud-2", 1));
    assert_eq!(added.data_bucket_id, with_bucket.data_buckets[0].id);

    // The existing first bucket is reused.
    let again = mock_server_add_route(with_bucket, "CRUD").expect("adds");
    assert_eq!(again.data_buckets.len(), 1);
    assert_eq!(again.routes[2].data_bucket_id, again.data_buckets[0].id);

    // An HTTP route touches no bucket.
    let http = mock_server_add_route(again, "PUT").expect("adds");
    assert_eq!(http.data_buckets.len(), 1);
    assert_eq!(http.routes[3].data_bucket_id, "");
    assert_eq!(
        kind(mock_server_add_route(http, "nope").expect_err("unknown")),
        "Validation"
    );
}

#[test]
fn duplicating_a_request_gets_a_fresh_id_and_a_copy_suffix() {
    let request = crate::workspace::Request::new("req_1", "ws", "List", "http://x").unwrap();
    let copy = request_duplicate(request.clone());
    assert_ne!(copy.id, request.id);
    assert_eq!(copy.name, "List copy");
    assert_eq!(copy.url, request.url);
}
