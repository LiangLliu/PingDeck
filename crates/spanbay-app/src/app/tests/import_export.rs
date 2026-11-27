//! Postman, cURL and environment import/export and response-save tests.

use base64::Engine as _;
use std::fs;

use super::support::*;
use crate::workspace::{
    AuthConfig, CollectionId, Environment, HttpMethod, RequestBody, WorkspaceId,
};

/// A small save limit, so the oversized cases stay cheap.
const CAP: usize = 4096;

fn ws(id: &str) -> WorkspaceId {
    WorkspaceId::new(id).unwrap()
}

fn col(id: &str) -> CollectionId {
    CollectionId::new(id).unwrap()
}

#[test]
fn response_save_preserves_bytes_and_enforces_the_capture_limit() {
    use crate::app::{ResponseBodyEncoding, response_save_to_path_impl};

    let state = crate::app::AppState::open_in_memory().unwrap();
    let path = temp_path("response-save.bin");
    let bytes = [0, 0xff, 0x80, b'A'];
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    response_save_to_path_impl(
        &state,
        CAP,
        encoded,
        ResponseBodyEncoding::Base64,
        Some(path.clone()),
    )
    .expect("binary response should save");
    assert_eq!(fs::read(&path).unwrap(), bytes);

    response_save_to_path_impl(
        &state,
        CAP,
        "héllo".into(),
        ResponseBodyEncoding::Text,
        Some(path.clone()),
    )
    .expect("text response should save");
    assert_eq!(fs::read(&path).unwrap(), "héllo".as_bytes());

    let oversized = vec![0xff; CAP + 1];
    assert!(
        response_save_to_path_impl(
            &state,
            CAP,
            base64::engine::general_purpose::STANDARD.encode(oversized),
            ResponseBodyEncoding::Base64,
            Some(path.clone())
        )
        .is_err()
    );
    assert!(
        response_save_to_path_impl(
            &state,
            CAP,
            "%%%".into(),
            ResponseBodyEncoding::Base64,
            Some(path.clone())
        )
        .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), "héllo".as_bytes());
    fs::remove_file(path).unwrap();
}

#[test]
fn imported_environment_ids_cannot_overwrite_existing_workspaces() {
    let state = crate::app::AppState::open_in_memory().unwrap();
    for id in ["ws_one", "ws_two"] {
        crate::app::workspace_create_impl(&state, id.into(), id.into()).unwrap();
    }
    let original = Environment::new("env_import_collision", "ws_one", "Original").unwrap();
    state
        .store
        .lock()
        .unwrap()
        .save_environment(&original)
        .unwrap();
    let imported = crate::app::import_export_service::save_imported_environment(
        &state,
        Environment::new("env_import_collision", "ws_two", "Imported").unwrap(),
    )
    .unwrap();
    assert_ne!(imported.id, original.id);
    let store = state.store.lock().unwrap();
    assert_eq!(
        store.get_environment(&ws("ws_one"), &original.id).unwrap(),
        Some(original)
    );
    assert_eq!(
        store.get_environment(&ws("ws_two"), &imported.id).unwrap(),
        Some(imported)
    );
}

