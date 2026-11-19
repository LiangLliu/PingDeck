//! Workbench rules: mock server validation, the endpoints of a CRUD route, the history row
//! shape and duplicate environment variables.

use pingdeck_core::app::factory_service::mock_crud_endpoints;
use pingdeck_core::app::misc_logic_service::{history_page_impl, validate_mock_server};
use pingdeck_core::app::{
    environment_save_impl, history_get_impl, history_restore_many_impl, mock_server_save_impl,
    workspace_create_impl,
};
use pingdeck_core::model::items::{MockRoute, MockServer};
use serde_json::{Value, json};

use crate::{outcome, verify, world};

const WS: &str = "ws_billing";

/// Sets `value` at a dotted path (`routes.0.method`) of `target`.
fn patch(target: &mut Value, path: &str, value: &Value) {
    let pointer = format!("/{}", path.replace('.', "/"));
    *target
        .pointer_mut(&pointer)
        .unwrap_or_else(|| panic!("no {path}")) = value.clone();
}

/// A mock server with one HTTP route and one response, as the base of the patches.
fn http_server() -> Value {
    let mut v: Value = serde_json::from_str(
        r#"{
        "id": "mock_1",
        "workspace_id": "ws_billing",
        "name": "Users API",
        "port": 3000,
        "endpoint_prefix": "api",
        "latency_ms": 10,
        "cors_enabled": true,
        "headers": [],
        "proxy_enabled": false,
        "proxy_base_url": "",
        "proxy_remove_prefix": false,
        "proxy_req_headers": [],
        "proxy_res_headers": [],
        "data_buckets": [{"id": "bucket_1", "name": "users", "documentation": "", "value_json": "[]"}],
        "callbacks": [],
        "routes": [{
            "id": "route_1",
            "method": "GET",
            "path": "/users/:id",
            "description": "",
            "enabled": true,
            "response_mode": "Sequential",
            "route_type": "Http",
            "data_bucket_id": "bucket_1",
            "crud_id_key": "id",
            "position": 0,
            "responses": [{
                "id": "resp_1",
                "label": "ok",
                "status_code": 200,
                "headers": [],
                "body": "{}",
                "latency_ms": 0,
                "is_default": true,
                "rules_operator": "And",
                "rules": [],
                "callback_ids": [],
                "position": 0
            }]
        }],
        "updated_at_ms": 1700000000000
    }"#,
    )
    .unwrap();
    v["workspace_id"] = json!(WS);
    v
}

#[test]
fn mock_validation() {
    let w = world("mock");
    workspace_create_impl(&w.state, WS.into(), "Billing".into()).unwrap();
    let patches: Vec<(&str, Vec<(&str, Value)>)> = vec![
        ("valid", vec![]),
        ("blank name", vec![("name", json!("  "))]),
        ("port 0", vec![("port", json!(0))]),
        ("latency over 60 s", vec![("latency_ms", json!(60_001))]),
        (
            "route path without a slash",
            vec![("routes.0.path", json!("users"))],
        ),
        (
            "unsupported method",
            vec![("routes.0.method", json!("FETCH"))],
        ),
        ("lower-case method", vec![("routes.0.method", json!("get"))]),
        ("wildcard method", vec![("routes.0.method", json!("*"))]),
        (
            "CRUD route without a bucket",
            vec![
                ("routes.0.route_type", json!("Crud")),
                ("routes.0.data_bucket_id", json!("")),
            ],
        ),
        (
            "status 99",
            vec![("routes.0.responses.0.status_code", json!(99))],
        ),
        (
            "status 600",
            vec![("routes.0.responses.0.status_code", json!(600))],
        ),
        (
            "response latency over 60 s",
            vec![("routes.0.responses.0.latency_ms", json!(60_001))],
        ),
        (
            "every issue, in order",
            vec![
                ("routes.0.responses.0.status_code", json!(42)),
                ("routes.0.path", json!("")),
                ("name", json!("")),
            ],
        ),
    ];
    let cases = patches
        .into_iter()
        .map(|(name, changes)| {
            let mut server = http_server();
            changes
                .iter()
                .for_each(|(path, value)| patch(&mut server, path, value));
            let server: MockServer = serde_json::from_value(server).unwrap();
            let fields: Vec<String> = validate_mock_server(&server)
                .into_iter()
                .map(|issue| issue.field)
                .collect();
            let result = mock_server_save_impl(&w.state, server);
            let changes: Vec<Value> = changes
                .into_iter()
                .map(|(path, value)| json!([path, value]))
                .collect();
            json!({ "name": name, "patch": changes, "fields": fields, "expected": outcome(result, |_| json!("saved")) })
        })
        .collect();
    let note = "Save a valid server with one HTTP route and one response after applying `patch` ([dotted path, value] pairs). Outcome: saved or error kind; `fields` lists the issue fields in core order.";
    verify("mock-validation", note, Value::Null, cases);
}

