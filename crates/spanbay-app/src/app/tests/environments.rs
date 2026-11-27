//! Workbench environments, globals and collection variables: redacted DTOs, keep-secret saves,
//! unique names, duplicate, delete with undo, export/import extension keys, compare, the
//! unresolved-variable scan, stale-save refusal, duplicate keys, resolved lists, variable define
//! and guard timestamps.

use std::time::{Duration, Instant};

use super::support::{SECRET, env_input, request, row, save_env, state};
use crate::app::environment_service::{environment_delete_at, environment_restore_at};
use crate::app::{
    AppState, CompareState, EnvironmentDto, EnvironmentVariableDto, EnvironmentVariableInput,
    GlobalsSaveInput, SaveCollectionInput, SaveEnvironmentInput, UndoToken,
    UnresolvedCollectionDto, collection_list_impl, collection_save_impl, environment_compare_impl,
    environment_duplicate_impl, environment_export_impl, environment_import_text_impl,
    environment_list_impl, environment_save_impl, environment_unresolved_scan_impl,
    globals_get_impl, globals_save_impl,
};
use crate::error::AppError;
use crate::workspace::{
    AuthConfig, CollectionId, EnvironmentVariable, Flow, Folder, FolderId, FormPart, KeyValue,
    RequestBody, VariableScopes,
};

fn row_of(
    dto: &EnvironmentVariableDto,
    key: &str,
    value: Option<&str>,
) -> EnvironmentVariableInput {
    EnvironmentVariableInput {
        id: Some(dto.id.clone()),
        key: key.into(),
        value: value.map(Into::into),
        is_secret: dto.is_secret,
        enabled: dto.enabled,
        note: dto.note.clone(),
    }
}

fn collection_input(id: &str, variables: Vec<EnvironmentVariableInput>) -> SaveCollectionInput {
    SaveCollectionInput {
        id: id.into(),
        workspace_id: "ws1".into(),
        name: id.into(),
        variables,
        ..Default::default()
    }
}

fn stored_value(state: &AppState, environment_id: &str, key: &str) -> Option<String> {
    state
        .store()
        .list_environments("ws1")
        .unwrap()
        .into_iter()
        .find(|environment| environment.id.as_str() == environment_id)?
        .variables
        .into_iter()
        .find(|variable| variable.key == key)?
        .value
}