#[test]
fn imported_folder_cycle_rolls_back_collection_and_folders() {
    let state = crate::app::AppState::open_in_memory().unwrap();
    crate::app::workspace_create_impl(&state, "ws_import_cycle".into(), "Workspace".into())
        .unwrap();
    let workspace = crate::workspace::WorkspaceId::new("ws_import_cycle").unwrap();
    let collection = crate::workspace::Collection::new("col", "ws_import_cycle", "Import").unwrap();
    let root = crate::workspace::Folder::new("root", "col", "Root").unwrap();
    let first = crate::workspace::Folder::new("first", "col", "First")
        .unwrap()
        .with_parent("second")
        .unwrap();
    let second = crate::workspace::Folder::new("second", "col", "Second")
        .unwrap()
        .with_parent("first")
        .unwrap();
    let error = crate::app::import_export_service::persist_imported_collection(
        &state,
        &workspace,
        collection,
        vec![root, first, second],
        Vec::new(),
        Vec::new(),
    )
    .unwrap_err();
    assert!(error.message().contains("cycle"), "{error}");
    let store = state.store.lock().unwrap();
    assert!(
        store
            .list_collections("ws_import_cycle")
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .list_folders(&ws("ws_import_cycle"), &col("col__import_1"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn import_export_errors_carry_their_kind() {
    use crate::app::{ResponseBodyEncoding, response_save_to_path_impl};
    use crate::error::AppError;

    let state = crate::app::AppState::open_in_memory().unwrap();
    crate::app::workspace_create_impl(&state, "ws_kinds".into(), "Kinds".into()).unwrap();

    let payload = r#"{"info":{"name":"Same"},"item":[]}"#;
    let missing = crate::app::postman_import_text_impl(
        &state,
        "ws_nope".into(),
        payload.into(),
        crate::app::ImportConflict::KeepBoth,
    )
    .unwrap_err();
    assert_eq!(missing.to_string(), "workspace ws_nope not found");
    assert!(matches!(missing, AppError::NotFound { .. }));
    let export =
        crate::app::environment_export_impl(&state, "ws_kinds".into(), "env_nope".into(), false)
            .unwrap_err();
    assert_eq!(export.to_string(), "environment env_nope not found");
    assert!(matches!(export, AppError::NotFound { .. }));
    let format = crate::app::collection_import_text_impl(
        &state,
        "ws_kinds".into(),
        "xml".into(),
        String::new(),
        crate::app::ImportConflict::KeepBoth,
    )
    .unwrap_err();
    assert_eq!(format.to_string(), "unsupported import format: xml");
    assert!(matches!(format, AppError::Unsupported { .. }));
    let invalid = crate::app::environment_import_text_impl(
        &state,
        "ws_kinds".into(),
        "{".into(),
        crate::app::ImportConflict::KeepBoth,
    )
    .unwrap_err();
    assert!(matches!(invalid, AppError::Validation { .. }));
    let encoding = response_save_to_path_impl(
        &state,
        CAP,
        "%%%".into(),
        ResponseBodyEncoding::Base64,
        None,
    )
    .unwrap_err();
    assert!(matches!(encoding, AppError::Validation { .. }));
}

#[test]
fn postman_import_text_impl_imports_collection_folder_request_and_variables() {
    let state = crate::app::AppState::open_in_memory().expect("state should open");
    crate::app::workspace_create_impl(&state, "ws_import".into(), "Import".into()).unwrap();
    let payload = r#"{
            "info": {
                "name": "Import API",
                "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
            },
            "item": [
                {
                    "name": "Health",
                    "item": [
                        {
                            "name": "Ping",
                            "request": {
                                "method": "GET",
                                "header": [],
                                "url": "https://example.test/ping"
                            }
                        }
                    ]
                }
            ],
            "variable": [{ "key": "baseUrl", "value": "https://example.test" }]
        }"#;

    let result = crate::app::postman_import_text_impl(
        &state,
        "ws_import".into(),
        payload.into(),
        crate::app::ImportConflict::KeepBoth,
    )
    .unwrap();

    assert_eq!(result.collection_name, "Import API");
    assert_eq!(result.folder_count, 1);
    assert_eq!(result.request_count, 1);
    assert_eq!(result.variable_count, 1);
    assert!(result.environment_id.is_none());

    let store = state.store.lock().unwrap();
    let collections = store.list_collections("ws_import").unwrap();
    assert_eq!(collections.len(), 1);
    assert_eq!(collections[0].variables.len(), 1);
    assert_eq!(collections[0].variables[0].key, "baseUrl");
    assert_eq!(
        store
            .list_folders(&ws("ws_import"), &col(&result.collection_id))
            .unwrap()
            .len(),
        1
    );
    let requests = store.list_requests("ws_import").unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].name, "Ping");
    assert!(requests[0].auth == AuthConfig::Inherit);
    assert_eq!(store.list_environments("ws_import").unwrap().len(), 0);
}

#[test]
fn environment_import_text_impl_reads_postman_values() {
    let state = crate::app::AppState::open_in_memory().expect("state should open");
    crate::app::workspace_create_impl(&state, "ws_env_import".into(), "Env".into()).unwrap();
    let environment = crate::app::environment_import_text_impl(
        &state,
        "ws_env_import".into(),
        r#"{
              "name": "Imported",
              "values": [
                {"key": "baseUrl", "value": "https://api.test", "enabled": true},
                {"key": "token", "value": "abc", "type": "secret", "enabled": true},
                {"key": "disabled", "value": "x", "enabled": false}
              ]
            }"#
        .into(),
        crate::app::ImportConflict::KeepBoth,
    )
    .expect("environment import should succeed");

    assert_eq!(environment.name, "Imported");
    assert_eq!(environment.variables.len(), 3);
    assert!(
        environment
            .variables
            .iter()
            .any(|variable| variable.key == "token"
                && variable.is_secret
                && variable.has_secret
                && variable.value.is_none())
    );
    let stored = state.store().list_environments("ws_env_import").unwrap();
    assert_eq!(stored[0].variables[1].value.as_deref(), Some("abc"));
    assert!(
        environment
            .variables
            .iter()
            .any(|variable| variable.key == "disabled" && !variable.enabled)
    );
}