#[test]
fn mock_crud_routes() {
    let shapes = [
        ("api", "/users"),
        ("/api/", "/users"),
        ("", "/users"),
        ("api", ""),
        ("api", "users"),
        (" v1 ", "/orders"),
    ];
    let mut cases: Vec<Value> = shapes
        .iter()
        .map(|(prefix, path)| {
            let mut server = http_server();
            server["endpoint_prefix"] = json!(prefix);
            server["routes"][0]["route_type"] = json!("Crud");
            server["routes"][0]["path"] = json!(path);
            let server: MockServer = serde_json::from_value(server).unwrap();
            let route: &MockRoute = &server.routes[0];
            let result = mock_crud_endpoints(&server, route);
            json!({ "name": format!("prefix {prefix:?}, path {path:?}"), "endpoint_prefix": prefix, "route_path": path, "route_type": "Crud",
                "expected": outcome(result, |list| json!(list.iter().map(|e| format!("{} {}", e.method, e.path)).collect::<Vec<_>>())) })
        })
        .collect();
    let server: MockServer = serde_json::from_value(http_server()).unwrap();
    cases.push(json!({ "name": "not a CRUD route", "endpoint_prefix": "api", "route_path": "/users", "route_type": "Http",
        "expected": outcome(mock_crud_endpoints(&server, &server.routes[0]), |_| Value::Null) }));
    verify(
        "mock-crud-endpoints",
        "The endpoints (`METHOD path`) a route of `route_type` at `route_path` answers on a server whose prefix is `endpoint_prefix`.",
        Value::Null,
        cases,
    );
}

