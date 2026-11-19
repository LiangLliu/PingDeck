//! Storage tests for workspaces, collections, folders, and requests.

use crate::storage::{PlacementFault, StorageError};
use pingdeck_model::models::Assertion;
use pingdeck_model::{
    ApiKeyLocation, AuthConfig, Collection, CollectionId, Environment, EnvironmentVariable,
    ExecutionHistoryEntry, Folder, FolderId, HttpMethod, KeyValue, ProtocolKind, Request,
    RequestBody, RequestId, Workspace, WorkspaceId,
};

use super::super::WorkspaceStore;

#[test]
fn hierarchy_entities_round_trip_list_touch_and_cascade() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    assert!(Workspace::new("ws_invalid", "   ").is_err());
    assert_eq!(store.workspace_count().unwrap(), 0);

    let billing = Workspace::new("ws_billing", " Billing APIs ").unwrap();
    let identity = Workspace::new("ws_identity", "Identity APIs").unwrap();
    store.save_workspace(&billing).unwrap();
    store.save_workspace(&identity).unwrap();
    assert_eq!(
        store.get_workspace(&billing.id).unwrap(),
        Some(billing.clone())
    );

    assert!(store.touch_workspace(&identity.id).unwrap());
    assert!(
        !store
            .touch_workspace(&WorkspaceId::new("ws_missing").unwrap())
            .unwrap()
    );
    assert_eq!(
        store
            .list_workspaces()
            .unwrap()
            .into_iter()
            .map(|w| w.id)
            .collect::<Vec<_>>(),
        vec![identity.id.clone(), billing.id.clone()]
    );

    let mut collection =
        Collection::new("col_billing", billing.id.as_str(), "Billing Collection").unwrap();
    collection.description = "Reusable billing calls".into();
    collection.auth = AuthConfig::Basic {
        username: "user".into(),
        password_ref: "{{password}}".into(),
    };
    collection.variables = vec![
        EnvironmentVariable::plain("baseUrl", "https://api.example.com").unwrap(),
        EnvironmentVariable::masked("token", "local-secret").unwrap(),
    ];
    collection.pre_request_script = "pm.test('pre', () => {});".into();
    collection.test_script = "console.log('done');".into();
    collection.position = 2;
    store.save_collection(&collection).unwrap();
    assert_eq!(
        store.get_collection(&billing.id, &collection.id).unwrap(),
        Some(collection.clone())
    );

    let mut parent = Folder::new("fold_parent", collection.id.as_str(), "Billing").unwrap();
    parent.description = "Invoice endpoints".into();
    parent.auth = AuthConfig::ApiKey {
        key: "X-Key".into(),
        value_ref: "{{key}}".into(),
        location: ApiKeyLocation::Header,
    };
    parent.pre_request_script = "console.log('folder');".into();
    parent.test_script = "pm.test('folder', () => {});".into();
    parent.position = 3;
    let child = Folder::new("fold_child", collection.id.as_str(), "Invoices")
        .unwrap()
        .with_parent(parent.id.as_str())
        .unwrap();
    store.save_folder(&parent).unwrap();
    store.save_folder(&child).unwrap();
    assert_eq!(
        store.get_folder(&billing.id, &parent.id).unwrap(),
        Some(parent.clone())
    );
    assert_eq!(
        store.get_folder(&billing.id, &child.id).unwrap(),
        Some(child.clone())
    );

    let mut request = Request::new(
        "req_list_invoices",
        billing.id.as_str(),
        "List invoices",
        "{{baseUrl}}/v1/invoices",
    )
    .unwrap()
    .with_collection(collection.id.as_str())
    .unwrap()
    .with_folder(child.id.as_str())
    .unwrap();
    request.protocol = ProtocolKind::Graphql;
    request.method = HttpMethod::Post;
    request.query_params.push(KeyValue {
        key: "customer_id".into(),
        value: "{{customerId}}".into(),
        enabled: true,
        description: None,
    });
    request.headers.push(KeyValue {
        key: "Authorization".into(),
        value: "Bearer {{token}}".into(),
        enabled: true,
        description: None,
    });
    request.auth = AuthConfig::Bearer {
        token_ref: "secret://env/token".into(),
    };
    request.body = RequestBody::Graphql {
        query: "query AccountSummary { account { id } }".into(),
        variables_json: Some("{\"customerId\":\"{{customerId}}\"}".into()),
        operation_name: Some("AccountSummary".into()),
    };
    request.assertions = vec![
        Assertion::StatusEquals { value: 200 },
        Assertion::StatusIn {
            values: vec![200, 201],
        },
        Assertion::HeaderExists {
            key: "Content-Type".into(),
        },
        Assertion::JsonPathEquals {
            path: "data.items[0].id".into(),
            value: serde_json::json!("inv_1"),
        },
        Assertion::ResponseTimeLessThanMs { value: 800 },
    ];
    request.network_options.retry_count = 1;
    request.network_options.send_cookies = false;
    store.save_request(&request).unwrap();
    assert_eq!(
        store.get_request(&billing.id, &request.id).unwrap(),
        Some(request.clone())
    );
    assert_eq!(
        store.list_collections(billing.id.as_str()).unwrap(),
        vec![collection.clone()]
    );
    assert_eq!(
        store
            .list_folders(&billing.id, &collection.id)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        store.list_requests(billing.id.as_str()).unwrap(),
        vec![request.clone()]
    );

    let mut bad_req = request.clone();
    bad_req.assertions = vec![Assertion::HeaderEquals {
        key: "  ".into(),
        value: "application/json".into(),
    }];
    assert!(store.save_request(&bad_req).is_err());

    let env = Environment::new("env_staging", billing.id.as_str(), "Staging").unwrap();
    store.save_environment(&env).unwrap();
    assert_eq!(
        store.list_environments(billing.id.as_str()).unwrap(),
        vec![env.clone()]
    );
    assert!(
        store
            .list_environments(identity.id.as_str())
            .unwrap()
            .is_empty()
    );

    let history = ExecutionHistoryEntry::from_success(
        "hist_list",
        &request,
        Some(env.id.as_str()),
        200,
        "OK",
        "https://api.example.com/v1",
        42,
        "{\"ok\":true}",
        11,
        1,
        "1700000000000",
    )
    .unwrap();
    store.save_execution_history(&history).unwrap();

    // Deleting folder unfiles child request; deleting workspace cascades everything.
    assert!(store.delete_folder(&billing.id, &child.id).unwrap());
    assert_eq!(
        store
            .get_request(&billing.id, &request.id)
            .unwrap()
            .unwrap()
            .folder_id,
        None
    );
    assert!(store.delete_workspace(&billing.id).unwrap());
    assert!(!store.delete_workspace(&billing.id).unwrap());
    assert_eq!(store.get_workspace(&billing.id).unwrap(), None);
    assert!(
        store
            .list_collections(billing.id.as_str())
            .unwrap()
            .is_empty()
    );
    assert!(store.list_requests(billing.id.as_str()).unwrap().is_empty());
    assert!(
        store
            .list_environments(billing.id.as_str())
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .list_execution_history(billing.id.as_str(), 20)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn imported_collection_saves_nested_folders_and_rolls_back_on_error_or_cycle() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let workspace = Workspace::new("ws_import", "Import").unwrap();
    let collection = Collection::new("col_import", workspace.id.as_str(), "Import").unwrap();
    let parent = Folder::new("fold_parent", collection.id.as_str(), "Parent").unwrap();
    let child = Folder::new("fold_child", collection.id.as_str(), "Child")
        .unwrap()
        .with_parent(parent.id.as_str())
        .unwrap();
    let request = Request::new(
        "req_import",
        workspace.id.as_str(),
        "Request",
        "https://example.com",
    )
    .unwrap()
    .with_collection(collection.id.as_str())
    .unwrap()
    .with_folder(child.id.as_str())
    .unwrap();
    store.save_workspace(&workspace).unwrap();

    store
        .save_imported_collection(
            &collection,
            &[child.clone(), parent.clone()],
            std::slice::from_ref(&request),
        )
        .unwrap();
    assert_eq!(
        store.get_collection(&workspace.id, &collection.id).unwrap(),
        Some(collection.clone())
    );
    assert_eq!(
        store.get_folder(&workspace.id, &child.id).unwrap(),
        Some(child)
    );
    assert_eq!(
        store.get_request(&workspace.id, &request.id).unwrap(),
        Some(request.clone())
    );

    // Folder cycle rolls back.
    let col_cycle = Collection::new("col_cycle", workspace.id.as_str(), "Cycle").unwrap();
    let f1 = Folder::new("f1", col_cycle.id.as_str(), "F1")
        .unwrap()
        .with_parent("f2")
        .unwrap();
    let f2 = Folder::new("f2", col_cycle.id.as_str(), "F2")
        .unwrap()
        .with_parent("f1")
        .unwrap();
    assert!(matches!(
        store.save_imported_collection(&col_cycle, &[f1, f2], &[]),
        Err(StorageError::InvalidFolderMove { .. })
    ));
    assert_eq!(
        store.get_collection(&workspace.id, &col_cycle.id).unwrap(),
        None
    );

    // Database failure on request insert rolls back collection update.
    let mut updated = collection.clone();
    updated.name = "Changed".into();
    let mut failing_req = request;
    failing_req.id = RequestId::new("req_fail").unwrap();
    let new_folder = Folder::new("fold_new", collection.id.as_str(), "New").unwrap();
    failing_req.folder_id = Some(new_folder.id.clone());
    let mut first_request = failing_req.clone();
    first_request.id = RequestId::new("req_first").unwrap();
    store
        .connection
        .execute_batch(
            "CREATE TRIGGER fail_import_request BEFORE INSERT ON requests
             WHEN NEW.id = 'req_fail' BEGIN SELECT RAISE(ABORT, 'import failure'); END;",
        )
        .unwrap();
    assert!(matches!(
        store.save_imported_collection(
            &updated,
            std::slice::from_ref(&new_folder),
            &[first_request.clone(), failing_req]
        ),
        Err(StorageError::Sqlite(_))
    ));
    assert_eq!(
        store.get_collection(&workspace.id, &collection.id).unwrap(),
        Some(collection)
    );
    assert_eq!(
        store.get_folder(&workspace.id, &new_folder.id).unwrap(),
        None
    );
    assert_eq!(
        store.get_request(&workspace.id, &first_request.id).unwrap(),
        None
    );
}

