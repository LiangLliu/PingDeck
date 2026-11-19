//! Rule layer of the demo/core conformance checks.
//!
//! Each test runs one drift-prone rule on a real `AppState` and freezes inputs and outcomes in
//! `fixtures/demo-conformance/<rule>.json`; `src/api/demoConformance.test.ts` replays the same
//! inputs on the browser demo and expects the same outcomes. A changed outcome is stale until
//! regenerated with `UPDATE_DEMO_CONFORMANCE=1 cargo test -p pingdeck-core --test demo_conformance
//! --locked`; then fix the demo, never the core, until the TypeScript side is green again.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test harness

mod agent_smoke;
mod factory_fixtures;
mod gateway_smoke;
mod platform_smoke;
mod providers;
mod streams;
mod url_sync_fixture;
mod workspace;

use std::fs;
use std::path::PathBuf;

use pingdeck_core::app::AppState;
use pingdeck_core::error::AppError;
use serde_json::{Value, json};

#[path = "../common/mod.rs"]
mod common;

fn fixture_path(rule: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/demo-conformance")
        .join(format!("{rule}.json"))
}

fn sort_json_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .into_iter()
                .map(|(k, v)| (k, sort_json_keys(v)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sort_json_keys).collect()),
        other => other,
    }
}

/// Compares the rendered fixture of `rule` with the file, or writes it under
/// `UPDATE_DEMO_CONFORMANCE`.
pub fn verify(rule: &str, note: &str, setup: Value, cases: Vec<Value>) {
    let payload =
        sort_json_keys(json!({ "rule": rule, "note": note, "setup": setup, "cases": cases }));
    let rendered = format!("{}\n", serde_json::to_string_pretty(&payload).unwrap());
    let path = fixture_path(rule);
    if fs::read_to_string(&path).ok().as_deref() == Some(rendered.as_str()) {
        return;
    }
    if std::env::var_os("UPDATE_DEMO_CONFORMANCE").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, rendered).unwrap();
        return;
    }
    panic!(
        "{} is stale: regenerate with `UPDATE_DEMO_CONFORMANCE=1 cargo test -p pingdeck-core --test demo_conformance --locked`, then make the demo match",
        path.display()
    );
}

/// `{ "ok": <projection> }` or `{ "error": "<AppError kind>" }`.
pub fn outcome<T>(result: Result<T, AppError>, project: impl FnOnce(T) -> Value) -> Value {
    match result {
        Ok(value) => json!({ "ok": project(value) }),
        Err(error) => json!({ "error": serde_json::to_value(&error).unwrap()["kind"] }),
    }
}

/// A state on a database file of its own (the LLM layer needs one); the directory goes with it.
pub struct World {
    pub state: AppState,
    pub db: PathBuf,
    _dir: common::TempDir,
}

/// Unix milliseconds now.
pub fn now_ms() -> i64 {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    i64::try_from(since.as_millis()).unwrap()
}

pub fn world(name: &str) -> World {
    let dir = common::TempDir::new(&format!("pd-conformance-{name}"));
    let db = dir.join("app.sqlite3");
    World {
        state: AppState::open(&db).unwrap(),
        db,
        _dir: dir,
    }
}