#[test]
fn history_row_shape() {
    let w = world("history");
    workspace_create_impl(&w.state, WS.into(), "Billing".into()).unwrap();
    let entry: Value = serde_json::from_str(
        r#"{
        "id": "hist_full",
        "workspace_id": "ws_billing",
        "request_id": "req-1",
        "request_name": "List users",
        "method": "Post",
        "request_url": "https://api.test/users",
        "final_url": "",
        "status_code": 0,
        "status_text": "Failed",
        "elapsed_ms": 1500,
        "body_size_bytes": 0,
        "executed_at": "2026-01-01T00:00:00Z",
        "result_kind": "Failure",
        "error_kind": "Timeout",
        "error_message": "request timed out",
        "environment_id": "env-1",
        "environment_name": "",
        "source": "request",
        "source_id": "",
        "source_name": "",
        "protocol": "",
        "attempt_count": 3,
        "response_body_preview": "",
        "response_body_truncated": false,
        "response_headers_truncated": false,
        "response_headers": [{ "key": "content-type", "value": "application/json", "enabled": true }],
        "sent_request": { "method": "GET", "url": "https://api.example.com/v1", "headers": [], "body_preview": "" },
        "call_chain": {
            "auth": {
                "applied_to": "Header: Authorization (Bearer)",
                "auth_type": "bearer",
                "details": [{ "enabled": true, "key": "header", "value": "Authorization" }],
                "source": "request"
            },
            "transport": {
                "client_cert_host": "api.example.com",
                "custom_ca_count": 1,
                "protocol": "HTTP/2.0",
                "proxy_bypassed": true,
                "proxy_url": "http://user:****@proxy:8080",
                "redirect_hops": [],
                "remote_addr": "93.184.216.34:443",
                "tls_disabled_versions": ["TLS 1.0"]
            },
            "variables": []
        },
        "request_snapshot": {
            "id": "req-1",
            "workspace_id": "ws-1",
            "collection_id": "col-1",
            "folder_id": "fld-1",
            "name": "List users",
            "description": "",
            "protocol": "HttpRest",
            "mode": "http",
            "method": "Post",
            "url": "https://api.test/users",
            "path_variables": [],
            "query_params": [],
            "headers": [{ "key": "Accept", "value": "application/json", "enabled": true }],
            "auth": "Inherit",
            "body": "None",
            "pre_request_script": "",
            "test_script": "",
            "assertions": [],
            "network_options": {
                "timeout_ms": 30000,
                "follow_redirects": true,
                "max_redirects": 10,
                "verify_tls": true,
                "retry_count": 0,
                "encode_url_automatically": true,
                "send_cookies": true,
                "follow_original_method": false,
                "follow_auth_header": false,
                "remove_referer_on_redirect": false,
                "http_version": "Auto",
                "disabled_tls_versions": [],
                "proxy_url": null
            },
            "ai": null,
            "position": 0
        }
    }"#,
    )
    .unwrap();
    history_restore_many_impl(&w.state, vec![serde_json::from_value(entry).unwrap()]).unwrap();
    let full =
        serde_json::to_value(history_get_impl(&w.state, WS.into(), "hist_full".into()).unwrap())
            .unwrap();
    let page = history_page_impl(&w.state, WS.into(), Default::default(), None, None).unwrap();
    let row = serde_json::to_value(&page.entries[0]).unwrap();
    let empty = |v: Option<&Value>| {
        v.is_none_or(|v| v.is_null() || v.as_array().is_some_and(Vec::is_empty))
    };
    let mut thin: Vec<&String> = full
        .as_object()
        .unwrap()
        .iter()
        .filter(|(k, v)| !empty(Some(v)) && empty(row.get(*k)))
        .map(|(k, _)| k)
        .collect();
    thin.sort();
    let others_equal = full
        .as_object()
        .unwrap()
        .iter()
        .filter(|(k, _)| !thin.contains(k))
        .all(|(k, v)| row.get(k) == Some(v));
    let cases = vec![
        json!({ "name": "a page row of an entry with every heavy field", "expected": { "thin": thin, "others_equal": others_equal } }),
    ];
    let note = "A page row (`listHistoryPage`) leaves out the heavy fields `thin` that the entry by id (`getHistoryEntry`) has; every other field equals the entry's.";
    verify("history-row-shape", note, Value::Null, cases);
}

#[test]
fn environment_duplicates() {
    let w = world("env");
    workspace_create_impl(&w.state, WS.into(), "Billing".into()).unwrap();
    let attempts = [
        ("distinct keys", ["token", "region"]),
        ("same key twice", ["token", "token"]),
        ("keys differing in case", ["token", "Token"]),
        ("same key after trimming", ["token", " token "]),
    ];
    let cases = attempts
        .iter()
        .enumerate()
        .map(|(n, (name, keys))| {
            let variables: Vec<Value> = keys.iter().map(|k| json!({ "id": null, "key": k, "value": "v", "is_secret": false, "enabled": true, "note": "" })).collect();
            let input = serde_json::from_value(json!({ "id": format!("env_conf_{n}"), "workspace_id": WS, "name": format!("Conformance {n}"), "variables": variables })).unwrap();
            json!({ "name": name, "keys": keys, "expected": outcome(environment_save_impl(&w.state, input), |_| json!("saved")) })
        })
        .collect();
    verify(
        "environment-duplicates",
        "Save a new environment whose variables have `keys`. Outcome: saved or error kind.",
        Value::Null,
        cases,
    );
}