#[test]
fn collection_import_text_impl_imports_curl() {
    let state = crate::app::AppState::open_in_memory().expect("state should open");
    crate::app::workspace_create_impl(&state, "ws_curl".into(), "cURL".into()).unwrap();
    let result = crate::app::collection_import_text_impl(
            &state,
            "ws_curl".into(),
            "curl".into(),
            r#"curl -X POST 'https://api.example.com/users' -H 'Content-Type: application/json' -H 'Authorization: Bearer tok' -d '{"name":"Ada"}'"#.into(), crate::app::ImportConflict::KeepBoth,
        )
        .expect("cURL import should succeed");

    assert_eq!(result.collection_name, "api.example.com/users");
    assert_eq!(result.request_count, 1);
    let store = state.store.lock().unwrap();
    let requests = store.list_requests("ws_curl").unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, HttpMethod::Post);
    assert_eq!(requests[0].url, "https://api.example.com/users");
    assert!(matches!(
        &requests[0].body,
        RequestBody::Json { content } if content.contains("Ada")
    ));
    assert_eq!(
        requests[0].auth,
        AuthConfig::Bearer {
            token_ref: "tok".into()
        }
    );
    assert!(
        requests[0]
            .headers
            .iter()
            .any(|header| header.key == "Content-Type")
    );
}

#[test]
fn postman_export_save_to_path_impl_checks_the_path_before_exporting_and_writes_the_file() {
    let state = crate::app::AppState::open_in_memory().expect("state should open");
    crate::app::workspace_create_impl(&state, "ws_export_save".into(), "Export".into()).unwrap();

    let canceled = crate::app::postman_export_save_to_path_impl(
        &state,
        "ws_export_save".into(),
        "col_unknown".into(),
        None,
        false,
        None,
    )
    .expect("a cancelled save must not touch the (unknown) collection");
    assert!(!canceled.saved);
    assert_eq!(canceled.path, None);

    let payload = r#"{
        "info": {
            "name": "Saved API",
            "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
        },
        "item": [{
            "name": "Ping",
            "request": { "method": "GET", "header": [], "url": "https://example.test/ping" }
        }]
    }"#;
    let imported = crate::app::postman_import_text_impl(
        &state,
        "ws_export_save".into(),
        payload.into(),
        crate::app::ImportConflict::KeepBoth,
    )
    .unwrap();
    let path = temp_path("postman-export-save.json");
    let saved = crate::app::postman_export_save_to_path_impl(
        &state,
        "ws_export_save".into(),
        imported.collection_id,
        None,
        false,
        Some(path.clone()),
    )
    .expect("export should save");

    assert!(saved.saved);
    assert!(saved.path.is_some());
    let written: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap())
        .expect("saved file should be Postman JSON");
    assert_eq!(written["info"]["name"], "Saved API");
    fs::remove_file(path).unwrap();
}

