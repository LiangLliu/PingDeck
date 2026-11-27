//! Mockoon environment import tests.

use crate::workspace::items::{MockResponseMode, MockRouteType, MockRuleTarget, MockRulesOperator};
use crate::workspace::models::WorkspaceId;

use super::super::mockoon::import_mockoon_environment;

#[test]
fn imports_mockoon_environment_with_warnings_and_rejects_invalid_json() {
    let workspace_id = WorkspaceId::new("ws_mock").unwrap();
    assert!(import_mockoon_environment(&workspace_id, "{").is_err());
    assert!(import_mockoon_environment(&workspace_id, "[]").is_err());

    let environment = serde_json::json!({
        "uuid": "env_uuid", "name": "Pet Store", "port": 3010, "endpointPrefix": "api",
        "latency": 25, "cors": false, "headers": [{"key": "x-env", "value": "store"}],
        "proxyMode": true, "proxyHost": "https://api.example.test", "proxyRemovePrefix": true,
        "proxyReqHeaders": [{"key": "x-from", "value": "mock"}],
        "proxyResHeaders": [{"key": "x-proxied", "value": "true"}],
        "data": [{"uuid": "bucket_uuid", "name": "Pets", "value": "[{\"id\":1}]", "documentation": "Pets"}],
        "callbacks": [
            {"uuid": "callback_uuid", "name": "notify", "method": "POST", "endpoint": "http://example.test/hook", "body": "ok", "latency": 5, "headers": [{"key": "x-hook", "value": "1"}]},
            {"uuid": "callback_bad", "name": "broken", "method": "TRACE", "endpoint": "http://example.test/hook"}
        ],
        "routes": [
            {
                "uuid": "route_uuid", "type": "http", "method": "get", "endpoint": "pets/:id",
                "documentation": "One pet", "responseMode": "SEQUENTIAL",
                "responses": [{
                    "uuid": "response_uuid", "label": "Found", "statusCode": 200,
                    "body": "{\"id\":\"{{urlParam 'id'}}\"}",
                    "headers": [{"key": "Content-Type", "value": "application/json"}],
                    "latency": 5,
                    "rules": [{"target": "query", "modifier": "species", "operator": "equals", "value": "cat"}],
                    "rulesOperator": "OR",
                    "callbacks": [{"id": "callback_uuid"}]
                }]
            },
            {"uuid": "route_crud", "type": "crud", "method": "all", "endpoint": "pets", "databucketID": "bucket_uuid", "responseMode": "DISABLE_RULES", "responses": [{"uuid": "response_crud", "statusCode": 200, "body": ""}]},
            {"type": "graphql", "method": "post", "endpoint": "graphql"}
        ]
    });
    let (server, warnings) =
        import_mockoon_environment(&workspace_id, &environment.to_string()).unwrap();
    assert_eq!(
        (
            server.id.as_str(),
            server.name.as_str(),
            server.port,
            server.endpoint_prefix.as_str(),
            server.latency_ms,
            server.cors_enabled
        ),
        ("env_uuid", "Pet Store", 3010, "api", 25, false)
    );
    assert!(
        !server.proxy_enabled
            && server.proxy_remove_prefix
            && server.proxy_base_url == "https://api.example.test"
    );
    assert_eq!(
        (
            server.data_buckets.len(),
            server.callbacks.len(),
            server.routes.len()
        ),
        (1, 1, 2)
    );
    for needle in [
        "proxy mode was disabled on import",
        "TRACE",
        "1 callback(s) imported but not attached",
        "graphql",
    ] {
        assert!(warnings.iter().any(|w| w.contains(needle)), "{warnings:?}");
    }
    let r0 = &server.routes[0];
    assert_eq!(
        (r0.method.as_str(), r0.path.as_str(), r0.response_mode),
        ("GET", "/pets/:id", MockResponseMode::Sequential)
    );
    assert_eq!(
        (
            r0.responses[0].rules_operator,
            r0.responses[0].rules[0].target,
            r0.responses[0].callback_ids.is_empty()
        ),
        (MockRulesOperator::Or, MockRuleTarget::Query, true)
    );
    assert_eq!(
        (
            server.routes[1].route_type,
            server.routes[1].method.as_str(),
            server.routes[1].response_mode
        ),
        (MockRouteType::Crud, "ANY", MockResponseMode::DisableRules)
    );
}