fn env_secret_rows(state: &AppState) -> i64 {
    state
        .store()
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM secrets WHERE secret_ref LIKE 'env.%'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[derive(Clone, Copy, Debug)]
enum Scope {
    Environment,
    Globals,
    Collection,
}

const SCOPES: [Scope; 3] = [Scope::Environment, Scope::Globals, Scope::Collection];

impl Scope {
    fn save(
        self,
        state: &AppState,
        rows: Vec<EnvironmentVariableInput>,
    ) -> Vec<EnvironmentVariableDto> {
        self.try_save(state, rows).unwrap()
    }

    fn try_save(
        self,
        state: &AppState,
        rows: Vec<EnvironmentVariableInput>,
    ) -> Result<Vec<EnvironmentVariableDto>, AppError> {
        match self {
            Self::Environment => save_env(state, "env_a", "A", rows).map(|s| s.variables),
            Self::Globals => globals_save_impl(
                state,
                GlobalsSaveInput {
                    workspace_id: "ws1".into(),
                    variables: rows,
                    base_revision: None,
                },
            )
            .map(|saved| saved.variables),
            Self::Collection => {
                collection_save_impl(state, collection_input("col_a", rows)).map(|s| s.variables)
            }
        }
    }

    fn list(self, state: &AppState) -> Vec<EnvironmentVariableDto> {
        match self {
            Self::Environment => {
                environment_list_impl(state, "ws1".into())
                    .unwrap()
                    .remove(0)
                    .variables
            }
            Self::Globals => globals_get_impl(state, "ws1".into()).unwrap().variables,
            Self::Collection => {
                collection_list_impl(state, "ws1".into())
                    .unwrap()
                    .remove(0)
                    .variables
            }
        }
    }

    fn stored(self, state: &AppState) -> Vec<EnvironmentVariable> {
        let store = state.store();
        match self {
            Self::Environment => store.list_environments("ws1").unwrap().remove(0).variables,
            Self::Globals => store.list_global_variables("ws1").unwrap(),
            Self::Collection => store.list_collections("ws1").unwrap().remove(0).variables,
        }
    }

    fn secret_prefix(self) -> &'static str {
        match self {
            Self::Environment => "env.env_a.",
            Self::Globals => "global.ws1.",
            Self::Collection => "collection.col_a.",
        }
    }

    fn secret_refs(self, state: &AppState) -> Vec<String> {
        let store = state.store();
        let mut stmt = store
            .connection()
            .prepare("SELECT secret_ref FROM secrets WHERE secret_ref LIKE ?1 ORDER BY secret_ref")
            .unwrap();
        stmt.query_map([format!("{}%", self.secret_prefix())], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn secret_ref(self, key: &str) -> String {
        format!("{}{key}", self.secret_prefix())
    }
}

fn resolved(variables: &[EnvironmentVariable], key: &str) -> Option<String> {
    crate::workspace::resolve_template(
        &format!("{{{{{key}}}}}"),
        &VariableScopes::new().with_environment_vars(variables.to_vec()),
    )
    .ok()
}

#[test]
fn scopes_redact_preserve_rename_replace_and_validate_secret_variables() {
    let state = state();
    let empty = save_env(
        &state,
        "env_empty",
        "Empty",
        vec![row("token", Some(""), true)],
    )
    .unwrap();
    assert!(empty.variables[0].is_secret && !empty.variables[0].has_secret);

    for scope in SCOPES {
        let state = self::state();
        let saved = scope.save(
            &state,
            vec![
                row("host", Some("h.example"), false),
                row("token", Some(SECRET), true),
            ],
        );
        let listed = scope.list(&state);
        for vars in [&saved, &listed] {
            assert_eq!(vars[0].value.as_deref(), Some("h.example"));
            assert!(!vars[0].has_secret);
            assert_eq!(vars[1].value, None);
            assert!(vars[1].is_secret && vars[1].has_secret);
            assert!(vars[1].id.starts_with("var_"), "{scope:?}: {}", vars[1].id);
            assert!(!serde_json::to_string(vars).unwrap().contains(SECRET));
        }
        assert_eq!(
            resolved(&scope.stored(&state), "token").as_deref(),
            Some(SECRET)
        );

        // Untouched by key (id: None) and untouched by id (id: Some) across saves keep value.
        let by_key = scope.save(
            &state,
            vec![row("host", Some("h2"), false), row("token", None, true)],
        );
        let by_id = scope.save(
            &state,
            vec![
                row_of(&by_key[0], "host", Some("h3")),
                row_of(&by_key[1], "token", None),
            ],
        );
        assert_eq!(by_id[1].id, saved[1].id, "{scope:?}: identity is stable");
        assert!(by_id[1].has_secret);
        assert_eq!(
            resolved(&scope.stored(&state), "token").as_deref(),
            Some(SECRET)
        );
        assert_eq!(
            resolved(&scope.stored(&state), "host").as_deref(),
            Some("h3")
        );
        assert_eq!(scope.secret_refs(&state), vec![scope.secret_ref("token")]);

        // Rename by id moves the secret to the new key and drops the old key.
        let renamed = scope.save(&state, vec![row_of(&by_id[1], "api_token", None)]);
        assert_eq!(renamed[0].id, saved[1].id);
        assert!(renamed[0].has_secret);
        let stored = scope.stored(&state);
        assert_eq!(resolved(&stored, "api_token").as_deref(), Some(SECRET));
        assert_eq!(resolved(&stored, "token"), None);
        assert_eq!(
            scope.secret_refs(&state),
            vec![scope.secret_ref("api_token")]
        );

        // Some("new-secret") replaces the stored secret.
        scope.save(&state, vec![row("api_token", Some("new-secret"), true)]);
        assert_eq!(
            resolved(&scope.stored(&state), "api_token").as_deref(),
            Some("new-secret")
        );

        // A secret row without a value and nothing stored names the key and changes nothing.
        let error = scope
            .try_save(&state, vec![row("again", None, true)])
            .unwrap_err();
        assert!(matches!(&error, AppError::Validation { .. }));
        assert!(
            error
                .message()
                .contains("secret variable again needs a value"),
            "{error}"
        );
        assert_eq!(
            resolved(&scope.stored(&state), "api_token").as_deref(),
            Some("new-secret")
        );
    }
}

#[test]
fn environment_names_flags_duplicate_and_undo_restore_lifecycle() {
    let state = state();
    let vars = || {
        vec![
            row("token", Some(SECRET), true),
            row("host", Some("h"), false),
        ]
    };
    let input = |id: &str, ws: &str, name: &str, before: Option<bool>, safe: Option<bool>| {
        SaveEnvironmentInput {
            confirm_before_send: before,
            confirm_safe_methods: safe,
            ..env_input(id, ws, name, vars())
        }
    };
    let saved = environment_save_impl(
        &state,
        input("env_a", "ws1", "Staging", Some(true), Some(true)),
    )
    .unwrap();
    assert!(saved.confirm_before_send && saved.confirm_safe_methods);

    let clash = save_env(&state, "env_b", "staging", vec![]).unwrap_err();
    assert!(
        matches!(clash, AppError::Validation { .. })
            && clash.message() == "An environment named staging already exists"
    );
    environment_save_impl(&state, input("env_c", "ws2", "Staging", None, None)).unwrap();

    let kept = environment_save_impl(
        &state,
        SaveEnvironmentInput {
            variables: vec![row("token", None, true), row("host", Some("h"), false)],
            ..input("env_a", "ws1", "STAGING", None, Some(false))
        },
    )
    .unwrap();
    assert!(kept.confirm_before_send && !kept.confirm_safe_methods);
    assert_eq!(
        environment_list_impl(&state, "ws1".into()).unwrap(),
        vec![kept]
    );

    environment_save_impl(
        &state,
        input("env_a", "ws1", "Staging", Some(true), Some(true)),
    )
    .unwrap();
    let first = environment_duplicate_impl(&state, "ws1".into(), "env_a".into()).unwrap();
    let second = environment_duplicate_impl(&state, "ws1".into(), "env_a".into()).unwrap();
    assert_eq!(
        (first.name.as_str(), second.name.as_str()),
        ("Staging copy", "Staging copy 2")
    );
    assert!(first.id != "env_a" && first.id != second.id);
    assert!(first.confirm_before_send && first.confirm_safe_methods);
    for id in ["env_a", first.id.as_str(), second.id.as_str()] {
        assert_eq!(stored_value(&state, id, "token").as_deref(), Some(SECRET));
    }
    assert_eq!(env_secret_rows(&state), 4);
    assert!(matches!(
        environment_duplicate_impl(&state, "ws1".into(), "env_nope".into()),
        Err(AppError::NotFound { .. })
    ));

    let state = self::state();
    environment_save_impl(
        &state,
        input("env_a", "ws1", "Production", Some(true), None),
    )
    .unwrap();
    let before = state.store().list_environments("ws1").unwrap();
    let t0 = Instant::now();

    let token = environment_delete_at(&state, "ws1", "env_a", t0).unwrap();
    assert!(
        environment_list_impl(&state, "ws1".into())
            .unwrap()
            .is_empty()
    );
    assert_eq!(env_secret_rows(&state), 0);

    let restored =
        environment_restore_at(&state, "ws1", &token, t0 + Duration::from_secs(20)).unwrap();
    assert!(restored.id == "env_a" && restored.confirm_before_send);
    assert_eq!(state.store().list_environments("ws1").unwrap(), before);
    assert_eq!(env_secret_rows(&state), 1);
    assert!(matches!(
        environment_restore_at(&state, "ws1", &token, t0 + Duration::from_secs(21)),
        Err(AppError::Conflict { .. } | AppError::NotFound { .. })
    ));

    let token2 = environment_delete_at(&state, "ws1", "env_a", t0).unwrap();
    for (ws, tok, elapsed) in [
        ("ws2", token2.clone(), Duration::from_secs(1)),
        (
            "ws1",
            UndoToken("undo_unknown".into()),
            Duration::from_secs(1),
        ),
        ("ws1", token2, Duration::from_secs(60)),
    ] {
        assert!(matches!(
            environment_restore_at(&state, ws, &tok, t0 + elapsed),
            Err(AppError::NotFound { .. })
        ));
    }
    assert_eq!(env_secret_rows(&state), 0);

    save_env(&state, "env_a", "A", vec![row("token", Some(SECRET), true)]).unwrap();
    let token3 = crate::app::environment_delete_impl(&state, "ws1".into(), "env_a".into()).unwrap();
    assert!(crate::app::environment_undo_commit_impl(
        &state,
        token3.clone()
    ));
    let restored3 = crate::app::environment_restore_impl(&state, "ws1".into(), token3).unwrap();
    assert_eq!(restored3.id, "env_a");
    assert!(!crate::app::environment_undo_commit_impl(
        &state,
        UndoToken("undo_unknown".into())
    ));
}

#[test]
fn export_import_and_compare_handle_secrets_and_extension_keys() {
    let state = state();
    let mut note_row = row("host", Some("h"), false);
    note_row.note = "Sent as Billing-Version".into();
    environment_save_impl(
        &state,
        SaveEnvironmentInput {
            confirm_before_send: Some(true),
            confirm_safe_methods: Some(false),
            ..env_input(
                "env_a",
                "ws1",
                "Production",
                vec![note_row, row("token", Some(SECRET), true)],
            )
        },
    )
    .unwrap();
    let safe = environment_export_impl(&state, "ws1".into(), "env_a".into(), false).unwrap();
    assert!(!safe.contains(SECRET), "{safe}");
    let parsed: serde_json::Value = serde_json::from_str(&safe).unwrap();
    assert_eq!(
        (
            parsed["values"][1]["type"].as_str(),
            parsed["values"][1]["value"].as_str(),
            parsed["_spanbay"]["confirm_before_send"].as_bool(),
            parsed["_spanbay"]["confirm_safe_methods"].as_bool(),
            parsed["values"][0]["_spanbay"]["note"].as_str(),
        ),
        (
            Some("secret"),
            Some(""),
            Some(true),
            Some(false),
            Some("Sent as Billing-Version"),
        )
    );

    let full = environment_export_impl(&state, "ws1".into(), "env_a".into(), true).unwrap();
    let full_json: serde_json::Value = serde_json::from_str(&full).unwrap();
    assert_eq!(full_json["values"][1]["value"], SECRET);

    let imported = environment_import_text_impl(
        &state,
        "ws2".into(),
        full,
        crate::app::ImportConflict::KeepBoth,
    )
    .unwrap();
    assert!(imported.confirm_before_send && !imported.confirm_safe_methods);
    assert_eq!(imported.variables[0].note, "Sent as Billing-Version");
    assert!(
        imported.variables[1].has_secret
            && !serde_json::to_string(&imported).unwrap().contains(SECRET)
    );
    let imported_domain = &state.store().list_environments("ws2").unwrap()[0];
    assert_eq!(imported_domain.variables[1].value.as_deref(), Some(SECRET));

    let plain = r#"{"name":"Production","values":[{"key":"a","value":"1","type":"default","enabled":true}]}"#;
    let again = environment_import_text_impl(
        &state,
        "ws2".into(),
        plain.into(),
        crate::app::ImportConflict::KeepBoth,
    )
    .unwrap();
    assert_eq!(again.name, "Production (2)");
    assert!(
        !again.confirm_before_send
            && !again.confirm_safe_methods
            && again.variables[0].note.is_empty()
    );

    let mut disabled = row("dis", Some("d"), false);
    disabled.enabled = false;
    save_env(
        &state,
        "env_l",
        "Left",
        vec![
            row("host", Some("a.example"), false),
            row("tok", Some("secret-aaa"), true),
            row("only_l", Some("1"), false),
            row("shared", Some("same"), false),
            row("mix", Some("secret-mix"), true),
            row("same_secret", Some("secret-same"), true),
            disabled,
        ],
    )
    .unwrap();
    save_env(
        &state,
        "env_r",
        "Right",
        vec![
            row("host", Some("b.example"), false),
            row("tok", Some("secret-bbb"), true),
            row("mix", Some("secret-mix"), false),
            row("shared", Some("same"), false),
            row("same_secret", Some("secret-same"), true),
            row("only_r", Some("2"), false),
            row("dis", Some("d"), false),
        ],
    )
    .unwrap();

    let rows = environment_compare_impl(&state, "ws1".into(), "env_l".into(), "env_r".into(), None)
        .unwrap();
    let summary: Vec<(&str, CompareState)> = rows
        .iter()
        .map(|compared| (compared.key.as_str(), compared.state))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("host", CompareState::Differs),
            ("tok", CompareState::Differs),
            ("only_l", CompareState::OnlyLeft),
            ("shared", CompareState::Same),
            ("mix", CompareState::Differs),
            ("same_secret", CompareState::Same),
            ("only_r", CompareState::OnlyRight),
            ("dis", CompareState::OnlyRight),
        ]
    );
    assert_eq!(
        (rows[0].left.as_deref(), rows[0].right.as_deref()),
        (Some("a.example"), Some("b.example"))
    );
    assert!(rows[1].left_secret && rows[1].right_secret);
    assert_eq!(
        (rows[1].left.as_ref(), rows[1].right.as_ref()),
        (None, None)
    );
    assert!(
        rows[4].left.is_none()
            && rows[4].left_secret
            && !rows[4].right_secret
            && rows[4].right.as_deref() == Some("secret-mix")
    );
    let json = serde_json::to_string(&rows).unwrap();
    for secret in ["secret-aaa", "secret-bbb", "secret-same"] {
        assert!(!json.contains(secret), "{json}");
    }
    assert!(matches!(
        environment_compare_impl(&state, "ws1".into(), "env_l".into(), "env_x".into(), None),
        Err(AppError::NotFound { .. })
    ));
}