#[test]
fn exports_of_another_workspaces_collection_or_environment_are_not_found() {
    let state = crate::app::AppState::open_in_memory().unwrap();
    for id in ["ws_owner", "ws_other"] {
        crate::app::workspace_create_impl(&state, id.into(), id.into()).unwrap();
    }
    let payload = r#"{"info":{"name":"Mine"},"item":[{"name":"Ping","request":{"method":"GET","url":"https://example.test"}}]}"#;
    let imported = crate::app::postman_import_text_impl(
        &state,
        "ws_owner".into(),
        payload.into(),
        crate::app::ImportConflict::KeepBoth,
    )
    .unwrap();
    let environment = Environment::new("env_owned", "ws_owner", "Owned").unwrap();
    state
        .store
        .lock()
        .unwrap()
        .save_environment(&environment)
        .unwrap();

    let foreign_collection = crate::app::postman_export_impl(
        &state,
        "ws_other".into(),
        imported.collection_id.clone(),
        None,
        false,
    )
    .unwrap_err();
    assert!(matches!(
        foreign_collection,
        crate::error::AppError::NotFound { .. }
    ));
    let foreign_environment = crate::app::postman_export_impl(
        &state,
        "ws_other".into(),
        imported.collection_id.clone(),
        Some("env_owned".into()),
        false,
    )
    .unwrap_err();
    assert!(matches!(
        foreign_environment,
        crate::error::AppError::NotFound { .. }
    ));
    let foreign_filename =
        crate::app::postman_export_suggested_filename(&state, "ws_other", &imported.collection_id)
            .unwrap_err();
    assert!(matches!(
        foreign_filename,
        crate::error::AppError::NotFound { .. }
    ));
    let foreign_env_export =
        crate::app::environment_export_impl(&state, "ws_other".into(), "env_owned".into(), false)
            .unwrap_err();
    assert!(matches!(
        foreign_env_export,
        crate::error::AppError::NotFound { .. }
    ));
    let path = temp_path("postman-export-foreign.json");
    let foreign_save = crate::app::postman_export_save_to_path_impl(
        &state,
        "ws_other".into(),
        imported.collection_id.clone(),
        None,
        false,
        Some(path.clone()),
    )
    .unwrap_err();
    assert!(matches!(
        foreign_save,
        crate::error::AppError::NotFound { .. }
    ));
    assert!(!path.exists());

    let owned = crate::app::postman_export_impl(
        &state,
        "ws_owner".into(),
        imported.collection_id.clone(),
        Some("env_owned".into()),
        false,
    )
    .unwrap()
    .json;
    assert!(owned.contains("Mine"));
    assert!(
        crate::app::environment_export_impl(&state, "ws_owner".into(), "env_owned".into(), false,)
            .unwrap()
            .contains("Owned")
    );
    assert!(
        !crate::app::postman_export_suggested_filename(&state, "ws_owner", &imported.collection_id)
            .unwrap()
            .is_empty()
    );
}

const SCRIPTED_POSTMAN: &str = r#"{
    "info": {"name": "Shop", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"},
    "auth": {"type": "bearer", "bearer": [{"key": "token", "value": "{{t}}"}]},
    "item": [
      {"name": "Orders", "item": [
        {"name": "List", "event": [{"listen": "test", "script": {"exec": ["pm.test('ok', () => {})"]}}],
         "request": {"method": "GET", "url": "https://shop.test/orders",
                     "auth": {"type": "basic", "basic": [{"key": "username", "value": "u"}, {"key": "password", "value": "p"}]}}}
      ]},
      {"name": "Ping", "request": {"method": "GET", "url": "https://shop.test/ping"}}
    ],
    "variable": [
      {"key": "base", "value": "https://shop.test"},
      {"key": "key", "value": "s3cret", "type": "secret"}
    ]
  }"#;

