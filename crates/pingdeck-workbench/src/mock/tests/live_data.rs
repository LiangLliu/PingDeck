//! Live data: the buckets CRUD routes change, read next to the saved data and reset.

use serde_json::{Value, json};

use crate::workspace::items::MockDataBucket;

use super::super::live_data::{MAX_BUCKET_BYTES, MockDataResetError};
use super::super::server::MockServerManager;
use super::support::*;

fn bucket(id: &str, name: &str, value: &str) -> MockDataBucket {
    MockDataBucket {
        id: id.into(),
        name: name.into(),
        documentation: String::new(),
        value_json: value.into(),
    }
}

/// A server with CRUD routes over `users` and `pets`.
fn two_bucket_server() -> crate::workspace::items::MockServer {
    let mut config = server(vec![
        crud_route("route_users", "/users", "b_users"),
        crud_route("route_pets", "/pets", "b_pets"),
    ]);
    config.data_buckets = vec![
        bucket("b_users", "Users", r#"[{"id":1,"name":"Ada"}]"#),
        bucket("b_pets", "Pets", r#"[{"id":1,"name":"Rex"}]"#),
    ];
    config
}

fn live(manager: &MockServerManager, id: &str) -> Vec<(String, Value, Option<u32>, bool, bool)> {
    manager
        .live_data(id)
        .expect("the server runs")
        .into_iter()
        .map(|b| {
            let value = serde_json::from_str(&b.value_json).unwrap_or(Value::Null);
            (b.id, value, b.items, b.changed, b.truncated)
        })
        .collect()
}

#[test]
fn live_data_tracks_crud_mutations_resets_hot_reloads_and_bounds_size() {
    let mut config = two_bucket_server();
    let manager = MockServerManager::new();
    let id = config.id.clone();
    assert_eq!(manager.live_data(&id), None);
    let bound = manager.start(config.clone()).unwrap();

    let before = live(&manager, &id);
    assert!(before.iter().all(|r| !r.3 && !r.4) && before[0].2 == Some(1));

    assert_eq!(
        send_with_body(bound, "POST", "/users", r#"{"name":"Grace"}"#).0,
        201
    );
    send_with_body(bound, "POST", "/pets", r#"{"name":"Tom"}"#);
    let after = live(&manager, &id);
    assert_eq!(
        (&after[0].1, after[0].2, after[0].3, after[1].3),
        (
            &json!([{"id":1,"name":"Ada"},{"id":2,"name":"Grace"}]),
            Some(2),
            true,
            true
        )
    );

    config.latency_ms = 1;
    assert!(manager.update(config.clone()));
    assert!(live(&manager, &id).iter().all(|r| r.3));

    config.data_buckets[1].value_json = r#"[{"id":1,"name":"Rex"},{"id":9,"name":"New"}]"#.into();
    assert!(manager.update(config.clone()));
    let reloaded = live(&manager, &id);
    assert!(reloaded[0].3 && !reloaded[1].3 && reloaded[1].2 == Some(2));

    manager.reset_live_data(&id, Some("b_users")).unwrap();
    assert!(!live(&manager, &id)[0].3);
    send_with_body(bound, "POST", "/users", r#"{"name":"Grace"}"#);
    manager.reset_live_data(&id, None).unwrap();
    assert!(live(&manager, &id).iter().all(|r| !r.3));
    assert_eq!(
        manager.reset_live_data(&id, Some("nope")),
        Err(MockDataResetError::UnknownBucket)
    );
    assert_eq!(
        manager.reset_live_data("ghost", None),
        Err(MockDataResetError::NotRunning)
    );

    assert!(manager.stop(&id));
    let mut bad = server(vec![crud_route("route_x", "/x", "b_x")]);
    bad.data_buckets = vec![bucket("b_x", "X", "{ not json")];
    let _ = manager.start(bad).unwrap();
    let rows = live(&manager, &id);
    assert_eq!((&rows[0].1, rows[0].3), (&json!([]), false));

    let big_text = format!("[\"{}\"]", "x".repeat(MAX_BUCKET_BYTES + 10));
    let mut big_cfg = server(Vec::new());
    big_cfg.data_buckets = (0..6)
        .map(|i| bucket(&format!("b{i}"), &format!("B{i}"), &big_text))
        .collect();
    assert!(manager.update(big_cfg));
    let buckets = manager.live_data(&id).unwrap();
    assert!(buckets.iter().all(|b| b.truncated && b.items == Some(1)));
    assert_eq!(buckets[0].value_json.len(), MAX_BUCKET_BYTES);
    assert!(
        buckets.iter().map(|b| b.value_json.len()).sum::<usize>()
            <= super::super::live_data::MAX_TOTAL_BYTES
    );
    assert!(buckets[5].value_json.is_empty());
}
