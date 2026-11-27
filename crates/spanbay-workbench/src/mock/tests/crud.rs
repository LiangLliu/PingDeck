//! `Crud` route behaviour against data buckets.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::workspace::items::MockDataBucket;
use crate::workspace::models::KeyValue;

use super::super::crud::next_crud_id;
use super::super::server::{MockServerManager, ServerState};
use super::support::*;

#[test]
fn crud_routes_mutate_buckets_respect_explicit_routes_cors_and_id_rules() {
    let tokio_runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let runtime = ServerState::new(
        server(Vec::new()),
        SocketAddr::from(([127, 0, 0, 1], 0)),
        reqwest::Client::new(),
        200,
        tokio_runtime.handle().clone(),
        None,
        std::sync::Arc::default(),
    );
    assert!(next_crud_id(&runtime, &[serde_json::json!({ "id": i64::MAX })], "id").is_string());
    assert_eq!(
        next_crud_id(&runtime, &[serde_json::json!({ "id": 7 })], "id"),
        8
    );

    let mut config = server(vec![
        crud_route("route_users", "/users", "bucket_users"),
        crud_route("route_root", "/", "bucket_items"),
        route(
            "route_dump",
            "GET",
            "/dump",
            vec![response("resp_dump", 200, "{{data 'Users'}}", vec![])],
        ),
        route(
            "route_me",
            "GET",
            "/users/me",
            vec![response("resp_me", 200, "me", vec![])],
        ),
        route(
            "route_login",
            "POST",
            "/users/login",
            vec![response("resp_login", 200, "login", vec![])],
        ),
    ]);
    config.cors_enabled = true;
    config.headers = vec![KeyValue {
        key: "x-global".into(),
        value: "g".into(),
        enabled: true,
        description: None,
    }];
    config.data_buckets = vec![
        MockDataBucket {
            id: "bucket_users".into(),
            name: "Users".into(),
            documentation: String::new(),
            value_json: r#"[{"id":1,"name":"Ada"}]"#.into(),
        },
        MockDataBucket {
            id: "bucket_items".into(),
            name: "Items".into(),
            documentation: String::new(),
            value_json: r#"[{"id":"8"},{"id":7}]"#.into(),
        },
    ];

    let manager = MockServerManager::new();
    let bound = manager.start(config.clone()).unwrap();

    assert_eq!(get(bound, "/users").2, r#"[{"id":1,"name":"Ada"}]"#);
    assert_eq!(
        get(bound, "/users?name=Ada").2,
        r#"[{"id":1,"name":"Ada"}]"#
    );
    assert_eq!(get(bound, "/users?name=Grace").2, "[]");
    assert_eq!(get(bound, "/users/1").2, r#"{"id":1,"name":"Ada"}"#);
    assert_eq!(get(bound, "/users/9").0, 404);
    assert_eq!(get(bound, "/users/me").2, "me");
    assert_eq!(
        send_with_body(bound, "POST", "/users/login", "{}").2,
        "login"
    );
    assert_eq!(get(bound, "/7").2, r#"{"id":7}"#);
    let null_id: Value =
        serde_json::from_str(&send_with_body(bound, "POST", "/", r#"{"id":null,"n":1}"#).2)
            .unwrap();
    assert_eq!(null_id["id"], 9);

    let created: Value =
        serde_json::from_str(&send_with_body(bound, "POST", "/users", r#"{"name":"Grace"}"#).2)
            .unwrap();
    assert_eq!(
        (&created["id"], &created["name"]),
        (&Value::from(2), &Value::from("Grace"))
    );
    let patched: Value = serde_json::from_str(
        &send_with_body(bound, "PATCH", "/users/2", r#"{"name":"Grace Hopper"}"#).2,
    )
    .unwrap();
    assert_eq!(patched["name"], "Grace Hopper");
    assert_eq!(
        send_with_body(bound, "PUT", "/users/2", r#"{"name":"G"}"#).0,
        200
    );
    assert!(get(bound, "/dump").2.contains(r#""name":"G""#));
    assert_eq!(send_with_body(bound, "DELETE", "/users/2", "").0, 200);
    assert_eq!(get(bound, "/users/2").0, 404);

    let (status, headers, _) = send(
        bound,
        "GET /users HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://localhost:3000\r\n\r\n",
    );
    assert_eq!(
        (
            status,
            header(&headers, "x-global"),
            header(&headers, "access-control-allow-origin")
        ),
        (200, "g", "http://localhost:3000")
    );
    let (status, _, body) = send(
        bound,
        "OPTIONS /users/1 HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://localhost:3000\r\n\r\n",
    );
    assert_eq!((status, body.is_empty()), (204, true));

    let _ = send_with_body(bound, "POST", "/users", r#"{"name":"Live"}"#);
    config.cors_enabled = false;
    config.latency_ms = 50;
    assert!(manager.update(config.clone()));
    let started = Instant::now();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&get(bound, "/users").2).unwrap(),
        serde_json::json!([{"id": 1, "name": "Ada"}, {"id": 2, "name": "Live"}])
    );
    assert!(started.elapsed() >= Duration::from_millis(50));
    assert_eq!(
        send(bound, "OPTIONS /users HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").0,
        405
    );

    config.latency_ms = 0;
    config.data_buckets[0].value_json = r#"[{"id":10,"name":"Edited"}]"#.into();
    assert!(manager.update(config));
    assert_eq!(get(bound, "/users").2, r#"[{"id":10,"name":"Edited"}]"#);
}

#[test]
fn crud_bucket_item_and_byte_caps_recover_after_deletes_and_replacements() {
    let items: Vec<String> = (0..10_000).map(|id| format!(r#"{{"id":{id}}}"#)).collect();
    let mut config = server(vec![crud_route("route_users", "/users", "bucket_users")]);
    config.data_buckets = vec![MockDataBucket {
        id: "bucket_users".into(),
        name: "Users".into(),
        documentation: String::new(),
        value_json: format!("[{}]", items.join(",")),
    }];
    let manager = MockServerManager::new();
    let bound = manager.start(config.clone()).unwrap();
    assert_eq!(
        send_with_body(bound, "POST", "/users", r#"{"name":"late"}"#).0,
        507
    );
    assert_eq!(send_with_body(bound, "DELETE", "/users/0", "").0, 200);
    assert_eq!(
        send_with_body(bound, "POST", "/users", r#"{"name":"fits"}"#).0,
        201
    );

    let blob = "x".repeat(16 * 1024 * 1024 - 100);
    config.data_buckets[0].value_json = format!(r#"[{{"id":1,"blob":"{blob}"}}]"#);
    assert!(manager.update(config));
    let big = format!(r#"{{"name":"{}"}}"#, "y".repeat(900 * 1024));
    assert_eq!(send_with_body(bound, "POST", "/users", &big).0, 413);
    assert_eq!(send_with_body(bound, "PATCH", "/users/1", &big).0, 413);
    assert_eq!(
        send_with_body(bound, "PUT", "/users/1", r#"{"blob":"small"}"#).0,
        200
    );
    assert_eq!(send_with_body(bound, "POST", "/users", &big).0, 201);
    assert_eq!(send_with_body(bound, "DELETE", "/users/1", "").0, 200);
    for _ in 0..17 {
        assert_eq!(send_with_body(bound, "POST", "/users", &big).0, 201);
    }
    assert_eq!(send_with_body(bound, "POST", "/users", &big).0, 413);
}