fn import_names(state: &crate::app::AppState, workspace: &str) -> Vec<String> {
    state
        .store()
        .list_collections(workspace)
        .unwrap()
        .into_iter()
        .map(|collection| collection.name)
        .collect()
}

#[test]
fn the_import_preview_counts_what_the_import_creates_and_writes_nothing() {
    let state = crate::app::AppState::open_in_memory().unwrap();
    crate::app::workspace_create_impl(&state, "ws_prev".into(), "Preview".into()).unwrap();
    let preview = |format: &str, payload: &str| {
        crate::app::import_preview_impl(&state, "ws_prev".into(), format.into(), payload.into())
    };

    let shop = preview("postman", SCRIPTED_POSTMAN).unwrap();
    assert_eq!(shop.name.as_deref(), Some("Shop"));
    assert_eq!(
        (shop.folders, shop.requests, shop.variables, shop.secrets),
        (Some(1), Some(2), Some(2), 1)
    );
    assert!(shop.scripts);
    assert!(shop.script_issues.is_empty());
    assert_eq!(shop.auth, ["bearer", "basic"]);
    let required = preview(
        "postman",
        r#"{"info":{"name":"Req"},"item":[{"name":"Sign","event":[{"listen":"prerequest","script":{"exec":["pm.test('ok', () => {});","const lodash = require('lodash');"]}}],"request":{"method":"GET","url":"https://x.test"}}]}"#,
    )
    .unwrap();
    let issues: Vec<_> = required
        .script_issues
        .iter()
        .map(|issue| (issue.api.as_str(), issue.location.as_str(), issue.line))
        .collect();
    assert_eq!(issues, [("require", "Sign", 2)]);
    assert_eq!(shop.clash.existing_id, None);
    assert!(
        import_names(&state, "ws_prev").is_empty(),
        "a preview writes nothing"
    );

    // After the import the same payload reports the clash.
    let imported = crate::app::postman_import_text_impl(
        &state,
        "ws_prev".into(),
        SCRIPTED_POSTMAN.into(),
        crate::app::ImportConflict::KeepBoth,
    )
    .unwrap();
    let again = preview("postman", SCRIPTED_POSTMAN).unwrap();
    assert_eq!(
        again.clash.existing_id.as_deref(),
        Some(imported.collection_id.as_str())
    );

    let curl = preview("curl", "curl -X POST https://x.test/a -u me:pw").unwrap();
    assert_eq!((curl.requests, curl.folders), (Some(1), Some(0)));
    assert_eq!(curl.auth, ["basic"]);

    let environment = preview(
        "environment",
        r#"{"name":"Local","values":[{"key":"a","value":"1"},{"key":"b","value":"x","type":"secret"}]}"#,
    )
    .unwrap();
    assert_eq!(
        (
            environment.variables,
            environment.secrets,
            environment.name.as_deref()
        ),
        (Some(2), 1, Some("Local"))
    );
    assert_eq!(environment.clash.existing_id, None);

    let mockoon = preview(
        "mockoon",
        r#"{"name":"Pets","port":3001,"routes":[{"uuid":"r1","type":"http","method":"get","endpoint":"pets","responses":[{"uuid":"x","statusCode":200,"body":"[]","label":"ok","headers":[],"rules":[],"latency":0,"default":true,"filePath":"","sendFileAsBody":false,"databucketID":"","disableTemplating":false}]}]}"#,
    )
    .unwrap();
    assert_eq!(
        (mockoon.routes, mockoon.name.as_deref()),
        (Some(1), Some("Pets"))
    );

    assert!(matches!(
        preview("postman", "{"),
        Err(crate::error::AppError::Validation { .. })
    ));
    assert!(matches!(
        preview("zip", "{}"),
        Err(crate::error::AppError::Unsupported { .. })
    ));
    assert!(import_names(&state, "ws_prev").len() == 1);
}

