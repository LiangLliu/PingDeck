//! Requests pane: the workspace tree, single reads, deletes with a 60 s undo (secrets of a deleted
//! collection included), the dynamic variable names.

use std::time::{Duration, Instant};

use crate::app::tree_service::{DeletedKind, delete_at, restore_at};
use crate::app::{
    AppState, UndoToken, collection_delete_impl, collection_get_impl, commit_delete_undo_impl,
    folder_delete_impl, folder_get_impl, list_dynamic_variable_names_impl, request_delete_impl,
    request_get_impl, restore_deleted_impl, workspace_create_impl, workspace_tree_impl,
};
use crate::error::AppError;
use crate::workspace::{
    Collection, EnvironmentVariable, Folder, HttpMethod, ProtocolKind, Request, VariableScopes,
    WorkspaceId, dynamic_variable_names, resolve_template,
};

const SECRET: &str = "col-s3cr3t-value";

fn state() -> AppState {
    let state = AppState::open_in_memory().unwrap();
    workspace_create_impl(&state, "ws1".into(), "One".into()).unwrap();
    workspace_create_impl(&state, "ws2".into(), "Two".into()).unwrap();
    state
}

fn request(id: &str, folder: Option<&str>, url: &str) -> Request {
    let mut request = Request::new(id, "ws1", format!("Name {id}"), url)
        .unwrap()
        .with_collection("col_a")
        .unwrap();
    if let Some(folder) = folder {
        request = request.with_folder(folder).unwrap();
    }
    request
}

/// `col_a` (plain + secret variable) with `fld_root > fld_child` and `fld_other`, and requests in
/// each folder, one at the collection root and one loose.
fn seed(state: &AppState) {
    let store = state.store();
    let mut collection = Collection::new("col_a", "ws1", "A").unwrap();
    collection.variables = vec![
        EnvironmentVariable::plain("host", "example.test").unwrap(),
        EnvironmentVariable::masked("token", SECRET).unwrap(),
    ];
    store.save_collection(&collection).unwrap();
    store
        .save_folder(&Folder::new("fld_root", "col_a", "Root").unwrap())
        .unwrap();
    store
        .save_folder(
            &Folder::new("fld_child", "col_a", "Child")
                .unwrap()
                .with_parent("fld_root")
                .unwrap(),
        )
        .unwrap();
    store
        .save_folder(&Folder::new("fld_other", "col_a", "Other").unwrap())
        .unwrap();
    for (id, folder) in [
        ("req_in_root", Some("fld_root")),
        ("req_in_child", Some("fld_child")),
        ("req_in_other", Some("fld_other")),
        ("req_at_root", None),
    ] {
        store
            .save_request(&request(id, folder, "https://x.test/a"))
            .unwrap();
    }
    let loose = Request::new("req_loose", "ws1", "Loose", "https://x.test/l").unwrap();
    store.save_request(&loose).unwrap();
}

