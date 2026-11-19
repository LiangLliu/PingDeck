//! Storage tests for environments, collection variables, and global variables.

use crate::storage::StorageError;
use pingdeck_model::{
    Collection, Environment, EnvironmentId, EnvironmentVariable, Workspace, WorkspaceId,
};

use super::super::WorkspaceStore;

fn secret_refs(store: &WorkspaceStore) -> Vec<String> {
    let mut statement = store
        .connection
        .prepare("SELECT secret_ref FROM secrets ORDER BY secret_ref")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn raw_values(store: &WorkspaceStore, table: &str) -> Vec<String> {
    let mut statement = store
        .connection
        .prepare(&format!("SELECT value FROM {table} ORDER BY key"))
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn environment_with(
    id: &str,
    workspace: &Workspace,
    variables: &[(&str, &str, bool)],
) -> Environment {
    let mut environment = Environment::new(id, workspace.id.as_str(), id).unwrap();
    for (key, value, secret) in variables {
        environment.variables.push(if *secret {
            EnvironmentVariable::masked(key, *value).unwrap()
        } else {
            EnvironmentVariable::plain(key, *value).unwrap()
        });
    }
    environment
}

#[test]
fn environments_globals_and_collection_secrets_round_trip_and_clean_up() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let ws = Workspace::new("ws", "W").unwrap();
    let other_ws = Workspace::new("ws_other", "Other").unwrap();
    store.save_workspace(&ws).unwrap();
    store.save_workspace(&other_ws).unwrap();

    let mut env = environment_with(
        "env_a",
        &ws,
        &[
            ("token", "tok-secret-1", true),
            ("host", "h", false),
            ("none", "", true),
        ],
    );
    env.confirm_before_send = true;
    env.confirm_safe_methods = true;
    env.variables[0].note = "Sent as Billing-Version".into();
    store.save_environment(&env).unwrap();

    let mut globals = vec![
        EnvironmentVariable::masked("api", "glob-secret-1").unwrap(),
        EnvironmentVariable::plain("region", "eu").unwrap(),
    ];
    globals[1].note = "every environment".into();
    store.save_global_variables("ws", &globals).unwrap();

    let mut col = Collection::new("col_a", "ws", "Col").unwrap();
    col.variables = vec![
        EnvironmentVariable::masked("col_tok", "col-secret-1").unwrap(),
        EnvironmentVariable::plain("host", "h").unwrap(),
    ];
    store.save_collection(&col).unwrap();

    assert_eq!(
        secret_refs(&store),
        vec![
            "collection.col_a.col_tok",
            "env.env_a.token",
            "global.ws.api"
        ]
    );
    assert_eq!(
        raw_values(&store, "environment_variables"),
        vec!["h", "", ""]
    );
    assert_eq!(raw_values(&store, "global_variables"), vec!["", "eu"]);
    assert_eq!(
        store.get_environment(&ws.id, &env.id).unwrap(),
        Some(env.clone())
    );
    assert_eq!(store.list_global_variables("ws").unwrap(), globals);
    assert_eq!(
        store.get_collection(&ws.id, &col.id).unwrap(),
        Some(col.clone())
    );
    let stored_collection: String = store
        .connection
        .query_row(
            "SELECT variables_json FROM collections WHERE id = 'col_a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!stored_collection.contains("col-secret-1"));
    env.confirm_before_send = false;
    store.save_environment(&env).unwrap();
    let updated = store.get_environment(&ws.id, &env.id).unwrap().unwrap();
    assert!(!updated.confirm_before_send && updated.confirm_safe_methods);
    assert_eq!(
        store
            .stored_environment_variable_value(&ws.id, "env_a", "token")
            .unwrap()
            .as_deref(),
        Some("tok-secret-1")
    );

    // Replacing variables drops removed secrets
    let replaced_env = environment_with("env_a", &ws, &[("token", "tok-secret-2", true)]);
    store.save_environment(&replaced_env).unwrap();
    store.save_global_variables("ws", &[]).unwrap();
    col.variables.clear();
    store.save_collection(&col).unwrap();
    assert_eq!(secret_refs(&store), vec!["env.env_a.token"]);

    // Cross-workspace isolation and uniqueness
    assert!(matches!(
        store.save_environment(&Environment::new("env_a", "ws_other", "Stolen").unwrap()),
        Err(StorageError::ItemWorkspaceMismatch { .. })
    ));
    assert_eq!(store.get_environment(&other_ws.id, &env.id).unwrap(), None);
    assert_eq!(
        store
            .stored_environment_variable_value(&other_ws.id, "env_a", "token")
            .unwrap(),
        None
    );
    assert!(!store.delete_environment(&other_ws.id, &env.id).unwrap());

    assert!(matches!(
        store.save_environment(&Environment::new("env_dup", "ws", "ENV_A").unwrap()),
        Err(StorageError::Sqlite(_))
    ));
    assert!(
        !store
            .environment_name_taken(&ws.id, "ENV_A", &env.id)
            .unwrap()
    );
    assert!(
        store
            .environment_name_taken(&ws.id, "ENV_A", &EnvironmentId::new("env_x").unwrap())
            .unwrap()
    );

    // Deleting environment or workspace removes secrets
    assert!(store.delete_environment(&ws.id, &env.id).unwrap());
    assert!(secret_refs(&store).is_empty());
    let mut kept = environment_with("env_keep", &other_ws, &[("k", "keep-secret", true)]);
    kept.name = "env_a".to_owned();
    store.save_environment(&kept).unwrap();

    store
        .save_environment(&environment_with("env_b", &ws, &[("k", "sec", true)]))
        .unwrap();
    store
        .save_global_variables("ws", &[EnvironmentVariable::masked("g", "sec").unwrap()])
        .unwrap();
    assert!(
        store
            .delete_workspace(&WorkspaceId::new("ws").unwrap())
            .unwrap()
    );
    assert_eq!(secret_refs(&store), vec!["env.env_keep.k"]);
}

#[test]
fn script_state_rolls_back_all_scopes_when_global_insert_fails() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let workspace = Workspace::new("ws_scripts", "Scripts").unwrap();
    let mut collection =
        Collection::new("col_scripts", workspace.id.as_str(), "Collection").unwrap();
    let mut environment =
        Environment::new("env_scripts", workspace.id.as_str(), "Environment").unwrap();
    let original = vec![EnvironmentVariable::plain("key", "before").unwrap()];
    collection.variables = original.clone();
    environment.variables = original.clone();
    store.save_workspace(&workspace).unwrap();
    store.save_collection(&collection).unwrap();
    store.save_environment(&environment).unwrap();
    store
        .save_global_variables(workspace.id.as_str(), &original)
        .unwrap();
    store
        .connection
        .execute_batch(
            "CREATE TRIGGER fail_script_global BEFORE INSERT ON global_variables
             WHEN NEW.key = 'fail' BEGIN SELECT RAISE(ABORT, 'script global failure'); END;",
        )
        .unwrap();

    let mut changed_col = collection.clone();
    changed_col.variables = vec![EnvironmentVariable::plain("key", "after").unwrap()];
    let mut changed_env = environment.clone();
    changed_env.variables = vec![EnvironmentVariable::plain("key", "after").unwrap()];
    let changed_globals = vec![EnvironmentVariable::plain("fail", "after").unwrap()];
    assert!(
        store
            .save_script_state(
                workspace.id.as_str(),
                Some(&changed_col),
                Some(&changed_env),
                Some(&changed_globals),
            )
            .is_err()
    );
    assert_eq!(
        store.get_collection(&workspace.id, &collection.id).unwrap(),
        Some(collection)
    );
    assert_eq!(
        store
            .get_environment(&workspace.id, &environment.id)
            .unwrap(),
        Some(environment)
    );
    assert_eq!(
        store.list_global_variables(workspace.id.as_str()).unwrap(),
        original
    );
}