#[test]
fn a_collection_name_clash_follows_the_conflict_choice() {
    use crate::app::ImportConflict::{KeepBoth, Replace, Skip};
    let state = crate::app::AppState::open_in_memory().unwrap();
    crate::app::workspace_create_impl(&state, "ws_conf".into(), "Conflict".into()).unwrap();
    let import = |on_conflict| {
        crate::app::postman_import_text_impl(
            &state,
            "ws_conf".into(),
            SCRIPTED_POSTMAN.into(),
            on_conflict,
        )
        .unwrap()
    };
    let first = import(KeepBoth);
    assert!(!first.skipped && first.replaced == 0);

    // Skip: nothing written, the existing collection reported.
    let skipped = import(Skip);
    assert!(skipped.skipped);
    assert_eq!(skipped.collection_id, first.collection_id);
    assert_eq!((skipped.request_count, skipped.folder_count), (0, 0));
    assert_eq!(state.store().list_requests("ws_conf").unwrap().len(), 2);

    // Keep both: a second collection of the same name.
    let both = import(KeepBoth);
    assert_ne!(both.collection_id, first.collection_id);
    assert_eq!(import_names(&state, "ws_conf"), ["Shop", "Shop"]);

    // A flow step calls a request of the second copy, which a replace removes: the preview counts
    // it, and pairs both requests of the file with the first copy.
    let called = state
        .store()
        .list_requests("ws_conf")
        .unwrap()
        .into_iter()
        .find(|r| r.collection_id.as_ref().map(|c| c.as_str()) == Some(&both.collection_id))
        .unwrap()
        .id;
    let flow: crate::workspace::Flow = serde_json::from_value(serde_json::json!({
        "id": "flow_conf", "workspace_id": "ws_conf", "name": "f", "description": "",
        "updated_at_ms": 0,
        "steps": [{"id": "s1", "name": "s", "request_id": called.as_str(), "delay_ms": 0,
                   "continue_on_error": false, "position": 0}],
    }))
    .unwrap();
    state.store().save_flow(&flow).unwrap();
    let preview = crate::app::import_preview_impl(
        &state,
        "ws_conf".into(),
        "postman".into(),
        SCRIPTED_POSTMAN.into(),
    )
    .unwrap();
    assert_eq!(
        (preview.affected_flow_steps, preview.matched_requests),
        (1, 2)
    );

    // Replace: the first copy becomes the file under its own ids, the other copy goes.
    let first_ids: Vec<String> = state
        .store()
        .list_requests("ws_conf")
        .unwrap()
        .into_iter()
        .filter(|r| r.collection_id.as_ref().map(|c| c.as_str()) == Some(&first.collection_id))
        .map(|r| r.id.as_str().to_owned())
        .collect();
    let replaced = import(Replace);
    assert_eq!((replaced.replaced, replaced.updated), (1, 2));
    assert_eq!(replaced.collection_id, first.collection_id);
    let store = state.store();
    let collections = store.list_collections("ws_conf").unwrap();
    assert_eq!(collections.len(), 1);
    let mut kept: Vec<String> = store
        .list_requests("ws_conf")
        .unwrap()
        .into_iter()
        .map(|r| r.id.as_str().to_owned())
        .collect();
    kept.sort();
    let mut expected_ids = first_ids.clone();
    expected_ids.sort();
    assert_eq!(kept, expected_ids);
    drop(store);

    // Undo: both copies come back under their ids, the step works again.
    let token = replaced.undo_token.expect("a replace offers undo");
    let report = crate::app::restore_deleted_impl(&state, "ws_conf".into(), token).unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    let mut ids: Vec<String> = state
        .store()
        .list_collections("ws_conf")
        .unwrap()
        .into_iter()
        .map(|c| c.id.as_str().to_owned())
        .collect();
    ids.sort();
    let mut expected = vec![first.collection_id.clone(), both.collection_id.clone()];
    expected.sort();
    assert_eq!(ids, expected);
    let refs =
        crate::app::flow_ref_list_impl(&state, "ws_conf".into(), called.as_str().into()).unwrap();
    assert_eq!(refs.len(), 1);
    // Replace with nothing to replace is a plain import.
    let other = crate::app::collection_import_text_impl(
        &state,
        "ws_conf".into(),
        "curl".into(),
        "curl https://x.test/".into(),
        Replace,
    )
    .unwrap();
    assert_eq!(other.replaced, 0);
    assert_eq!(import_names(&state, "ws_conf").len(), 3);
}