fn secret_rows(state: &AppState) -> i64 {
    state
        .store()
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM secrets WHERE secret_ref LIKE 'collection.%'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn folder_of(state: &AppState, id: &str) -> Option<String> {
    request_get_impl(state, "ws1".into(), id.into())
        .unwrap()
        .folder_id
        .map(|folder| folder.as_str().to_owned())
}

fn ws1() -> WorkspaceId {
    WorkspaceId::new("ws1").unwrap()
}

#[test]
fn the_tree_is_one_summary_call_with_small_rows_and_a_cut_url() {
    let state = state();
    seed(&state);
    let mut long = request("req_long", None, "https://x.test/");
    long.url = "€".repeat(400);
    long.method = HttpMethod::Post;
    long.protocol = ProtocolKind::Graphql;
    long.body = crate::workspace::RequestBody::RawText {
        language: None,
        content: "b".repeat(100_000),
    };
    state.store().save_request(&long).unwrap();

    let tree = workspace_tree_impl(&state, "ws1".into()).unwrap();
    assert_eq!(tree.collections.len(), 1);
    assert_eq!(tree.folders.len(), 3);
    assert_eq!(tree.requests.len(), 6);
    let summary = tree.requests.iter().find(|r| r.id == "req_long").unwrap();
    assert_eq!(summary.url.chars().count(), 256);
    assert_eq!(summary.method, HttpMethod::Post);
    assert_eq!(summary.protocol, ProtocolKind::Graphql);
    assert_eq!(summary.collection_id.as_deref(), Some("col_a"));
    let loose = tree.requests.iter().find(|r| r.id == "req_loose").unwrap();
    assert_eq!(loose.collection_id, None);
    let child = tree.folders.iter().find(|f| f.id == "fld_child").unwrap();
    assert_eq!(child.parent_folder_id.as_deref(), Some("fld_root"));
    // Every row stays under 512 bytes even though the request carries a 100 kB body and a 400
    // character URL (the 256 characters of `€` are 768 bytes of UTF-8: the cut is by characters).
    for row in tree.requests.iter().filter(|r| r.id != "req_long") {
        assert!(serde_json::to_string(row).unwrap().len() <= 512);
    }
    assert!(serde_json::to_string(summary).unwrap().len() <= 1024);

    // Another workspace sees none of it.
    let other = workspace_tree_impl(&state, "ws2".into()).unwrap();
    assert!(other.collections.is_empty() && other.folders.is_empty() && other.requests.is_empty());
    let missing = workspace_tree_impl(&state, "ws_missing".into()).unwrap_err();
    assert!(matches!(missing, AppError::NotFound { .. }), "{missing}");
}

#[test]
fn single_reads_are_workspace_scoped_and_collections_come_back_redacted() {
    let state = state();
    seed(&state);
    assert_eq!(
        request_get_impl(&state, "ws1".into(), "req_in_root".into())
            .unwrap()
            .name,
        "Name req_in_root"
    );
    assert_eq!(
        folder_get_impl(&state, "ws1".into(), "fld_child".into())
            .unwrap()
            .parent_folder_id
            .unwrap()
            .as_str(),
        "fld_root"
    );
    let collection = collection_get_impl(&state, "ws1".into(), "col_a".into()).unwrap();
    let token = collection
        .variables
        .iter()
        .find(|v| v.key == "token")
        .unwrap();
    assert!(token.is_secret && token.has_secret && token.value.is_none());
    assert!(!serde_json::to_string(&collection).unwrap().contains(SECRET));
    let host = collection
        .variables
        .iter()
        .find(|v| v.key == "host")
        .unwrap();
    assert_eq!(host.value.as_deref(), Some("example.test"));

    for error in [
        request_get_impl(&state, "ws2".into(), "req_in_root".into()).unwrap_err(),
        request_get_impl(&state, "ws1".into(), "req_none".into()).unwrap_err(),
        collection_get_impl(&state, "ws2".into(), "col_a".into()).unwrap_err(),
        folder_get_impl(&state, "ws2".into(), "fld_root".into()).unwrap_err(),
        folder_get_impl(&state, "ws1".into(), "fld_none".into()).unwrap_err(),
    ] {
        assert!(matches!(error, AppError::NotFound { .. }), "{error}");
    }
}

#[test]
fn a_deleted_request_comes_back_and_the_token_is_spent() {
    let state = state();
    seed(&state);
    let token = request_delete_impl(&state, "ws1".into(), "req_at_root".into()).unwrap();
    assert!(request_get_impl(&state, "ws1".into(), "req_at_root".into()).is_err());

    // A foreign workspace cannot use the token.
    let foreign = restore_deleted_impl(&state, "ws2".into(), token.clone()).unwrap_err();
    assert!(matches!(foreign, AppError::NotFound { .. }), "{foreign}");

    let report = restore_deleted_impl(&state, "ws1".into(), token.clone()).unwrap();
    assert_eq!(report.restored, vec!["req_at_root".to_owned()]);
    assert!(report.failed.is_empty());
    assert!(request_get_impl(&state, "ws1".into(), "req_at_root".into()).is_ok());
    let again = restore_deleted_impl(&state, "ws1".into(), token).unwrap_err();
    assert!(matches!(again, AppError::NotFound { .. }), "{again}");

    // Foreign and unknown ids are NotFound and delete nothing.
    for (workspace, id) in [("ws2", "req_at_root"), ("ws1", "req_none")] {
        let error = request_delete_impl(&state, workspace.into(), id.into()).unwrap_err();
        assert!(matches!(error, AppError::NotFound { .. }), "{error}");
    }
    assert!(request_get_impl(&state, "ws1".into(), "req_at_root".into()).is_ok());
}

#[test]
fn a_deleted_folder_unlinks_its_requests_and_restores_subfolders_and_links() {
    let state = state();
    seed(&state);
    let token = folder_delete_impl(&state, "ws1".into(), "fld_root".into()).unwrap();

    // The subfolder went with it; the requests stayed, unlinked, at the collection root.
    assert!(folder_get_impl(&state, "ws1".into(), "fld_root".into()).is_err());
    assert!(folder_get_impl(&state, "ws1".into(), "fld_child".into()).is_err());
    assert!(folder_get_impl(&state, "ws1".into(), "fld_other".into()).is_ok());
    assert_eq!(folder_of(&state, "req_in_root"), None);
    assert_eq!(folder_of(&state, "req_in_child"), None);
    assert_eq!(
        folder_of(&state, "req_in_other").as_deref(),
        Some("fld_other")
    );

    // `req_in_child` was moved into another folder meanwhile: the restore leaves it there.
    let mut moved = request_get_impl(&state, "ws1".into(), "req_in_child".into()).unwrap();
    moved.folder_id = Some(crate::workspace::FolderId::new("fld_other").unwrap());
    state.store().save_request(&moved).unwrap();

    let report = restore_deleted_impl(&state, "ws1".into(), token).unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(
        report.restored,
        ["fld_root", "fld_child", "req_in_root"].map(str::to_owned)
    );
    let child = folder_get_impl(&state, "ws1".into(), "fld_child".into()).unwrap();
    assert_eq!(child.parent_folder_id.unwrap().as_str(), "fld_root");
    assert_eq!(
        folder_of(&state, "req_in_root").as_deref(),
        Some("fld_root")
    );
    assert_eq!(
        folder_of(&state, "req_in_child").as_deref(),
        Some("fld_other")
    );
}

#[test]
fn a_deleted_collection_restores_with_folders_requests_and_secret_values() {
    let state = state();
    seed(&state);
    assert_eq!(secret_rows(&state), 1);
    let token = collection_delete_impl(&state, "ws1".into(), "col_a".into()).unwrap();

    // Everything below went, the secret left the store, only the loose request is left.
    let tree = workspace_tree_impl(&state, "ws1".into()).unwrap();
    assert!(tree.collections.is_empty() && tree.folders.is_empty());
    assert_eq!(tree.requests.len(), 1);
    assert_eq!(
        secret_rows(&state),
        0,
        "no secret outlives the delete in the store"
    );

    let report = restore_deleted_impl(&state, "ws1".into(), token).unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(report.restored[0], "col_a");
    assert_eq!(report.restored.len(), 1 + 3 + 4);
    let tree = workspace_tree_impl(&state, "ws1".into()).unwrap();
    assert_eq!(
        (
            tree.collections.len(),
            tree.folders.len(),
            tree.requests.len()
        ),
        (1, 3, 5)
    );
    assert_eq!(
        folder_of(&state, "req_in_child").as_deref(),
        Some("fld_child")
    );
    assert_eq!(secret_rows(&state), 1);
    let stored = state
        .store()
        .get_collection(
            &ws1(),
            &crate::workspace::CollectionId::new("col_a").unwrap(),
        )
        .unwrap()
        .unwrap();
    let secret = stored.variables.iter().find(|v| v.key == "token").unwrap();
    assert_eq!(secret.value.as_deref(), Some(SECRET));
}

#[test]
fn a_restore_that_restores_nothing_reports_why_and_keeps_the_token() {
    let state = state();
    seed(&state);
    let token = request_delete_impl(&state, "ws1".into(), "req_at_root".into()).unwrap();
    // The id was taken meanwhile.
    state
        .store()
        .save_request(&request("req_at_root", None, "https://x.test/new"))
        .unwrap();
    let report = restore_deleted_impl(&state, "ws1".into(), token.clone()).unwrap();
    assert!(report.restored.is_empty());
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].id, "req_at_root");
    assert_eq!(report.failed[0].kind, DeletedKind::Request);

    // The clash is gone: the same token still works.
    request_delete_impl(&state, "ws1".into(), "req_at_root".into()).unwrap();
    let retry = restore_deleted_impl(&state, "ws1".into(), token).unwrap();
    assert_eq!(retry.restored, vec!["req_at_root".to_owned()]);

    // A collection whose id came back is refused as a whole.
    let token = collection_delete_impl(&state, "ws1".into(), "col_a".into()).unwrap();
    state
        .store()
        .save_collection(&Collection::new("col_a", "ws1", "Again").unwrap())
        .unwrap();
    let report = restore_deleted_impl(&state, "ws1".into(), token).unwrap();
    assert!(report.restored.is_empty());
    assert_eq!(report.failed[0].kind, DeletedKind::Collection);
}

