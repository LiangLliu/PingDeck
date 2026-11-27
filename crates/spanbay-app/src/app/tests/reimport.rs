//! Re-importing a collection in place ("Update", id-keeping "Replace"), copying subtrees across
//! workspaces, and saved examples surviving an editor save.

use crate::app::ImportConflict::{KeepBoth, Replace, Update};
use crate::app::{
    AppState, CopyItemInput, CopyItemKind, postman_import_text_impl, restore_deleted_impl,
    tree_copy_impl,
};
use crate::workspace::models::{Assertion, RequestExample};
use crate::workspace::{Request, WorkspaceId};

const V1: &str = r#"{
  "info": {"name": "Shop"},
  "item": [
    {"name": "Orders", "item": [
      {"name": "List", "request": {"method": "GET", "url": "https://shop.test/orders",
        "header": [{"key": "X-Tenant", "value": ""}]}}
    ]},
    {"name": "Ping", "request": {"method": "GET", "url": "https://shop.test/ping"}}
  ]
}"#;

/// V1 with List renamed, a new request, and Ping gone.
const V2: &str = r#"{
  "info": {"name": "Shop"},
  "item": [
    {"name": "Orders", "item": [
      {"name": "List orders", "request": {"method": "GET", "url": "https://shop.test/orders/",
        "header": [{"key": "X-Tenant", "value": ""}, {"key": "Accept", "value": "application/json"}]}},
      {"name": "Create", "request": {"method": "POST", "url": "https://shop.test/orders"}}
    ]}
  ]
}"#;

fn ws() -> WorkspaceId {
    WorkspaceId::new("ws_re").unwrap()
}

fn requests(state: &AppState) -> Vec<Request> {
    state.store().list_requests("ws_re").unwrap()
}

fn named(state: &AppState, name: &str) -> Request {
    requests(state)
        .into_iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("{name} missing"))
}

fn setup() -> (AppState, String, Request) {
    let state = AppState::open_in_memory().unwrap();
    crate::app::workspace_create_impl(&state, "ws_re".into(), "Re".into()).unwrap();
    let first = postman_import_text_impl(&state, "ws_re".into(), V1.into(), KeepBoth).unwrap();
    // What the user adds after the first import.
    let mut list = named(&state, "List");
    list.assertions = vec![Assertion::StatusEquals { value: 200 }];
    list.network_options.verify_tls = false;
    list.headers[0].value = "acme".into();
    state.store().save_request(&list).unwrap();
    (state, first.collection_id, list)
}

#[test]
fn update_keeps_ids_and_what_the_user_added_and_undo_puts_it_back() {
    let (state, collection_id, list) = setup();
    let result = postman_import_text_impl(&state, "ws_re".into(), V2.into(), Update).unwrap();
    assert_eq!(result.collection_id, collection_id);
    assert_eq!((result.updated, result.request_count), (1, 2));
    assert!(
        result.warnings.iter().any(|w| w.contains("were kept")),
        "{:?}",
        result.warnings
    );

    let after = state
        .store()
        .get_request(&ws(), &list.id)
        .unwrap()
        .expect("paired request keeps its id");
    assert_eq!(after.name, "List orders");
    assert_eq!(after.assertions, list.assertions);
    assert!(!after.network_options.verify_tls);
    // The file's blank value keeps the filled-in one; its new header arrives.
    let headers: Vec<(&str, &str)> = after
        .headers
        .iter()
        .map(|h| (h.key.as_str(), h.value.as_str()))
        .collect();
    assert_eq!(
        headers,
        [("X-Tenant", "acme"), ("Accept", "application/json")]
    );
    // Ping is not in the file: an update keeps it. Create is new.
    assert_eq!(requests(&state).len(), 3);
    named(&state, "Create");

    let token = result.undo_token.expect("an update offers undo");
    let report = restore_deleted_impl(&state, "ws_re".into(), token).unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    let back = state.store().get_request(&ws(), &list.id).unwrap().unwrap();
    assert_eq!((back.name.as_str(), back.headers.len()), ("List", 1));
    let names: Vec<String> = requests(&state).into_iter().map(|r| r.name).collect();
    assert!(!names.contains(&"Create".to_owned()), "{names:?}");
}

