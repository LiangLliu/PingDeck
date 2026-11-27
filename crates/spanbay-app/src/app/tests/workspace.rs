//! Workspace, global, environment, collection, folder and request use-case tests.

use crate::workspace::{
    AuthConfig, EnvironmentVariable, HttpMethod, KeyValue, NetworkOptions, ProtocolKind,
    RequestBody, WorkspaceId,
};

#[test]
fn workspace_commands_create_and_get_workspace() {
    let state = crate::app::AppState::open_in_memory().expect("state should open");

    let created =
        crate::app::workspace_create_impl(&state, "ws_billing".into(), "Billing APIs".into())
            .unwrap();
    let loaded = state
        .store()
        .get_workspace(&WorkspaceId::new("ws_billing").unwrap())
        .unwrap()
        .expect("workspace should exist");

    assert_eq!(loaded, created);
}

#[test]
fn globals_reject_invalid_variables_without_corrupting_saved_values() {
    let state = crate::app::AppState::open_in_memory().unwrap();
    crate::app::workspace_create_impl(&state, "ws_globals".into(), "Globals".into()).unwrap();
    let to_inputs = |variables: &[EnvironmentVariable]| {
        variables
            .iter()
            .map(|variable| crate::app::EnvironmentVariableInput {
                key: variable.key.clone(),
                value: variable.value.clone(),
                is_secret: variable.is_secret,
                enabled: variable.enabled,
                ..Default::default()
            })
            .collect::<Vec<_>>()
    };
    let valid = vec![EnvironmentVariable::plain("existing", "value").unwrap()];
    crate::app::globals_save_impl(
        &state,
        crate::app::GlobalsSaveInput {
            workspace_id: "ws_globals".into(),
            variables: to_inputs(&valid),
            base_revision: None,
        },
    )
    .unwrap();
    for variable in [
        crate::app::EnvironmentVariableInput {
            key: "missing".into(),
            is_secret: true,
            ..Default::default()
        },
        crate::app::EnvironmentVariableInput {
            key: "plain".into(),
            ..Default::default()
        },
    ] {
        assert!(
            crate::app::globals_save_impl(
                &state,
                crate::app::GlobalsSaveInput {
                    workspace_id: "ws_globals".into(),
                    variables: vec![variable],
                    base_revision: None,
                }
            )
            .is_err()
        );
        // The first save named the variable; compare everything but its identity.
        let without_ids = |variables: Vec<crate::app::EnvironmentVariableDto>| {
            variables
                .into_iter()
                .map(|mut variable| {
                    variable.id.clear();
                    variable
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            without_ids(
                crate::app::globals_get_impl(&state, "ws_globals".into())
                    .unwrap()
                    .variables
            ),
            without_ids(crate::app::environment_service::redact_variables(
                "ws_globals",
                &valid
            ))
        );
    }
    let secrets = vec![
        EnvironmentVariable::masked("local", "secret").unwrap(),
        EnvironmentVariable::masked("other", "secret2").unwrap(),
    ];
    crate::app::globals_save_impl(
        &state,
        crate::app::GlobalsSaveInput {
            workspace_id: "ws_globals".into(),
            variables: to_inputs(&secrets),
            base_revision: None,
        },
    )
    .unwrap();
    let mut stored = state.store().list_global_variables("ws_globals").unwrap();
    assert!(
        stored
            .iter()
            .all(|variable| variable.id.starts_with("var_"))
    );
    stored.iter_mut().for_each(|variable| variable.id.clear());
    assert_eq!(stored, secrets);
    assert!(
        crate::app::globals_get_impl(&state, "ws_globals".into())
            .unwrap()
            .variables
            .iter()
            .all(|variable| variable.value.is_none() && variable.has_secret)
    );
}

#[test]
fn request_commands_save_and_get_graphql_request() {
    let state = crate::app::AppState::open_in_memory().expect("state should open");
    crate::app::workspace_create_impl(&state, "ws_billing".into(), "Billing APIs".into()).unwrap();
    crate::app::collection_save_impl(
        &state,
        crate::app::SaveCollectionInput {
            id: "col_billing".into(),
            workspace_id: "ws_billing".into(),
            name: "Billing Collection".into(),
            ..Default::default()
        },
    )
    .unwrap();
    crate::app::folder_save_impl(
        &state,
        crate::app::SaveFolderInput {
            id: "fold_invoices".into(),
            collection_id: "col_billing".into(),
            name: "Invoices".into(),
            ..Default::default()
        },
    )
    .unwrap();

    let saved = crate::app::request_save_impl(
        &state,
        crate::app::SaveRequestInput {
            id: "req_list_invoices".into(),
            workspace_id: "ws_billing".into(),
            collection_id: Some("col_billing".into()),
            folder_id: Some("fold_invoices".into()),
            name: "List invoices".into(),
            protocol: ProtocolKind::Graphql,
            method: HttpMethod::Post,
            url: "{{baseUrl}}/v1/invoices".into(),
            query_params: vec![KeyValue {
                key: "customer_id".into(),
                value: "{{customerId}}".into(),
                enabled: true,
                description: None,
            }],
            headers: vec![KeyValue {
                key: "Authorization".into(),
                value: "Bearer {{token}}".into(),
                enabled: true,
                description: None,
            }],
            auth: AuthConfig::Bearer {
                token_ref: "secret://env/token".into(),
            },
            body: RequestBody::Graphql {
                query: "query AccountSummary { account { id } }".into(),
                variables_json: Some("{\"customerId\":\"{{customerId}}\"}".into()),
                operation_name: Some("AccountSummary".into()),
            },
            network_options: NetworkOptions {
                timeout_ms: 30_000,
                follow_redirects: true,
                retry_count: 1,
                send_cookies: false,
                verify_tls: true,
                proxy_url: None,
                ..NetworkOptions::default()
            },
            ..Default::default()
        },
    )
    .unwrap();

    let stored = state
        .store()
        .get_request(
            &saved.workspace_id,
            &crate::workspace::RequestId::new("req_list_invoices").unwrap(),
        )
        .unwrap()
        .unwrap();
    // The save answers the stored request with the revision the next save echoes.
    assert_eq!(
        saved.revision.as_deref(),
        Some(crate::app::request_revision(&stored).as_str())
    );
    assert_eq!(
        crate::workspace::Request {
            revision: None,
            ..saved
        },
        stored
    );
}

#[test]
fn list_commands_return_workspace_scoped_data() {
    let state = crate::app::AppState::open_in_memory().expect("state should open");
    let workspace =
        crate::app::workspace_create_impl(&state, "ws_billing".into(), "Billing APIs".into())
            .unwrap();
    let collection = crate::app::collection_save_impl(
        &state,
        crate::app::SaveCollectionInput {
            id: "col_billing".into(),
            workspace_id: workspace.id.as_str().into(),
            name: "Billing Collection".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let request = crate::app::request_save_impl(
        &state,
        crate::app::SaveRequestInput {
            id: "req_list_invoices".into(),
            workspace_id: workspace.id.as_str().into(),
            collection_id: Some(collection.id.as_str().into()),
            name: "List invoices".into(),
            url: "{{baseUrl}}/v1/invoices".into(),
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(
        crate::app::workspace_list_impl(&state).unwrap(),
        vec![workspace]
    );
    assert_eq!(
        crate::app::collection_list_impl(&state, "ws_billing".into()).unwrap(),
        vec![collection]
    );
    assert_eq!(
        crate::app::request_list_impl(&state, "ws_billing".into()).unwrap(),
        vec![request]
    );
}

#[test]
fn delete_commands_remove_entities_and_report_missing_rows() {
    let state = crate::app::AppState::open_in_memory().expect("state should open");
    let workspace =
        crate::app::workspace_create_impl(&state, "ws_billing".into(), "Billing APIs".into())
            .unwrap();
    let collection = crate::app::collection_save_impl(
        &state,
        crate::app::SaveCollectionInput {
            id: "col_billing".into(),
            workspace_id: workspace.id.as_str().into(),
            name: "Billing".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let folder = crate::app::folder_save_impl(
        &state,
        crate::app::SaveFolderInput {
            id: "fold_invoices".into(),
            collection_id: collection.id.as_str().into(),
            name: "Invoices".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let environment = crate::app::environment_save_impl(
        &state,
        crate::app::SaveEnvironmentInput {
            id: "env_staging".into(),
            workspace_id: workspace.id.as_str().into(),
            name: "Staging".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let request = crate::app::request_save_impl(
        &state,
        crate::app::SaveRequestInput {
            id: "req_list".into(),
            workspace_id: workspace.id.as_str().into(),
            collection_id: Some(collection.id.as_str().into()),
            folder_id: Some(folder.id.as_str().into()),
            name: "List".into(),
            url: "{{baseUrl}}/v1".into(),
            ..Default::default()
        },
    )
    .unwrap();

    let ws = || String::from(workspace.id.as_str());
    crate::app::request_delete_impl(&state, ws(), request.id.as_str().into()).unwrap();
    let missing =
        crate::app::request_delete_impl(&state, ws(), request.id.as_str().into()).unwrap_err();
    assert!(matches!(missing, crate::error::AppError::NotFound { .. }));
    crate::app::environment_delete_impl(&state, ws(), environment.id.as_str().into()).unwrap();
    crate::app::folder_delete_impl(&state, ws(), folder.id.as_str().into()).unwrap();
    crate::app::collection_delete_impl(&state, ws(), collection.id.as_str().into()).unwrap();
    crate::app::workspace_delete_impl(&state, workspace.id.as_str().into()).unwrap();
    assert!(matches!(
        crate::app::workspace_delete_impl(&state, workspace.id.as_str().into()).unwrap_err(),
        crate::error::AppError::NotFound { .. }
    ));
}

#[test]
fn by_id_commands_of_another_workspace_are_not_found_and_leave_rows_intact() {
    use crate::error::AppError;
    use crate::workspace::{CollectionId, EnvironmentId, FolderId, RequestId, WorkspaceId};

    let state = crate::app::AppState::open_in_memory().expect("state should open");
    for id in ["ws_owner", "ws_other"] {
        crate::app::workspace_create_impl(&state, id.into(), id.into()).unwrap();
    }
    let owner = || String::from("ws_owner");
    let other = || String::from("ws_other");
    crate::app::collection_save_impl(
        &state,
        crate::app::SaveCollectionInput {
            id: "col_1".into(),
            workspace_id: owner(),
            name: "Owned".into(),
            ..Default::default()
        },
    )
    .unwrap();
    crate::app::folder_save_impl(
        &state,
        crate::app::SaveFolderInput {
            id: "fold_1".into(),
            collection_id: "col_1".into(),
            name: "Folder".into(),
            ..Default::default()
        },
    )
    .unwrap();
    crate::app::environment_save_impl(
        &state,
        crate::app::SaveEnvironmentInput {
            id: "env_1".into(),
            workspace_id: owner(),
            name: "Env".into(),
            ..Default::default()
        },
    )
    .unwrap();
    crate::app::request_save_impl(
        &state,
        crate::app::SaveRequestInput {
            id: "req_1".into(),
            workspace_id: owner(),
            collection_id: Some("col_1".into()),
            folder_id: Some("fold_1".into()),
            name: "Req".into(),
            url: "https://a.test".into(),
            ..Default::default()
        },
    )
    .unwrap();

    let not_found = |error: AppError| matches!(error, AppError::NotFound { .. });
    assert!(not_found(
        crate::app::environment_delete_impl(&state, other(), "env_1".into()).unwrap_err()
    ));
    assert!(not_found(
        crate::app::request_delete_impl(&state, other(), "req_1".into()).unwrap_err()
    ));
    assert!(not_found(
        crate::app::folder_delete_impl(&state, other(), "fold_1".into()).unwrap_err()
    ));
    assert!(not_found(
        crate::app::collection_delete_impl(&state, other(), "col_1".into()).unwrap_err()
    ));
    assert!(not_found(
        crate::app::folder_list_impl(&state, other(), "col_1".into()).unwrap_err()
    ));
    assert!(not_found(
        crate::app::folder_list_impl(&state, owner(), "col_missing".into()).unwrap_err()
    ));

    {
        let store = state.store();
        let ws = WorkspaceId::new("ws_owner").unwrap();
        assert!(
            store
                .get_collection(&ws, &CollectionId::new("col_1").unwrap())
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .get_folder(&ws, &FolderId::new("fold_1").unwrap())
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .get_request(&ws, &RequestId::new("req_1").unwrap())
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .get_environment(&ws, &EnvironmentId::new("env_1").unwrap())
                .unwrap()
                .is_some()
        );
    }
    assert_eq!(
        crate::app::folder_list_impl(&state, owner(), "col_1".into())
            .unwrap()
            .len(),
        1
    );

    // Moves: a source id of another workspace is NotFound; a foreign target is a Conflict.
    crate::app::collection_save_impl(
        &state,
        crate::app::SaveCollectionInput {
            id: "col_other".into(),
            workspace_id: other(),
            name: "Foreign".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let move_request = |workspace_id: String, target: &str| {
        crate::app::request_move_impl(
            &state,
            crate::app::MoveRequestInput {
                workspace_id,
                request_id: "req_1".into(),
                target_collection_id: Some(target.into()),
                target_folder_id: None,
                position: None,
            },
        )
    };
    let move_folder = |workspace_id: String, target: &str| {
        crate::app::folder_move_impl(
            &state,
            crate::app::MoveFolderInput {
                workspace_id,
                folder_id: "fold_1".into(),
                target_collection_id: target.into(),
                target_parent_folder_id: None,
            },
        )
    };
    assert!(not_found(move_request(other(), "col_other").unwrap_err()));
    assert!(not_found(move_folder(other(), "col_other").unwrap_err()));
    assert!(matches!(
        move_request(owner(), "col_other").unwrap_err(),
        AppError::Conflict { .. }
    ));
    assert!(matches!(
        move_folder(owner(), "col_other").unwrap_err(),
        AppError::Conflict { .. }
    ));
    let still_there = crate::app::request_list_impl(&state, owner()).unwrap();
    assert_eq!(
        still_there[0].collection_id.as_ref().unwrap().as_str(),
        "col_1"
    );

    crate::app::environment_delete_impl(&state, owner(), "env_1".into()).unwrap();
    crate::app::request_delete_impl(&state, owner(), "req_1".into()).unwrap();
    crate::app::folder_delete_impl(&state, owner(), "fold_1".into()).unwrap();
    crate::app::collection_delete_impl(&state, owner(), "col_1".into()).unwrap();
}

#[test]
fn folder_move_command_updates_descendants_and_requests() {
    let state = crate::app::AppState::open_in_memory().expect("state should open");
    crate::app::workspace_create_impl(&state, "ws_billing".into(), "Billing APIs".into()).unwrap();
    crate::app::collection_save_impl(
        &state,
        crate::app::SaveCollectionInput {
            id: "col_source".into(),
            workspace_id: "ws_billing".into(),
            name: "Source".into(),
            ..Default::default()
        },
    )
    .unwrap();
    crate::app::collection_save_impl(
        &state,
        crate::app::SaveCollectionInput {
            id: "col_target".into(),
            workspace_id: "ws_billing".into(),
            name: "Target".into(),
            ..Default::default()
        },
    )
    .unwrap();
    crate::app::folder_save_impl(
        &state,
        crate::app::SaveFolderInput {
            id: "fold_parent".into(),
            collection_id: "col_source".into(),
            name: "Parent".into(),
            ..Default::default()
        },
    )
    .unwrap();
    crate::app::folder_save_impl(
        &state,
        crate::app::SaveFolderInput {
            id: "fold_child".into(),
            collection_id: "col_source".into(),
            parent_folder_id: Some("fold_parent".into()),
            name: "Child".into(),
            ..Default::default()
        },
    )
    .unwrap();
    crate::app::request_save_impl(
        &state,
        crate::app::SaveRequestInput {
            id: "req_child".into(),
            workspace_id: "ws_billing".into(),
            collection_id: Some("col_source".into()),
            folder_id: Some("fold_child".into()),
            name: "Child request".into(),
            url: "{{baseUrl}}/child".into(),
            ..Default::default()
        },
    )
    .unwrap();

    let moved = crate::app::folder_move_impl(
        &state,
        crate::app::MoveFolderInput {
            workspace_id: "ws_billing".into(),
            folder_id: "fold_parent".into(),
            target_collection_id: "col_target".into(),
            target_parent_folder_id: None,
        },
    )
    .unwrap()
    .expect("folder should move");

    assert_eq!(moved.folders.len(), 2);
    assert!(
        moved
            .folders
            .iter()
            .all(|folder| folder.collection_id.as_str() == "col_target")
    );
    assert_eq!(
        moved.requests[0].collection_id.as_ref().unwrap().as_str(),
        "col_target"
    );
}

#[test]
fn workspace_touch_impl_moves_workspace_to_front_of_list() {
    let state = crate::app::AppState::open_in_memory().expect("state should open");
    crate::app::workspace_create_impl(&state, "ws_billing".into(), "Billing APIs".into()).unwrap();
    crate::app::workspace_create_impl(&state, "ws_other".into(), "Other APIs".into()).unwrap();

    assert!(
        crate::app::workspace_touch_impl(&state, "ws_other".into()).expect("touch should succeed")
    );

    let workspaces = crate::app::workspace_list_impl(&state).expect("workspaces should list");
    assert_eq!(workspaces[0].id.as_str(), "ws_other");
    assert_eq!(workspaces[1].id.as_str(), "ws_billing");
    assert!(
        !crate::app::workspace_touch_impl(&state, "ws_missing".into())
            .expect("touching a missing workspace should not fail")
    );
}

#[test]
fn saves_into_a_missing_workspace_are_not_found_not_a_database_error() {
    use crate::error::AppError;
    let state = crate::app::AppState::open_in_memory().expect("state should open");
    let not_found = |result: Result<(), AppError>| {
        assert!(
            matches!(result, Err(AppError::NotFound { .. })),
            "{result:?}"
        );
    };
    not_found(
        crate::app::environment_save_impl(
            &state,
            crate::app::SaveEnvironmentInput {
                id: "env_1".into(),
                workspace_id: "ws_missing".into(),
                name: "Env".into(),
                ..Default::default()
            },
        )
        .map(|_| ()),
    );
    not_found(
        crate::app::collection_save_impl(
            &state,
            crate::app::SaveCollectionInput {
                id: "col_1".into(),
                workspace_id: "ws_missing".into(),
                name: "Col".into(),
                ..Default::default()
            },
        )
        .map(|_| ()),
    );
    not_found(
        crate::app::globals_save_impl(
            &state,
            crate::app::GlobalsSaveInput {
                workspace_id: "ws_missing".into(),
                ..Default::default()
            },
        )
        .map(|_| ()),
    );
    not_found(
        crate::app::request_save_impl(
            &state,
            super::support::draft_request_input(
                "req_1",
                "ws_missing",
                "https://example.com".into(),
            ),
        )
        .map(|_| ()),
    );
}

#[test]
fn a_save_over_an_item_changed_since_it_was_read_is_refused_until_overwritten() {
    use crate::app::{
        SaveRequestInput, collection_get_impl, collection_save_checked_impl, folder_get_impl,
        folder_save_checked_impl, request_get_impl, request_save_checked_impl,
    };
    use crate::error::AppError;
    let state = crate::app::AppState::open_in_memory().expect("state should open");
    crate::app::workspace_create_impl(&state, "ws_cas".into(), "CAS".into()).unwrap();
    let mut request =
        crate::workspace::Request::new("req_cas", "ws_cas", "Read", "https://x.test/read").unwrap();
    state.store().save_request(&request).unwrap();
    let read = request_get_impl(&state, "ws_cas".into(), "req_cas".into()).unwrap();
    let base = read.revision.clone().expect("a read carries its revision");

    // The agent changes the request after the editor read it.
    request.url = "https://x.test/agent".into();
    state.store().save_request(&request).unwrap();
    let mut input: SaveRequestInput =
        serde_json::from_value(serde_json::to_value(&read).unwrap()).unwrap();
    input.name = "Edited".into();
    let refused = request_save_checked_impl(&state, input.clone(), Some(&base)).unwrap_err();
    assert!(matches!(refused, AppError::Conflict { .. }), "{refused:?}");
    let kept = request_get_impl(&state, "ws_cas".into(), "req_cas".into()).unwrap();
    assert_eq!(
        (kept.url.as_str(), kept.name.as_str()),
        ("https://x.test/agent", "Read")
    );

    // Overwriting is explicit (no revision); the answer's revision lets the next save through.
    let saved = request_save_checked_impl(&state, input.clone(), None).unwrap();
    input.description = "second edit".into();
    let edited =
        request_save_checked_impl(&state, input.clone(), saved.revision.as_deref()).unwrap();

    // A rename in the tree does not make the open tab stale: its next save goes through.
    let mut renamed = input.clone();
    renamed.name = "Renamed in tree".into();
    request_save_checked_impl(&state, renamed, edited.revision.as_deref()).unwrap();
    input.description = "third edit".into();
    request_save_checked_impl(&state, input, edited.revision.as_deref()).unwrap();

    // Collections and folders follow the same rule.
    let collection = crate::app::collection_save_impl(
        &state,
        crate::app::SaveCollectionInput {
            id: "col_cas".into(),
            workspace_id: "ws_cas".into(),
            name: "Col".into(),
            description: String::new(),
            auth: AuthConfig::Inherit,
            variables: Vec::new(),
            pre_request_script: String::new(),
            test_script: String::new(),
            position: 0,
        },
    )
    .unwrap();
    let stale = collection.revision.clone().unwrap();
    let mut edit = crate::app::SaveCollectionInput {
        id: "col_cas".into(),
        workspace_id: "ws_cas".into(),
        name: "Renamed by sync".into(),
        description: String::new(),
        auth: AuthConfig::Inherit,
        variables: Vec::new(),
        pre_request_script: String::new(),
        test_script: String::new(),
        position: 0,
    };
    collection_save_checked_impl(&state, edit.clone(), Some(&stale)).unwrap();
    edit.name = "Editor".into();
    assert!(matches!(
        collection_save_checked_impl(&state, edit, Some(&stale)),
        Err(AppError::Conflict { .. })
    ));
    assert_eq!(
        collection_get_impl(&state, "ws_cas".into(), "col_cas".into())
            .unwrap()
            .name,
        "Renamed by sync"
    );

    let folder = crate::app::folder_save_impl(
        &state,
        crate::app::SaveFolderInput {
            id: "fld_cas".into(),
            collection_id: "col_cas".into(),
            parent_folder_id: None,
            name: "Folder".into(),
            description: String::new(),
            auth: AuthConfig::Inherit,
            pre_request_script: String::new(),
            test_script: String::new(),
            position: 0,
        },
    )
    .unwrap();
    let read = folder_get_impl(&state, "ws_cas".into(), "fld_cas".into()).unwrap();
    assert_eq!(read.revision, folder.revision);
    let folder_edit = crate::app::SaveFolderInput {
        id: "fld_cas".into(),
        collection_id: "col_cas".into(),
        parent_folder_id: None,
        name: "Folder".into(),
        description: String::new(),
        auth: AuthConfig::Inherit,
        pre_request_script: "pm.test('x', () => {});".into(),
        test_script: String::new(),
        position: 3,
    };
    folder_save_checked_impl(&state, folder_edit.clone(), read.revision.as_deref()).unwrap();
    assert!(matches!(
        folder_save_checked_impl(&state, folder_edit, read.revision.as_deref()),
        Err(AppError::Conflict { .. })
    ));
}