#[test]
fn the_hold_lasts_sixty_seconds_and_a_dismissed_toast_leaves_one() {
    let state = state();
    seed(&state);
    let t0 = Instant::now();
    let secs = Duration::from_secs;

    // Expired: restore is NotFound, and the held secret is gone from memory.
    let expired = delete_at(&state, "ws1", DeletedKind::Collection, "col_a", t0).unwrap();
    let late = restore_at(&state, "ws1", &expired, t0 + secs(61)).unwrap_err();
    assert!(matches!(late, AppError::NotFound { .. }), "{late}");
    assert_eq!(state.delete_undo.len(), 0);

    // The expired collection is gone for good: seed it afresh. Still held at 59 s.
    seed(&state);
    let held = delete_at(&state, "ws1", DeletedKind::Collection, "col_a", t0).unwrap();
    assert!(
        restore_at(&state, "ws1", &held, t0 + secs(59))
            .unwrap()
            .failed
            .is_empty()
    );

    // Dismissed at 10 s: gone 1 s later, held until then; a second dismissal reports it.
    let first = delete_at(&state, "ws1", DeletedKind::Folder, "fld_other", t0).unwrap();
    assert!(state.delete_undo.commit_at(t0 + secs(10), &first.0));
    assert_eq!(state.delete_undo.len(), 1);
    state.delete_undo.purge_at(t0 + secs(11));
    assert_eq!(
        state.delete_undo.len(),
        0,
        "purged one second after the dismissal"
    );
    let gone = restore_at(&state, "ws1", &first, t0 + secs(11)).unwrap_err();
    assert!(matches!(gone, AppError::NotFound { .. }), "{gone}");
    assert!(!state.delete_undo.commit_at(t0 + secs(12), &first.0));
}