#[test]
fn moves_placement_validation_and_workspace_isolation() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let owner = Workspace::new("ws_owner", "Owner").unwrap();
    let other = Workspace::new("ws_other", "Other").unwrap();
    store.save_workspace(&owner).unwrap();
    store.save_workspace(&other).unwrap();

    let col1 = Collection::new("col_1", "ws_owner", "One").unwrap();
    let col2 = Collection::new("col_2", "ws_owner", "Two").unwrap();
    let foreign_col = Collection::new("col_foreign", "ws_other", "Foreign").unwrap();
    store.save_collection(&col1).unwrap();
    store.save_collection(&col2).unwrap();
    store.save_collection(&foreign_col).unwrap();

    let parent = Folder::new("fold_parent", "col_1", "Parent").unwrap();
    let child = Folder::new("fold_child", "col_1", "Child")
        .unwrap()
        .with_parent(parent.id.as_str())
        .unwrap();
    let foreign_fold = Folder::new("fold_foreign", "col_foreign", "Foreign").unwrap();
    store.save_folder(&parent).unwrap();
    store.save_folder(&child).unwrap();
    store.save_folder(&foreign_fold).unwrap();

    // Folder cycle and invalid collection placement checks.
    for bad_parent in [
        Some(parent.id.clone()),
        Some(child.id.clone()),
        Some(foreign_fold.id.clone()),
        Some(FolderId::new("fold_missing").unwrap()),
    ] {
        let mut invalid = parent.clone();
        invalid.parent_folder_id = bad_parent;
        assert!(matches!(
            store.save_folder(&invalid),
            Err(StorageError::InvalidFolderMove { .. })
        ));
    }
    let mut wrong_col_folder = parent.clone();
    wrong_col_folder.collection_id = col2.id.clone();
    assert!(matches!(
        store.save_folder(&wrong_col_folder),
        Err(StorageError::InvalidFolderMove {
            fault: PlacementFault::Invalid,
            ..
        })
    ));

    // Request placement faults: missing collection, foreign collection, mismatched folder.
    let mut req = Request::new("req_1", "ws_owner", "R1", "https://a.test").unwrap();
    req.collection_id = Some(CollectionId::new("col_gone").unwrap());
    assert!(matches!(
        store.save_request(&req),
        Err(StorageError::InvalidRequestPlacement {
            fault: PlacementFault::NotFound,
            ..
        })
    ));
    req.collection_id = Some(foreign_col.id.clone());
    assert!(matches!(
        store.save_request(&req),
        Err(StorageError::InvalidRequestPlacement {
            fault: PlacementFault::Conflict,
            ..
        })
    ));
    req.collection_id = Some(col2.id.clone());
    req.folder_id = Some(parent.id.clone());
    assert!(matches!(
        store.save_request(&req),
        Err(StorageError::InvalidRequestPlacement { .. })
    ));

    // Request with folder in same workspace (with and without collection).
    req.collection_id = None;
    store.save_request(&req).unwrap();
    req.collection_id = Some(col1.id.clone());
    req.folder_id = Some(child.id.clone());
    store.save_request(&req).unwrap();

    // Move request updates parent/position and saturates at i64::MAX.
    let moved_req = store
        .move_request(
            &owner.id,
            &req.id,
            Some(&col1.id),
            Some(&parent.id),
            Some(i64::MAX),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        (moved_req.folder_id, moved_req.position),
        (Some(parent.id.clone()), i64::MAX)
    );
    let loose = Request::new("req_loose", "ws_owner", "Loose", "https://a.test/loose").unwrap();
    store.save_request(&loose).unwrap();
    let appended = store
        .move_request(&owner.id, &loose.id, Some(&col1.id), Some(&parent.id), None)
        .unwrap()
        .unwrap();
    assert_eq!(appended.position, i64::MAX);

    // Move folder tree to col2 transactionally.
    store
        .move_request(&owner.id, &req.id, Some(&col1.id), Some(&child.id), Some(1))
        .unwrap();
    let moved_tree = store
        .move_folder_tree(&owner.id, &parent.id, &col2.id, None)
        .unwrap()
        .unwrap();
    assert_eq!(moved_tree.folders.len(), 2);
    assert!(
        moved_tree
            .folders
            .iter()
            .all(|f| f.collection_id == col2.id)
    );
    assert_eq!(moved_tree.requests[0].collection_id, Some(col2.id.clone()));

    // Cross-workspace isolation: reads, saves, moves, and deletes.
    assert!(matches!(
        store.save_collection(&Collection::new("col_1", "ws_other", "Stolen").unwrap()),
        Err(StorageError::ItemWorkspaceMismatch { .. })
    ));
    assert!(matches!(
        store.save_folder(&Folder::new("fold_parent", "col_foreign", "Stolen").unwrap()),
        Err(StorageError::ItemWorkspaceMismatch { .. })
    ));
    assert!(matches!(
        store.save_request(&Request::new("req_1", "ws_other", "Stolen", "https://b.test").unwrap()),
        Err(StorageError::ItemWorkspaceMismatch { .. })
    ));
    assert_eq!(store.get_collection(&other.id, &col1.id).unwrap(), None);
    assert_eq!(store.get_folder(&other.id, &parent.id).unwrap(), None);
    assert_eq!(store.get_request(&other.id, &req.id).unwrap(), None);
    assert!(store.list_folders(&other.id, &col2.id).unwrap().is_empty());
    assert!(!store.delete_request(&other.id, &req.id).unwrap());
    assert!(!store.delete_folder(&other.id, &parent.id).unwrap());
    assert!(!store.delete_collection(&other.id, &col2.id).unwrap());
    assert!(matches!(
        store.move_request(&owner.id, &req.id, Some(&foreign_col.id), None, None),
        Err(StorageError::InvalidRequestPlacement { .. })
    ));
    assert!(matches!(
        store.move_folder_tree(&owner.id, &parent.id, &foreign_col.id, None),
        Err(StorageError::InvalidFolderMove { .. })
    ));
    assert_eq!(
        store
            .move_request(&other.id, &req.id, Some(&col1.id), None, None)
            .unwrap(),
        None
    );
    assert!(
        store
            .move_folder_tree(&other.id, &parent.id, &col1.id, None)
            .unwrap()
            .is_none()
    );
    assert!(store.delete_request(&owner.id, &req.id).unwrap());
    assert!(store.delete_folder(&owner.id, &parent.id).unwrap());
    assert!(store.delete_collection(&owner.id, &col2.id).unwrap());
    assert_eq!(store.get_request(&owner.id, &req.id).unwrap(), None);
    assert_eq!(store.get_folder(&owner.id, &child.id).unwrap(), None);
    assert_eq!(store.get_collection(&owner.id, &col2.id).unwrap(), None);
}