#[test]
fn replace_makes_the_collection_the_file_but_keeps_paired_ids() {
    let (state, collection_id, list) = setup();
    let result = postman_import_text_impl(&state, "ws_re".into(), V2.into(), Replace).unwrap();
    assert_eq!(result.collection_id, collection_id);
    let after = state.store().get_request(&ws(), &list.id).unwrap().unwrap();
    // The file is the truth: the user's assertion and filled-in value go.
    assert!(after.assertions.is_empty());
    assert_eq!(after.headers[0].value, "");
    let names: Vec<String> = requests(&state).into_iter().map(|r| r.name).collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(!names.contains(&"Ping".to_owned()));
    assert!(result.warnings.iter().any(|w| w.contains("were removed")));

    restore_deleted_impl(&state, "ws_re".into(), result.undo_token.unwrap()).unwrap();
    named(&state, "Ping");
    assert_eq!(named(&state, "List").assertions, list.assertions);
}

#[test]
fn a_replaced_collection_that_cannot_go_is_a_warning_and_the_undo_still_holds_the_rest() {
    use crate::app::tree_service::{hold_update, snapshot_collection};
    use crate::workspace::CollectionId;
    let (state, collection_id, _) = setup();
    let second = postman_import_text_impl(&state, "ws_re".into(), V1.into(), KeepBoth).unwrap();
    let id = CollectionId::new(&collection_id).unwrap();
    let before = snapshot_collection(&state.store(), &ws(), &id)
        .unwrap()
        .unwrap();
    let mut warnings = Vec::new();
    // A blank id fails to capture; the second copy is still removed and held.
    let (token, replaced) = hold_update(
        &state,
        &ws(),
        before,
        Vec::new(),
        Vec::new(),
        &[String::new(), second.collection_id.clone()],
        &mut warnings,
    );
    assert_eq!(replaced, 1);
    assert!(
        warnings.len() == 1 && warnings[0].contains("could not be replaced"),
        "{warnings:?}"
    );
    assert_eq!(state.store().list_collections("ws_re").unwrap().len(), 1);
    restore_deleted_impl(&state, "ws_re".into(), token).unwrap();
    assert_eq!(state.store().list_collections("ws_re").unwrap().len(), 2);
}