#[test]
fn the_public_commit_ends_the_hold_and_an_unknown_token_is_not_committed() {
    let state = state();
    seed(&state);
    let token = request_delete_impl(&state, "ws1".into(), "req_at_root".into()).unwrap();
    assert!(commit_delete_undo_impl(&state, token.clone()));
    // Committed, but its one second has not passed (the real clock moved by microseconds).
    assert_eq!(
        restore_deleted_impl(&state, "ws1".into(), token)
            .unwrap()
            .restored,
        vec!["req_at_root".to_owned()]
    );
    assert!(!commit_delete_undo_impl(
        &state,
        UndoToken("undo_unknown".into())
    ));
}

#[test]
fn the_timer_drops_an_expired_hold_without_anyone_asking() {
    let state = state();
    seed(&state);
    // Held since 61 s ago: already past its limit when the timer starts.
    let long_ago = Instant::now() - Duration::from_secs(61);
    let token = delete_at(&state, "ws1", DeletedKind::Request, "req_at_root", long_ago).unwrap();
    assert_eq!(state.delete_undo.len(), 1);
    state.delete_undo.arm_timer();
    let deadline = Instant::now() + Duration::from_secs(10);
    while state.delete_undo.len() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(state.delete_undo.len(), 0);
    assert!(restore_deleted_impl(&state, "ws1".into(), token).is_err());
}

#[test]
fn dynamic_variable_names_are_the_resolvers_own_table() {
    let names = list_dynamic_variable_names_impl();
    assert_eq!(names, dynamic_variable_names());
    assert!(names.iter().any(|name| name == "$guid"));
    let scopes = VariableScopes::new();
    for name in &names {
        assert!(
            resolve_template(&format!("{{{{{name}}}}}"), &scopes).is_ok(),
            "{name} does not resolve"
        );
    }
}