#[test]
fn deleting_a_workspace_keeps_its_chats_with_a_dangling_binding() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let pay = Workspace::new("ws_pay", "Payments platform").unwrap();
    store.save_workspace(&pay).unwrap();
    for (id, deleted_at_ms) in [("s1", None), ("s2", None), ("s_gone", Some(5))] {
        let mut chat = super::agent::session(id, 1);
        chat.pingdeck_workspace_id = Some("ws_pay".to_owned());
        chat.pingdeck_environment_id = Some("env_stg".to_owned());
        chat.deleted_at_ms = deleted_at_ms;
        store.save_agent_session(&chat).unwrap();
    }
    assert_eq!(store.workspace_counts("ws_pay").unwrap().chats, 2);

    assert!(store.delete_workspace(&pay.id).unwrap());
    // The id stays so the UI can tell "workspace deleted" from "never bound".
    let kept = store.get_agent_session("s1").unwrap().expect("chat stays");
    assert_eq!(kept.pingdeck_workspace_id.as_deref(), Some("ws_pay"));
    assert_eq!(kept.pingdeck_workspace_name, None);
}

#[test]
fn the_agent_marker_reads_back_on_the_request_and_its_tree_summary_and_survives_a_save() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    store
        .save_workspace(&Workspace::new("ws_ag", "Agent").unwrap())
        .unwrap();
    let request = Request::new("req_ag", "ws_ag", "GET /x", "https://api.test/x").unwrap();
    store.save_request(&request).unwrap();
    let workspace = WorkspaceId::new("ws_ag").unwrap();
    let id = RequestId::new("req_ag").unwrap();
    let read = |store: &WorkspaceStore| {
        (
            store
                .get_request(&workspace, &id)
                .unwrap()
                .unwrap()
                .agent_touched_ms,
            store.workspace_tree(&workspace).unwrap().requests[0].agent_touched_ms,
        )
    };
    assert_eq!(read(&store), (None, None));
    assert!(
        store
            .mark_request_agent_touched("ws_ag", "req_ag", 42)
            .unwrap()
    );
    // A save from the editor does not clear it; only a send from the UI does.
    store.save_request(&request).unwrap();
    assert_eq!(read(&store), (Some(42), Some(42)));
    assert!(
        store
            .clear_request_agent_touched("ws_ag", "req_ag")
            .unwrap()
    );
    assert_eq!(read(&store), (None, None));
}