#[test]
fn unresolved_scan_lists_undefined_tokens_with_their_requests() {
    let state = state();
    let mut disabled = row("off", Some("1"), false);
    disabled.enabled = false;
    save_env(
        &state,
        "env_a",
        "Staging",
        vec![row("base_url", Some("https://x"), false), disabled],
    )
    .unwrap();
    globals_save_impl(
        &state,
        GlobalsSaveInput {
            workspace_id: "ws1".into(),
            variables: vec![row("g_var", Some("g"), false)],
            base_revision: None,
        },
    )
    .unwrap();
    let mut input = collection_input("c1", vec![row("col_var", Some("c"), false)]);
    input.auth = AuthConfig::Bearer {
        token_ref: "{{auth_token}}".into(),
    };
    collection_save_impl(&state, input).unwrap();

    let store = state.store();
    store
        .save_folder(&Folder::new("f1", "c1", "Folder").unwrap())
        .unwrap();
    // A secret synced without its value: it shadows the global `g_var` and needs a value.
    let mut staging = store.list_environments("ws1").unwrap().remove(0);
    staging
        .variables
        .push(EnvironmentVariable::masked("g_var", "").unwrap());
    store.save_environment(&staging).unwrap();
    let header = |key: &str, value: &str, enabled: bool| KeyValue {
        key: key.into(),
        value: value.into(),
        enabled,
        description: None,
    };
    let mut r1 = request("r1", "{{base_url}}/x/{{customer_id}}?a={{$guid}}");
    r1.folder_id = Some(FolderId::new("f1").unwrap());
    r1.headers = vec![
        header("X", "{{g_var}}", true),
        header("Y", "{{col_var}}", true),
        header("Z", "{{disabled_hdr}}", false),
    ];
    r1.body = RequestBody::Json {
        content:
            r#"{"id":"{{customer_id}}","s":"{{secret:KEY}}","t":"{{scripted}}","u":"{{ off }}"}"#
                .into(),
    };
    let mut r2 = request("r2", "{{customer_id}}{{col_var}}");
    r2.body = RequestBody::FormUrlEncoded {
        fields: vec![header("k", "{{form_val}}", true)],
    };
    let mut r2b = request("r2b", "http://h");
    r2b.body = RequestBody::Multipart {
        parts: vec![FormPart {
            key: "p".into(),
            value: "{{mp}}".into(),
            file_path: None,
            enabled: true,
        }],
    };
    let mut r3 = request("r3", "http://h");
    r3.body = RequestBody::Graphql {
        query: "{ a(id: \"{{gql_var}}\") }".into(),
        variables_json: Some(r#"{"x":"{{gql_extracted}}"}"#.into()),
        operation_name: None,
    };
    let mut r4 = request("r4", "{{extracted}}");
    r4.pre_request_script =
        "pm.environment.set(\"scripted\", \"1\"); pm.variables.set('other', 2);".into();
    r4.test_script = "pm.globals.set(\"unused\", 1);".into();
    let mut r5 = request("r5", "{{other}}{{unused}}");
    r5.collection_id = Some(CollectionId::new("c1").unwrap());
    r5.auth = AuthConfig::None;
    for req in [&r1, &r2, &r2b, &r3, &r4, &r5] {
        store.save_request(req).unwrap();
    }
    let flow: Flow = serde_json::from_value(serde_json::json!({
        "id": "flow1", "workspace_id": "ws1", "name": "f", "description": "",
        "updated_at_ms": 0,
        "steps": [{
            "id": "s1", "name": "s", "delay_ms": 0, "continue_on_error": false, "position": 0,
            "extractions": [
                {"variable_name": "gql_extracted", "source": "BodyJsonPath", "expression": "$.a"},
                {"variable_name": "extracted", "source": "BodyJsonPath", "expression": "$.b"},
            ],
        }],
    }))
    .unwrap();
    store.save_flow(&flow).unwrap();
    drop(store);

    let before = state.store().list_requests("ws1").unwrap();
    let rows =
        environment_unresolved_scan_impl(&state, "ws1".into(), Some("env_a".into())).unwrap();
    let summary: Vec<(&str, Vec<&str>, u32)> = rows
        .iter()
        .map(|row| {
            (
                row.name.as_str(),
                row.request_ids.iter().map(String::as_str).collect(),
                row.request_count,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("customer_id", vec!["r1", "r2"], 2),
            ("auth_token", vec!["r1"], 1),
            ("col_var", vec!["r2"], 1),
            ("form_val", vec!["r2"], 1),
            ("g_var", vec!["r1"], 1),
            ("gql_var", vec!["r3"], 1),
            ("mp", vec!["r2b"], 1),
            ("off", vec!["r1"], 1),
        ]
    );
    for row in &rows {
        let ids: Vec<&str> = row.requests.iter().map(|user| user.id.as_str()).collect();
        assert_eq!(
            ids,
            row.request_ids
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        for user in &row.requests {
            let saved = before
                .iter()
                .find(|request| request.id.as_str() == user.id)
                .unwrap();
            assert_eq!((&user.name, user.method), (&saved.name, saved.method));
        }
    }
    assert_eq!(state.store().list_requests("ws1").unwrap(), before);

    let col_var = rows.iter().find(|row| row.name == "col_var").unwrap();
    assert_eq!(
        col_var.collections,
        vec![UnresolvedCollectionDto {
            id: "c1".into(),
            name: "c1".into()
        }]
    );
    assert!(
        rows.iter()
            .filter(|row| row.name != "col_var")
            .all(|row| row.collections.is_empty())
    );
    assert!(matches!(
        environment_unresolved_scan_impl(&state, "ws1".into(), Some("env_x".into())),
        Err(AppError::NotFound { .. })
    ));
}

fn is_stale(error: &AppError) -> bool {
    matches!(error, AppError::Conflict { .. })
        && error.code() == Some(crate::error::ErrorCode::StaleWrite)
}

/// A script writes `token` (an id-less row, as `merge_script_variables` leaves it) while the
/// page holds the older copy.
fn script_writes_token(state: &AppState) {
    let mut environment = state
        .store()
        .get_environment(
            &crate::workspace::WorkspaceId::new("ws1").unwrap(),
            &crate::workspace::EnvironmentId::new("env_a").unwrap(),
        )
        .unwrap()
        .unwrap();
    environment
        .variables
        .push(EnvironmentVariable::plain("token", "from-script").unwrap());
    state
        .store()
        .save_script_state("ws1", None, Some(&environment), None)
        .unwrap();
}

#[test]
fn a_stale_environment_save_is_refused_and_keeps_the_script_value() {
    let state = state();
    let page = save_env(&state, "env_a", "A", vec![row("host", Some("a"), false)]).unwrap();
    // Read, then save at once: the revision is stable, so no false conflict.
    let listed = environment_list_impl(&state, "ws1".into())
        .unwrap()
        .remove(0);
    assert_eq!(listed.revision, page.revision);

    script_writes_token(&state);
    let mut stale = env_input(
        "env_a",
        "ws1",
        "A",
        vec![row_of(&page.variables[0], "host", Some("b"))],
    );
    stale.base_revision = Some(page.revision.clone());
    assert!(is_stale(
        &environment_save_impl(&state, stale.clone()).unwrap_err()
    ));
    assert_eq!(
        stored_value(&state, "env_a", "token").as_deref(),
        Some("from-script")
    );

    // Reloaded, the page edits `host` again and the script's `token` survives the save.
    let fresh = environment_list_impl(&state, "ws1".into())
        .unwrap()
        .remove(0);
    let mut rows: Vec<_> = fresh
        .variables
        .iter()
        .map(|v| row_of(v, &v.key, v.value.as_deref()))
        .collect();
    rows[0].value = Some("b".into());
    let saved = environment_save_impl(
        &state,
        SaveEnvironmentInput {
            base_revision: Some(fresh.revision.clone()),
            ..env_input("env_a", "ws1", "A", rows)
        },
    )
    .unwrap();
    assert_ne!(saved.revision, fresh.revision);
    assert_eq!(stored_value(&state, "env_a", "host").as_deref(), Some("b"));
    assert_eq!(
        stored_value(&state, "env_a", "token").as_deref(),
        Some("from-script")
    );
    // The id-less row took the effective id the page held, so the next list agrees with it.
    assert_eq!(saved.variables[1].id, "env_a:token");
    assert_eq!(
        state.store().list_environments("ws1").unwrap()[0].variables[1].id,
        "env_a:token"
    );
    // The returned revision is the stored one; without a base revision the old overwrite stays.
    assert_eq!(
        environment_list_impl(&state, "ws1".into()).unwrap()[0].revision,
        saved.revision
    );
    stale.base_revision = None;
    environment_save_impl(&state, stale).unwrap();
    assert_eq!(stored_value(&state, "env_a", "token"), None);
}

#[test]
fn a_stale_globals_save_is_refused() {
    let state = state();
    let save = |rows, base: Option<String>| {
        globals_save_impl(
            &state,
            GlobalsSaveInput {
                workspace_id: "ws1".into(),
                variables: rows,
                base_revision: base,
            },
        )
    };
    let page = save(vec![row("a", Some("1"), false)], None).unwrap();
    assert_eq!(globals_get_impl(&state, "ws1".into()).unwrap(), page);
    let mut globals = state.store().list_global_variables("ws1").unwrap();
    globals.push(EnvironmentVariable::plain("token", "from-script").unwrap());
    state
        .store()
        .save_script_state("ws1", None, None, Some(&globals))
        .unwrap();
    let edit = vec![row_of(&page.variables[0], "a", Some("2"))];
    assert!(is_stale(
        &save(edit.clone(), Some(page.revision)).unwrap_err()
    ));
    let fresh = globals_get_impl(&state, "ws1".into()).unwrap();
    assert_eq!(fresh.variables.len(), 2);
    let mut rows = edit;
    rows.push(row_of(&fresh.variables[1], "token", Some("from-script")));
    assert_eq!(save(rows, Some(fresh.revision)).unwrap().variables.len(), 2);
}

#[test]
fn duplicate_keys_are_refused_with_a_code_in_environments_and_globals() {
    use crate::app::misc_logic_service::{IssueCode, validate_environment, validate_globals};
    let state = state();
    let rows = vec![
        row("token", Some("a"), false),
        row("other", Some("b"), false),
        row(" token ", Some("c"), false),
    ];
    let issues = validate_environment("A", &rows);
    assert_eq!(issues.len(), 1);
    assert_eq!(
        (issues[0].code, issues[0].row),
        (Some(IssueCode::KeyDuplicate), Some(2))
    );
    let error = save_env(&state, "env_a", "A", rows.clone()).unwrap_err();
    assert!(matches!(error, AppError::Validation { .. }));
    assert_eq!(error.message(), issues[0].message);

    let issues = validate_globals(&rows);
    assert_eq!(issues[0].code, Some(IssueCode::KeyDuplicate));
    let error = globals_save_impl(
        &state,
        GlobalsSaveInput {
            workspace_id: "ws1".into(),
            variables: rows,
            base_revision: None,
        },
    )
    .unwrap_err();
    assert_eq!(error.message(), issues[0].message);
    assert!(
        globals_get_impl(&state, "ws1".into())
            .unwrap()
            .variables
            .is_empty()
    );
}

#[test]
fn resolved_list_follows_the_core_order_and_never_returns_a_secret() {
    let state = state();
    save_env(
        &state,
        "env_a",
        "A",
        vec![
            row("base_url", Some("https://env"), false),
            row("token", Some(SECRET), true),
            row("empty_secret", Some(""), true),
            EnvironmentVariableInput {
                enabled: false,
                ..row("page", Some("10"), false)
            },
        ],
    )
    .unwrap();
    globals_save_impl(
        &state,
        GlobalsSaveInput {
            workspace_id: "ws1".into(),
            variables: vec![
                row("base_url", Some("https://global"), false),
                row("page", Some("20"), false),
                row("empty_secret", Some("g"), false),
            ],
            base_revision: None,
        },
    )
    .unwrap();

    let rows = crate::app::environment_resolved_list_impl(
        &state,
        "ws1".into(),
        Some("env_a".into()),
        None,
    )
    .unwrap();
    let by = |name: &str| rows.iter().find(|r| r.name == name).unwrap();
    use crate::workspace::VariableResolutionSource as S;
    // the environment beats the globals; the overridden global is listed under it
    assert_eq!(by("base_url").source, S::Environment);
    assert_eq!(by("base_url").value.as_deref(), Some("https://env"));
    assert_eq!(by("base_url").layers.len(), 2);
    // a disabled environment row does not resolve: the global wins
    assert_eq!(
        (by("page").source, by("page").value.as_deref()),
        (S::Global, Some("20"))
    );
    assert!(!by("page").layers[0].enabled);
    // an empty secret shadows the global of its name and needs a value
    let empty = by("empty_secret");
    assert_eq!(
        (empty.source, empty.value.as_deref()),
        (S::Environment, None)
    );
    assert!(empty.is_secret && !empty.layers[0].has_value);
    assert_eq!(by("token").source, S::Environment);
    assert!(by("token").is_secret && by("token").value.is_none());
    let wire = serde_json::to_string(&rows).unwrap();
    assert!(
        !wire.contains(SECRET),
        "a secret value left the core: {wire}"
    );
}

#[test]
fn variable_define_writes_one_row_and_keeps_the_others() {
    let state = state();
    state
        .store()
        .save_request(&request("r1", "{{base_url}}/{{customer_id}}"))
        .unwrap();
    // no environment at all: the scan still runs
    let names = |env: Option<&str>| {
        environment_unresolved_scan_impl(&state, "ws1".into(), env.map(Into::into))
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(names(None), ["base_url", "customer_id"]);

    use crate::app::{DefineVariableInput, VariableScope, environment_variable_define};
    let define = |owner: Option<&str>, key: &str, value: &str, secret: bool| {
        environment_variable_define(
            &state,
            DefineVariableInput {
                workspace_id: "ws1".into(),
                scope: VariableScope::Environment,
                owner_id: owner.map(Into::into),
                key: key.into(),
                value: value.into(),
                is_secret: secret,
            },
            "ui",
        )
    };
    // without an environment one named Local is created
    let made = define(None, "customer_id", "c_1", false).unwrap();
    assert!(made.created);
    let local = environment_list_impl(&state, "ws1".into()).unwrap();
    assert_eq!(local[0].name, "Local");
    assert_eq!(names(Some(&made.owner_id)), ["base_url"]);

    // an editor that read the environment before the definition cannot overwrite it
    save_env(
        &state,
        &made.owner_id,
        "Local",
        vec![
            row_of(&local[0].variables[0], "customer_id", Some("c_1")),
            row("token", Some(SECRET), true),
        ],
    )
    .unwrap();
    let before = environment_list_impl(&state, "ws1".into()).unwrap()[0].clone();
    define(Some(&made.owner_id), "base_url", "https://x", false).unwrap();
    let stale = SaveEnvironmentInput {
        base_revision: Some(before.revision.clone()),
        ..env_input(&made.owner_id, "ws1", "Local", vec![])
    };
    assert!(matches!(
        environment_save_impl(&state, stale),
        Err(AppError::Conflict { .. })
    ));
    // the other rows, the secret value included, are kept as stored
    let after = state
        .store()
        .list_environments("ws1")
        .unwrap()
        .remove(0)
        .variables;
    let keys: Vec<_> = after
        .iter()
        .map(|v| (v.key.as_str(), v.value.as_deref()))
        .collect();
    assert_eq!(
        keys,
        [
            ("customer_id", Some("c_1")),
            ("token", Some(SECRET)),
            ("base_url", Some("https://x"))
        ]
    );
    // Defining an existing secret without the flag replaces its value and keeps it a secret.
    define(Some(&made.owner_id), "token", "rotated", false).unwrap();
    let token = state
        .store()
        .list_environments("ws1")
        .unwrap()
        .remove(0)
        .variables
        .into_iter()
        .find(|v| v.key == "token")
        .unwrap();
    assert!(token.is_secret && token.enabled);
    assert_eq!(token.value.as_deref(), Some("rotated"));
    assert!(define(Some("env_gone"), "x", "1", false).is_err());
}

#[test]
fn compare_with_globals_falls_back_per_side_and_names_the_source() {
    let state = state();
    globals_save_impl(
        &state,
        GlobalsSaveInput {
            workspace_id: "ws1".into(),
            variables: vec![
                row("debug", Some("false"), false),
                row("only_g", Some("g"), false),
            ],
            base_revision: None,
        },
    )
    .unwrap();
    save_env(
        &state,
        "env_l",
        "Left",
        vec![row("tenant", Some("acme"), false)],
    )
    .unwrap();
    save_env(
        &state,
        "env_r",
        "Right",
        vec![row("debug", Some("true"), false)],
    )
    .unwrap();

    let rows = environment_compare_impl(
        &state,
        "ws1".into(),
        "env_l".into(),
        "env_r".into(),
        Some(true),
    )
    .unwrap();
    let summary: Vec<_> = rows
        .iter()
        .map(|r| (r.key.as_str(), r.state, r.left_global, r.right_global))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("tenant", CompareState::OnlyLeft, false, false),
            ("debug", CompareState::Differs, true, false),
            ("only_g", CompareState::Same, true, true),
        ]
    );
    assert_eq!(rows[1].left.as_deref(), Some("false"));
    // Without the flag the globals stay out, and the wire carries no source keys.
    let plain =
        environment_compare_impl(&state, "ws1".into(), "env_l".into(), "env_r".into(), None)
            .unwrap();
    assert_eq!(plain.len(), 2);
    assert!(!serde_json::to_string(&plain).unwrap().contains("_global"));
}

#[test]
fn guard_changed_at_is_stamped_only_when_a_guard_flag_changes() {
    let state = state();
    let created = save_env(
        &state,
        "env_p",
        "Production",
        vec![row("a", Some("1"), false)],
    )
    .unwrap();
    assert_eq!(created.guard_changed_at, None);

    let guard = |on: bool, base: &EnvironmentDto| {
        environment_save_impl(
            &state,
            SaveEnvironmentInput {
                confirm_before_send: Some(on),
                base_revision: Some(base.revision.clone()),
                ..env_input(
                    "env_p",
                    "ws1",
                    "Production",
                    vec![row_of(&base.variables[0], "a", Some("1"))],
                )
            },
        )
        .unwrap()
    };
    let on = guard(true, &created);
    let stamp = on
        .guard_changed_at
        .expect("turning the guard on stamps the time");
    let listed = environment_list_impl(&state, "ws1".into()).unwrap();
    assert_eq!(listed[0].guard_changed_at, Some(stamp));

    // A variable edit with the flags unchanged keeps the stamp.
    let edited = environment_save_impl(
        &state,
        SaveEnvironmentInput {
            base_revision: Some(on.revision.clone()),
            ..env_input(
                "env_p",
                "ws1",
                "Production",
                vec![row_of(&on.variables[0], "a", Some("2"))],
            )
        },
    )
    .unwrap();
    assert_eq!(edited.guard_changed_at, Some(stamp));
    assert!(
        guard(false, &edited)
            .guard_changed_at
            .is_some_and(|at| at >= stamp)
    );
}

#[test]
fn secret_hint_and_core_copy_never_hand_the_value_to_the_webview() {
    let state = state();
    let saved = save_env(
        &state,
        "env_a",
        "A",
        vec![
            row("token", Some("tok-1234567890-9f2a"), true),
            row("host", Some("api.local"), false),
        ],
    )
    .unwrap();
    let token = saved.variables.iter().find(|v| v.key == "token").unwrap();
    assert_eq!(token.secret_hint.as_deref(), Some("…9f2a"));
    assert!(token.value.is_none());
    let wire =
        serde_json::to_string(&environment_list_impl(&state, "ws1".into()).unwrap()).unwrap();
    assert!(!wire.contains("tok-1234567890") && wire.contains("…9f2a"));

    let copy = |ws: &str, env: Option<&str>, id: &str| {
        let mut got = None;
        let result = crate::app::environment_secret_copy_impl(&state, ws, env, id, |value| {
            got = Some(value.to_owned());
            Ok(())
        });
        (result, got)
    };
    let (result, got) = copy("ws1", Some("env_a"), &token.id);
    assert!(result.is_ok());
    assert_eq!(got.as_deref(), Some("tok-1234567890-9f2a"));

    // A webview id is not authority: another workspace, the globals and a plain variable refuse.
    let host = saved.variables.iter().find(|v| v.key == "host").unwrap();
    for (ws, env, id) in [
        ("ws2", Some("env_a"), token.id.as_str()),
        ("ws1", None, token.id.as_str()),
        ("ws1", Some("env_a"), host.id.as_str()),
    ] {
        let (result, got) = copy(ws, env, id);
        assert!(
            matches!(result, Err(AppError::NotFound { .. })),
            "{ws} {env:?} {id}"
        );
        assert!(got.is_none());
    }
}