#[test]
fn hostile_imports_and_unsupported_regex_rules_validate_cleanly() {
    let workspace_id = WorkspaceId::new("ws_mock").unwrap();
    let environment = serde_json::json!({
        "uuid": "", "name": "  ", "port": 3010, "latency": 9_999_999_999_u64,
        "data": [{"uuid": "", "name": "", "value": "[]"}],
        "routes": [
            {"uuid": "", "type": "http", "method": "trace", "endpoint": "traced", "responses": [{"uuid": "", "statusCode": 200}]},
            {"type": "http", "method": "propfind", "endpoint": "dav", "responses": [{"statusCode": 200}]},
            {"uuid": "", "type": "crud", "method": "all", "endpoint": "orphans", "databucketID": "", "responses": [{"statusCode": 200}]},
            {"uuid": "", "type": "crud", "method": "all", "endpoint": "pets", "databucketID": "bucket_uuid", "crudKey": "slug", "responses": [{"statusCode": 200}]},
            {
                "uuid": "", "type": "http", "method": "get", "endpoint": "users",
                "responses": [{
                    "uuid": "", "label": "Not found", "statusCode": 404, "latency": 9_999_999_999_u64,
                    "rules": [
                        {"uuid": "", "target": "query", "modifier": "a", "operator": "equals", "value": "(?=x)"},
                        {"uuid": "", "target": "query", "modifier": "b", "operator": "regex", "value": "^(?!admin)"},
                        {"uuid": "", "target": "header", "modifier": "c", "operator": "regex_i", "value": "^ok$"}
                    ]
                }]
            }
        ]
    });
    let (server, warnings) =
        import_mockoon_environment(&workspace_id, &environment.to_string()).unwrap();
    assert_eq!(
        (server.name.as_str(), server.latency_ms, server.routes.len()),
        ("Imported mock", 60_000, 2)
    );
    assert_eq!(
        (
            server.routes[0].data_bucket_id.as_str(),
            server.routes[0].crud_id_key.as_str()
        ),
        ("bucket_uuid", "slug")
    );
    assert_eq!(server.routes[1].responses[0].latency_ms, 60_000);
    let values: Vec<_> = server.routes[1].responses[0]
        .rules
        .iter()
        .map(|r| r.value.as_str())
        .collect();
    // The look-ahead regex SpanBay cannot run stays as a rule that never matches.
    assert_eq!(values, ["(?=x)", "0", "^ok$"]);
    assert_eq!(
        server.routes[1].responses[0].rules[1].target,
        MockRuleTarget::RequestNumber
    );
    for needle in [
        "'TRACE'",
        "'PROPFIND'",
        "no data bucket",
        "clamped",
        "negative look-ahead",
    ] {
        assert!(warnings.iter().any(|w| w.contains(needle)), "{warnings:?}");
    }
}

/// An imported Mockoon route answers as Mockoon does. The rule-less response before the
/// default never matches, so a request no rule matches gets the default.
#[test]
fn imported_routes_answer_like_mockoon() {
    let workspace_id = WorkspaceId::new("ws_mock").unwrap();
    let response = |uuid: &str, default: bool, rules: serde_json::Value| serde_json::json!({"uuid": uuid, "statusCode": 200, "body": uuid, "default": default, "rules": rules});
    let environment = serde_json::json!({
        "uuid": "env_select", "name": "Select", "port": 3010, "endpointPrefix": "",
        "routes": [{
            "uuid": "route_select", "type": "http", "method": "get", "endpoint": "pick",
            "responses": [
                response("ruled", false, serde_json::json!([{"target": "query", "modifier": "x", "operator": "equals", "value": "1"}])),
                response("plain", false, serde_json::json!([])),
                response("fallback", true, serde_json::json!([]))
            ]
        }]
    });
    let (mut server, _) =
        import_mockoon_environment(&workspace_id, &environment.to_string()).unwrap();
    server.port = 0;
    let manager = super::super::server::MockServerManager::new();
    let bound = manager.start(server).unwrap();
    assert_eq!(
        (
            super::support::get(bound, "/pick?x=1").2,
            super::support::get(bound, "/pick?x=2").2
        ),
        ("ruled".to_owned(), "fallback".to_owned())
    );
}

#[test]
fn file_and_data_bucket_bodies_import_and_unrendered_helpers_are_named() {
    use crate::workspace::items::MockBodySource;

    let workspace_id = WorkspaceId::new("ws_mock").unwrap();
    let response = |uuid: &str, extra: serde_json::Value| {
        let mut value = serde_json::json!({"uuid": uuid, "statusCode": 200});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        value
    };
    let environment = serde_json::json!({
        "uuid": "env_bodies", "name": "Bodies", "port": 3010,
        "data": [{"uuid": "bucket_pets", "name": "Pets", "value": "[]"}],
        "routes": [{
            "uuid": "route_bodies", "type": "http", "method": "get", "endpoint": "bodies",
            "responses": [
                response("file_abs", serde_json::json!({"bodyType": "FILE", "filePath": "/srv/data/pets.json"})),
                response("file_rel", serde_json::json!({"bodyType": "FILE", "filePath": "pets.json"})),
                response("bucket", serde_json::json!({"bodyType": "DATABUCKET", "databucketID": "bucket_pets"})),
                response("helpers", serde_json::json!({"body": "{{setVar 'a' 1}}{{#each (data 'Pets')}}{{id}}{{/each}}{{faker 'music.genre'}}"}))
            ]
        }]
    });
    let (server, warnings) =
        import_mockoon_environment(&workspace_id, &environment.to_string()).unwrap();
    let responses = &server.routes[0].responses;
    assert_eq!(
        (responses[0].body.as_str(), responses[0].body_source),
        ("/srv/data/pets.json", Some(MockBodySource::File))
    );
    assert_eq!(responses[1].body_source, Some(MockBodySource::File));
    assert_eq!(
        (responses[2].body.as_str(), responses[2].body_source),
        ("{{data 'Pets'}}", None)
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("'pets.json' is not absolute")),
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("faker 'music.genre', setVar") && !w.contains("each")),
        "{warnings:?}"
    );
    // Imported content is not authorization: an absolute file body is named so the user sees
    // which local file the mock will serve.
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("local file '/srv/data/pets.json'")),
        "{warnings:?}"
    );
}