/// One case of `mock-select`: the route's mode, its responses in order (`id`, `default`, an
/// optional query rule `[key, value]`) and the query of the request.
type SelectCase = (
    &'static str,
    &'static str,
    Vec<(&'static str, bool, Option<(&'static str, &'static str)>)>,
    &'static str,
);

/// GET `path` on the live mock server: the body (the chosen response id), or null on a 404.
fn served_by(addr: std::net::SocketAddr, path: &str) -> Value {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    if head.starts_with("HTTP/1.1 404") {
        Value::Null
    } else {
        json!(body)
    }
}

#[test]
fn mock_select() {
    let ab = |a: Option<(&'static str, &'static str)>| vec![("a", false, a), ("b", true, None)];
    let x1 = Some(("x", "1"));
    let specs: Vec<SelectCase> = vec![
        (
            "a rule matches before the default",
            "FirstMatch",
            ab(x1),
            "x=1",
        ),
        (
            "no match answers with the default",
            "FirstMatch",
            ab(x1),
            "x=2",
        ),
        (
            "a response without rules never matches",
            "FirstMatch",
            vec![("a", false, x1), ("c", false, None), ("b", true, None)],
            "x=2",
        ),
        (
            "the default's rules take part in order",
            "FirstMatch",
            vec![
                ("d", true, Some(("y", "1"))),
                ("e", false, Some(("y", "1"))),
            ],
            "y=1",
        ),
        (
            "no default answers with the first response",
            "FirstMatch",
            vec![("n1", false, x1), ("n2", false, None)],
            "x=2",
        ),
        (
            "rules off answer with the default",
            "DisableRules",
            ab(x1),
            "x=1",
        ),
        (
            "rules off without a default answer with the first",
            "DisableRules",
            vec![("f", false, x1), ("g", false, None)],
            "x=2",
        ),
        ("fall through serves a match", "Fallback", ab(x1), "x=1"),
        (
            "fall through yields when nothing matches",
            "Fallback",
            ab(x1),
            "x=2",
        ),
    ];
    let routes: Vec<Value> = specs
        .iter()
        .enumerate()
        .map(|(n, (_, mode, responses, _))| {
            let responses: Vec<Value> = responses.iter().enumerate().map(|(p, (id, default, rule))| {
                let rules: Vec<Value> = rule.iter().map(|(key, value)| json!({ "id": format!("rule_{id}"), "target": "Query", "modifier": key, "operator": "Equals", "value": value, "invert": false, "position": 0 })).collect();
                json!({ "id": id, "label": id, "status_code": 200, "headers": [], "body": id, "latency_ms": 0, "is_default": default, "rules_operator": "And", "rules": rules, "callback_ids": [], "position": p })
            }).collect();
            json!({ "id": format!("route_{n}"), "method": "GET", "path": format!("/case/{n}"), "description": "", "enabled": true, "response_mode": mode, "route_type": "Http", "data_bucket_id": "", "crud_id_key": "id", "position": n, "responses": responses })
        })
        .collect();
    let mut server = http_server();
    server["port"] = json!(0);
    server["endpoint_prefix"] = json!("");
    server["routes"] = json!(routes);
    let server: MockServer = serde_json::from_value(server).unwrap();
    let manager = pingdeck_core::mock::MockServerManager::new();
    let addr = manager.start(server).unwrap();
    let cases = specs
        .iter()
        .enumerate()
        .map(|(n, (name, mode, responses, query))| {
            let responses: Vec<Value> = responses.iter().map(|(id, default, rule)| json!({ "id": id, "default": default, "rule": rule.map(|(k, v)| json!([k, v])) })).collect();
            json!({ "name": name, "mode": mode, "responses": responses, "query": query, "expected": served_by(addr, &format!("/case/{n}?{query}")) })
        })
        .collect();
    manager.stop("mock_1");
    verify(
        "mock-select",
        "The response (by id) a route in `mode` serves for a GET with `query`, its `responses` in order, each with an optional query rule `[key, equals value]`; null when the route yields (404).",
        Value::Null,
        cases,
    );
}
