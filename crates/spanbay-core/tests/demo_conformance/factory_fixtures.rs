//! Pins the factory defaults and reorder/duplicate rules to `fixtures/factories-cases.json`,
//! generated from the TypeScript implementation they replaced (ids masked as `<new>`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test helpers

use std::{collections::HashSet, fs, path::Path};

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use spanbay_core::{
    app::factory_service::*,
    model::items::{Flow, FlowStep, MockRoute, MockServer, MockTransactionLog},
};

fn fixture() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/factories-cases.json");
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn collect_ids(value: &Value, out: &mut HashSet<String>) {
    match value {
        Value::Array(items) => items.iter().for_each(|item| collect_ids(item, out)),
        Value::Object(map) => {
            for (key, item) in map {
                match item {
                    Value::String(id) if key == "id" => {
                        out.insert(id.clone());
                    }
                    _ => collect_ids(item, out),
                }
            }
        }
        _ => {}
    }
}

/// Replaces every `id` that the input did not already contain by `<new>`.
fn mask(value: Value, keep: &HashSet<String>) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(|v| mask(v, keep)).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, item)| {
                    let item = match item {
                        Value::String(id) if key == "id" && !keep.contains(&id) => "<new>".into(),
                        other => mask(other, keep),
                    };
                    (key, item)
                })
                .collect(),
        ),
        other => other,
    }
}

fn input<T: DeserializeOwned>(case: &Value, key: &str) -> T {
    serde_json::from_value(case[key].clone()).unwrap()
}

fn json<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap()
}

/// Runs `run` over every case of `op` and compares its masked output with `expected`.
fn check(op: &str, mut run: impl FnMut(&Value) -> Value) {
    let cases = fixture()[op]
        .as_array()
        .unwrap_or_else(|| panic!("{op} has cases"))
        .clone();
    assert!(!cases.is_empty(), "{op} has cases");
    for case in cases {
        let mut keep = HashSet::new();
        collect_ids(&case, &mut keep);
        keep.remove("<new>");
        let mut actual = mask(run(&case), &keep);
        let mut expected = case["expected"].clone();
        if op == "mock_route_from_log" {
            // The TS text was a locale time; the core uses a fixed UTC format.
            actual.as_object_mut().unwrap().remove("description");
            expected.as_object_mut().unwrap().remove("description");
        }
        assert_eq!(actual, expected, "{op}: {case}");
    }
}

fn flow_with_steps(count: u64) -> Flow {
    serde_json::from_value(serde_json::json!({
        "id": "flow_1", "workspace_id": "ws_a", "name": "Flow", "description": "",
        "steps": (0..count).map(|i| serde_json::json!({
            "id": format!("s{i}"), "name": "s", "request_id": "", "delay_ms": 0,
            "continue_on_error": false, "position": i
        })).collect::<Vec<_>>(),
        "updated_at_ms": 0
    }))
    .unwrap()
}

#[test]
fn assertion_defaults() {
    let expected = fixture()["assertion_new"].as_object().unwrap().clone();
    assert_eq!(expected.len(), assertion_kinds().len());
    for info in assertion_kinds() {
        assert_eq!(
            json(&assertion_new(&info.id).unwrap()),
            expected[&info.id],
            "{}",
            info.id
        );
    }
}

#[test]
fn entity_defaults() {
    let position = |case: &Value| usize::try_from(case["position"].as_u64().unwrap()).unwrap();
    check("mock_route_new", |c| {
        json(&mock_route_new(c["kind"].as_str().unwrap(), position(c)).unwrap())
    });
    check("mock_response_new", |c| {
        json(&mock_response_new(position(c)))
    });
    check("mock_callback_new", |c| {
        json(&mock_callback_new(position(c)))
    });
    check("mock_rule_new", |c| json(&mock_rule_new(position(c))));
    check("mock_bucket_new", |c| {
        json(&mock_bucket_new(c["name"].as_str().unwrap()))
    });
    check("flow_step_new", |c| {
        json(&flow_step_new(&flow_with_steps(
            c["steps"].as_u64().unwrap(),
        )))
    });
}

#[test]
fn reorder_duplicate_remove() {
    let range = |c: &Value| {
        let n = |k: &str| usize::try_from(c[k].as_u64().unwrap()).unwrap();
        (n("from"), n("to"))
    };
    let id = |c: &Value| c["id"].as_str().unwrap().to_owned();
    check("mock_route_duplicate", |c| {
        json(&mock_route_duplicate(input::<Vec<MockRoute>>(c, "routes"), &id(c)).unwrap())
    });
    check("mock_response_duplicate", |c| {
        json(&mock_response_duplicate(input::<MockRoute>(c, "route"), &id(c)).unwrap())
    });
    check("flow_steps_reorder", |c| {
        let (from, to) = range(c);
        json(&flow_steps_reorder(input::<Vec<FlowStep>>(c, "steps"), from, to).unwrap())
    });
    check("flow_step_duplicate", |c| {
        json(&flow_step_duplicate(input::<Vec<FlowStep>>(c, "steps"), &id(c)).unwrap())
    });
    check("flow_step_remove", |c| {
        json(&flow_step_remove(input::<Vec<FlowStep>>(c, "steps"), &id(c)).unwrap())
    });
}

#[test]
fn mock_route_helpers() {
    check("mock_route_from_log", |c| {
        let (server, log): (MockServer, MockTransactionLog) = (input(c, "server"), input(c, "log"));
        json(&mock_route_from_log(&server, &log))
    });
    check("mock_route_test_request", |c| {
        let (server, route): (MockServer, MockRoute) = (input(c, "server"), input(c, "route"));
        json(&mock_route_test_request(&server, &route))
    });
}