#[test]
fn moving_a_folder_tree_under_a_target_parent_checks_the_parent_then_reparents_the_subtree() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let ws = Workspace::new("ws_nest", "Nest").unwrap();
    store.save_workspace(&ws).unwrap();
    for (id, name) in [
        ("col_src", "Source"),
        ("col_dst", "Target"),
        ("col_side", "Side"),
    ] {
        store
            .save_collection(&Collection::new(id, ws.id.as_str(), name).unwrap())
            .unwrap();
    }
    let moved = Folder::new("fold_moved", "col_src", "Moved").unwrap();
    let child = Folder::new("fold_moved_child", "col_src", "Child")
        .unwrap()
        .with_parent("fold_moved")
        .unwrap();
    let target = Folder::new("fold_target", "col_dst", "Target").unwrap();
    let side = Folder::new("fold_side", "col_side", "Side").unwrap();
    for folder in [&moved, &child, &target, &side] {
        store.save_folder(folder).unwrap();
    }
    for (id, folder) in [
        ("req_in_child", "fold_moved_child"),
        ("req_in_moved", "fold_moved"),
    ] {
        let request = Request::new(id, ws.id.as_str(), id, "https://a.test")
            .unwrap()
            .with_collection("col_src")
            .unwrap()
            .with_folder(folder)
            .unwrap();
        store.save_request(&request).unwrap();
    }
    let destination = CollectionId::new("col_dst").unwrap();
    let state = |store: &WorkspaceStore| {
        (
            store
                .list_folders(&ws.id, &CollectionId::new("col_src").unwrap())
                .unwrap(),
            store.list_folders(&ws.id, &destination).unwrap(),
            store.list_requests(ws.id.as_str()).unwrap(),
        )
    };
    let before = state(&store);

    for (parent, expected) in [
        ("fold_moved", PlacementFault::Invalid),
        ("fold_moved_child", PlacementFault::Invalid),
        ("fold_side", PlacementFault::Conflict),
        ("fold_missing", PlacementFault::NotFound),
    ] {
        let parent = FolderId::new(parent).unwrap();
        assert!(matches!(
            store.move_folder_tree(&ws.id, &moved.id, &destination, Some(&parent)),
            Err(StorageError::InvalidFolderMove { fault, .. }) if fault == expected
        ));
        assert_eq!(state(&store), before);
    }
    assert_eq!(
        store
            .move_folder_tree(
                &ws.id,
                &FolderId::new("fold_gone").unwrap(),
                &destination,
                Some(&target.id)
            )
            .unwrap(),
        None
    );

    let result = store
        .move_folder_tree(&ws.id, &moved.id, &destination, Some(&target.id))
        .unwrap()
        .unwrap();
    let folder_ids: Vec<&str> = result.folders.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(folder_ids, ["fold_moved", "fold_moved_child"]);
    assert_eq!(result.folders[0].parent_folder_id, Some(target.id.clone()));
    assert_eq!(result.folders[1].parent_folder_id, Some(moved.id.clone()));
    assert!(
        result
            .folders
            .iter()
            .all(|f| f.collection_id == destination)
    );
    // Requests follow the moved folder ids in order, then their ids.
    let request_ids: Vec<&str> = result.requests.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(request_ids, ["req_in_moved", "req_in_child"]);
    assert!(
        result
            .requests
            .iter()
            .all(|r| r.collection_id.as_ref() == Some(&destination))
    );
    assert_eq!(
        store.list_folders(&ws.id, &destination).unwrap().len(),
        3,
        "target, moved and its child"
    );
}