#[test]
fn copies_reach_another_workspace_with_fresh_ids_and_secrets() {
    let state = AppState::open_in_memory().unwrap();
    for (id, name) in [("ws_a", "A"), ("ws_b", "B")] {
        crate::app::workspace_create_impl(&state, id.into(), name.into()).unwrap();
    }
    let payload = r#"{"info":{"name":"Src"},
      "item":[{"name":"F","item":[{"name":"Sub","item":[{"name":"R","request":{"method":"GET","url":"https://x.test"}}]}]}],
      "variable":[{"key":"token","value":"tok-123","type":"secret"}]}"#;
    let source = postman_import_text_impl(&state, "ws_a".into(), payload.into(), KeepBoth).unwrap();
    let copy = |kind, id: &str, collection: Option<&str>, folder: Option<&str>| {
        tree_copy_impl(
            &state,
            CopyItemInput {
                workspace_id: "ws_a".into(),
                kind,
                id: id.into(),
                target_workspace_id: "ws_b".into(),
                target_collection_id: collection.map(str::to_owned),
                target_folder_id: folder.map(str::to_owned),
            },
        )
    };
    let collection = copy(CopyItemKind::Collection, &source.collection_id, None, None).unwrap();
    assert_eq!((collection.folders, collection.requests), (2, 1));
    let store = state.store();
    let copied = store
        .get_collection(
            &WorkspaceId::new("ws_b").unwrap(),
            &crate::workspace::CollectionId::new(&collection.id).unwrap(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(copied.variables[0].value.as_deref(), Some("tok-123"));
    let source_folder = store
        .list_folders(
            &WorkspaceId::new("ws_a").unwrap(),
            &crate::workspace::CollectionId::new(&source.collection_id).unwrap(),
        )
        .unwrap()
        .into_iter()
        .find(|f| f.name == "F")
        .unwrap();
    drop(store);
    // The source keeps its rows; the copy has its own ids.
    assert_ne!(collection.id, source.collection_id);
    assert_eq!(state.store().list_requests("ws_a").unwrap().len(), 1);

    // A folder copy into the copied collection: a second "F" sibling becomes "F (copy)".
    let folder = copy(
        CopyItemKind::Folder,
        source_folder.id.as_str(),
        Some(&collection.id),
        None,
    )
    .unwrap();
    assert_eq!((folder.folders, folder.requests), (2, 1));
    let names: Vec<String> = state
        .store()
        .list_folders(
            &WorkspaceId::new("ws_b").unwrap(),
            &crate::workspace::CollectionId::new(&collection.id).unwrap(),
        )
        .unwrap()
        .into_iter()
        .map(|f| f.name)
        .collect();
    assert!(names.contains(&"F (copy)".to_owned()), "{names:?}");
    // A folder copy needs a target collection; an unknown target workspace is not found.
    assert!(copy(CopyItemKind::Folder, source_folder.id.as_str(), None, None).is_err());
    let lost = tree_copy_impl(
        &state,
        CopyItemInput {
            workspace_id: "ws_a".into(),
            kind: CopyItemKind::Collection,
            id: source.collection_id.clone(),
            target_workspace_id: "ws_none".into(),
            target_collection_id: None,
            target_folder_id: None,
        },
    );
    assert!(matches!(lost, Err(crate::error::AppError::NotFound { .. })));
}

#[test]
fn saved_examples_survive_an_editor_save_and_stay_out_of_history_snapshots() {
    let state = AppState::open_in_memory().unwrap();
    crate::app::workspace_create_impl(&state, "ws_re".into(), "Re".into()).unwrap();
    let mut request = Request::new("req_ex", "ws_re", "Ex", "https://x.test").unwrap();
    request.examples = vec![RequestExample {
        name: "OK".into(),
        status: 200,
        headers: Vec::new(),
        body: "{}".into(),
    }];
    state.store().save_request(&request).unwrap();
    let input: crate::app::SaveRequestInput = serde_json::from_value(serde_json::json!({
        "id": "req_ex", "workspace_id": "ws_re", "collection_id": null, "folder_id": null,
        "name": "Renamed", "protocol": "HttpRest", "method": "Get", "url": "https://x.test",
        "query_params": [], "headers": [], "auth": "None", "body": "None",
        "network_options": {},
    }))
    .unwrap();
    crate::app::request_save_impl(&state, input).unwrap();
    let stored = state
        .store()
        .get_request(&ws(), &request.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        (stored.name.as_str(), stored.examples.len()),
        ("Renamed", 1)
    );
    let entry = crate::workspace::ExecutionHistoryEntry::from_failure(
        "h1",
        &stored,
        None,
        "network",
        "down",
        1,
        1,
        "2026-01-01T00:00:00Z",
    )
    .unwrap()
    .with_request_snapshot(&stored);
    assert!(entry.request_snapshot.unwrap().examples.is_empty());

    // An explicit list replaces them: the Docs tab deletes the last example.
    let mut cleared: crate::app::SaveRequestInput = serde_json::from_value(serde_json::json!({
        "id": "req_ex", "workspace_id": "ws_re", "collection_id": null, "folder_id": null,
        "name": "Renamed", "protocol": "HttpRest", "method": "Get", "url": "https://x.test",
        "query_params": [], "headers": [], "auth": "None", "body": "None",
        "network_options": {}, "examples": [],
    }))
    .unwrap();
    crate::app::request_save_impl(&state, cleared.clone()).unwrap();
    let stored = state
        .store()
        .get_request(&ws(), &request.id)
        .unwrap()
        .unwrap();
    assert!(stored.examples.is_empty());
    // The editor path checks the same limit the store does.
    cleared.examples = Some(vec![request.examples[0].clone(); 21]);
    assert!(crate::app::request_save_impl(&state, cleared).is_err());

    // Over the limit the store refuses.
    request.examples = vec![request.examples[0].clone(); 21];
    assert!(state.store().save_request(&request).is_err());
}