#[test]
fn an_environment_name_clash_follows_the_conflict_choice() {
    use crate::app::ImportConflict::{KeepBoth, Replace, Skip};
    let state = crate::app::AppState::open_in_memory().unwrap();
    crate::app::workspace_create_impl(&state, "ws_envc".into(), "Env".into()).unwrap();
    let import = |payload: &str, on_conflict| {
        crate::app::environment_import_text_impl(
            &state,
            "ws_envc".into(),
            payload.into(),
            on_conflict,
        )
        .unwrap()
    };
    let original = r#"{"name":"Local","values":[
        {"key":"base","value":"https://one.test"},
        {"key":"token","value":"original-secret-value","type":"secret"},
        {"key":"gone","value":"x"}]}"#;
    let first = import(original, KeepBoth);

    // Skip answers with the existing environment and writes nothing.
    let skipped = import(r#"{"name":"local","values":[]}"#, Skip);
    assert_eq!(skipped.id, first.id);
    assert_eq!(state.store().list_environments("ws_envc").unwrap().len(), 1);

    // Replace: same id and name, the file's variables, the blank secret keeps its stored value.
    let replaced = import(
        r#"{"name":"LOCAL","_spanbay":{"confirm_before_send":true},"values":[
            {"key":"base","value":"https://two.test"},
            {"key":"token","value":"","type":"secret"},
            {"key":"extra","value":"y"}]}"#,
        Replace,
    );
    assert_eq!(
        (replaced.id.as_str(), replaced.name.as_str()),
        (first.id.as_str(), "Local")
    );
    assert!(replaced.confirm_before_send);
    let keys: Vec<_> = replaced
        .variables
        .iter()
        .map(|variable| variable.key.as_str())
        .collect();
    assert_eq!(keys, ["base", "token", "extra"]);
    let store = state.store();
    let stored = store.list_environments("ws_envc").unwrap();
    assert_eq!(stored.len(), 1);
    let value = |key: &str| {
        stored[0]
            .variables
            .iter()
            .find(|variable| variable.key == key)
            .and_then(|variable| variable.value.clone())
    };
    assert_eq!(value("base").as_deref(), Some("https://two.test"));
    assert_eq!(value("token").as_deref(), Some("original-secret-value"));
    drop(store);

    // Keep both numbers the new one.
    let both = import(original, KeepBoth);
    assert_eq!(both.name, "Local (2)");
    // Replace without a clash is a plain import.
    let fresh = import(r#"{"name":"Fresh","values":[]}"#, Replace);
    assert_eq!(fresh.name, "Fresh");
}

#[test]
fn request_curl_import_keeps_a_chrome_ansi_body_whole_and_writes_nothing() {
    use crate::app::request_curl_import_impl;

    let text = "curl 'https://billing.example.dev/invoices?status=open' \\\n  -H 'content-type: application/json' \\\n  --data-raw $'{\"note\":\"it\\'s\\nok\",\"amount\":12900}'";
    let draft = request_curl_import_impl("ws_1".into(), text.into()).unwrap();
    assert_eq!(draft.method, HttpMethod::Post);
    assert_eq!(draft.collection_id, None);
    assert_eq!(draft.folder_id, None);
    assert_eq!(draft.query_params.len(), 1);
    let content = match &draft.body {
        RequestBody::Json { content } => content.clone(),
        other => panic!("expected a JSON body, got {other:?}"),
    };
    assert_eq!(content, "{\"note\":\"it's\nok\",\"amount\":12900}");

    let error = request_curl_import_impl("ws_1".into(), "curl -H 'x: 1'".into()).unwrap_err();
    assert!(
        matches!(error, crate::error::AppError::Validation { .. }),
        "{error:?}"
    );
}