#[test]
fn an_invalid_imported_collection_is_rejected_whole() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let ws = Workspace::new("ws_bad_import", "Bad import").unwrap();
    store.save_workspace(&ws).unwrap();
    store
        .save_collection(&Collection::new("col_elsewhere", ws.id.as_str(), "Elsewhere").unwrap())
        .unwrap();
    let collection = Collection::new("col_bad", ws.id.as_str(), "Bad").unwrap();
    let folder = |id: &str| Folder::new(id, "col_bad", id).unwrap();
    let request = |id: &str| {
        Request::new(id, ws.id.as_str(), id, "https://a.test")
            .unwrap()
            .with_collection("col_bad")
            .unwrap()
    };
    let loose_request =
        Request::new("req_loose", ws.id.as_str(), "Loose", "https://a.test").unwrap();
    let foreign_folder_request = request("req_foreign_folder")
        .with_folder("fold_absent")
        .unwrap();

    let folder_cases: [(Vec<Folder>, &str); 3] = [
        (
            vec![Folder::new("fold_other", "col_elsewhere", "Other").unwrap()],
            "imported folder must belong to the imported collection",
        ),
        (
            vec![folder("fold_twice"), folder("fold_twice")],
            "imported collection contains duplicate folder IDs",
        ),
        (
            vec![folder("fold_orphan").with_parent("fold_absent").unwrap()],
            "imported folder parent fold_absent not found",
        ),
    ];
    for (folders, message) in folder_cases {
        let error = store
            .save_imported_collection(&collection, &folders, &[])
            .unwrap_err();
        assert!(
            matches!(
                &error,
                StorageError::InvalidFolderMove { reason, fault: PlacementFault::Invalid }
                    if reason == message
            ),
            "{error:?}"
        );
        assert_eq!(store.get_collection(&ws.id, &collection.id).unwrap(), None);
    }

    let request_cases: [(Vec<Request>, &str); 3] = [
        (
            vec![loose_request],
            "imported request must belong to the imported collection and its folders",
        ),
        (
            vec![foreign_folder_request],
            "imported request must belong to the imported collection and its folders",
        ),
        (
            vec![request("req_twice"), request("req_twice")],
            "imported collection contains duplicate request IDs",
        ),
    ];
    for (requests, message) in request_cases {
        let error = store
            .save_imported_collection(&collection, &[], &requests)
            .unwrap_err();
        assert!(
            matches!(
                &error,
                StorageError::InvalidRequestPlacement { reason, fault: PlacementFault::Invalid }
                    if reason == message
            ),
            "{error:?}"
        );
        assert_eq!(store.get_collection(&ws.id, &collection.id).unwrap(), None);
        assert!(
            store
                .get_request(&ws.id, &requests[0].id)
                .unwrap()
                .is_none()
        );
    }
}
