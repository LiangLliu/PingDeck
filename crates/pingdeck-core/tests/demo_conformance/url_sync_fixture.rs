//! Pins the request model helpers: `fixtures/url-sync-cases.json` (shared with the TypeScript
//! copy of the URL sync) for the pure functions, and in-memory app tests for drafts and
//! effective auth.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test helpers

use pingdeck_core::{
    app::{
        AppState, collection_save_impl, folder_save_impl, request_model_service as model,
        request_save_impl, workspace_create_impl,
    },
    error::AppError,
    model::settings::NetworkSettings,
    model::{
        Request,
        models::{
            AuthConfig, HttpMethod, KeyValue, NetworkField, NetworkOptions, ProtocolKind,
            RequestBody,
        },
    },
};
use serde_json::{Value, json};

fn cases(section: &str) -> Vec<Value> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/url-sync-cases.json");
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    fixture[section].as_array().unwrap().clone()
}

fn rows(value: &Value) -> Vec<KeyValue> {
    serde_json::from_value(value.clone()).unwrap()
}

fn text(case: &Value, key: &str) -> String {
    case[key].as_str().unwrap().to_owned()
}

fn row(key: &str, value: &str, enabled: bool) -> KeyValue {
    KeyValue {
        key: key.to_owned(),
        value: value.to_owned(),
        enabled,
        description: None,
    }
}

#[test]
fn url_with_query_params_matches_the_fixture() {
    for case in cases("url_with_query_params") {
        let actual = model::url_with_query_params(&text(&case, "url"), &rows(&case["params"]));
        assert_eq!(json!(actual), case["expected"], "{}", case["name"]);
    }
}

#[test]
fn query_params_from_url_matches_the_fixture() {
    for case in cases("query_params_from_url") {
        let actual = model::query_params_from_url(&text(&case, "url"));
        assert_eq!(json!(actual), case["expected"], "{}", case["name"]);
    }
}

#[test]
fn path_variable_names_match_the_fixture() {
    for case in cases("path_variable_names") {
        let actual = model::path_variable_names(&text(&case, "url"));
        assert_eq!(json!(actual), case["expected"], "{}", case["name"]);
    }
}

#[test]
fn merge_path_variables_matches_the_fixture() {
    for case in cases("merge_path_variables") {
        let actual = model::merge_path_variables(&text(&case, "url"), &rows(&case["existing"]));
        assert_eq!(json!(actual), case["expected"], "{}", case["name"]);
    }
}

#[test]
fn sync_request_url_matches_the_fixture() {
    for case in cases("sync_request_url") {
        let input: model::UrlSyncInput = serde_json::from_value(case["input"].clone()).unwrap();
        assert_eq!(
            json!(model::sync_request_url(input)),
            case["expected"],
            "{}",
            case["name"]
        );
    }
}

/// Whatever the rows hold survives a trip through the URL text.
#[test]
fn rows_round_trip_through_the_url() {
    for case in cases("url_with_query_params") {
        let params = rows(&case["params"]);
        let url = model::url_with_query_params(&text(&case, "url"), &params);
        let expected: Vec<KeyValue> = params
            .into_iter()
            .filter(|row| row.enabled && !row.key.trim().is_empty())
            .collect();
        assert_eq!(
            model::query_params_from_url(&url),
            expected,
            "{}",
            case["name"]
        );
    }
}

/// Replaces a generated `req_<uuid>` id with `<new>` so a fresh id can be compared.
fn mask_new_id(mut request: Value) -> Value {
    let generated = request["id"]
        .as_str()
        .is_some_and(|id| id.starts_with("req_") && id.len() == "req_".len() + 36);
    if generated {
        request["id"] = json!("<new>");
    }
    request
}

#[test]
fn new_request_drafts_match_the_fixture() {
    for case in cases("new_request_draft") {
        let overrides: Option<model::RequestDraftOverrides> =
            serde_json::from_value(case["overrides"].clone()).unwrap();
        let template: model::RequestTemplate =
            serde_json::from_value(case["template"].clone()).unwrap();
        let draft = model::request_new_draft(
            &NetworkOptions::from(&NetworkSettings::default()),
            &text(&case, "workspace_id"),
            case["collection_id"].as_str(),
            case["folder_id"].as_str(),
            template,
            overrides,
        )
        .unwrap();
        assert_eq!(
            mask_new_id(json!(draft)),
            case["expected"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn draft_normalize_matches_the_fixture() {
    for case in cases("draft_normalize") {
        let request: Request = serde_json::from_value(case["request"].clone()).unwrap();
        let normalized = model::draft_normalize(request, &text(&case, "workspace_id")).unwrap();
        assert_eq!(
            mask_new_id(json!(normalized)),
            case["expected"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn the_default_network_settings_are_the_core_default_options_followed_from_settings() {
    assert_eq!(
        NetworkOptions::from(&NetworkSettings::default()),
        NetworkOptions {
            inherit: NetworkField::ALL.to_vec(),
            ..NetworkOptions::default()
        }
    );
}

#[test]
fn new_drafts_start_with_the_given_network_options() {
    let options = NetworkOptions {
        timeout_ms: 1234,
        ..NetworkOptions::default()
    };
    let draft = model::request_new_draft(
        &options,
        "ws",
        None,
        None,
        model::RequestTemplate::HttpGet,
        None,
    )
    .unwrap();
    assert_eq!(draft.network_options, options);
    let from_history =
        model::request_draft_from_history(&options, history_entry(None), "ws", None).unwrap();
    assert_eq!(from_history.network_options, options);
}

#[test]
fn new_drafts_follow_their_template() {
    let get = model::request_new_draft(
        &NetworkOptions::default(),
        "ws",
        Some("col"),
        Some("fold"),
        model::RequestTemplate::HttpGet,
        None,
    )
    .unwrap();
    assert!(get.id.as_str().starts_with("req_"));
    assert_eq!(
        (get.name.as_str(), get.url.as_str(), get.method),
        ("Untitled Request", "{{baseUrl}}/path", HttpMethod::Get)
    );
    assert_eq!(
        get.collection_id.as_ref().map(|id| id.as_str()),
        Some("col")
    );
    assert_eq!(get.folder_id.as_ref().map(|id| id.as_str()), Some("fold"));
    assert_eq!(get.headers, vec![row("Accept", "application/json", true)]);
    assert_ne!(
        get.id,
        model::request_new_draft(
            &NetworkOptions::default(),
            "ws",
            None,
            None,
            model::RequestTemplate::HttpGet,
            None
        )
        .unwrap()
        .id
    );

    let post = model::request_new_draft(
        &NetworkOptions::default(),
        "ws",
        None,
        None,
        model::RequestTemplate::HttpPostJson,
        None,
    )
    .unwrap();
    assert_eq!(
        (post.name.as_str(), post.method),
        ("Create resource", HttpMethod::Post)
    );
    assert_eq!(
        post.headers,
        vec![row("Content-Type", "application/json", true)]
    );
    assert_eq!(
        post.body,
        RequestBody::Json {
            content: "{\n  \"name\": \"example\"\n}".to_owned()
        }
    );

    let graphql = model::request_new_draft(
        &NetworkOptions::default(),
        "ws",
        None,
        None,
        model::RequestTemplate::Graphql,
        None,
    )
    .unwrap();
    assert_eq!(graphql.protocol, ProtocolKind::Graphql);
    assert_eq!(graphql.url, "{{baseUrl}}/graphql");
    assert!(graphql.headers.is_empty());

    assert!(matches!(
        model::request_new_draft(
            &NetworkOptions::default(),
            " ",
            None,
            None,
            model::RequestTemplate::HttpGet,
            None
        ),
        Err(AppError::Validation { .. })
    ));
}

#[test]
fn overrides_derive_name_rows_and_variables_from_the_url() {
    let overrides = model::RequestDraftOverrides {
        method: Some(HttpMethod::Post),
        url: Some("http://127.0.0.1:3000/users/:id?limit=5".to_owned()),
        body: Some(RequestBody::RawText {
            language: None,
            content: "hello".to_owned(),
        }),
        ..Default::default()
    };
    let draft = model::request_new_draft(
        &NetworkOptions::default(),
        "ws",
        None,
        None,
        model::RequestTemplate::HttpGet,
        Some(overrides),
    )
    .unwrap();
    assert_eq!(draft.name, "http://127.0.0.1:3000/users/:id?limit=5");
    assert_eq!(draft.method, HttpMethod::Post);
    assert_eq!(draft.query_params, vec![row("limit", "5", true)]);
    assert_eq!(draft.path_variables, vec![row("id", "", true)]);
    assert_eq!(
        draft.body,
        RequestBody::RawText {
            language: None,
            content: "hello".to_owned()
        }
    );

    let named = model::RequestDraftOverrides {
        name: Some("Mine".to_owned()),
        url: Some("   ".to_owned()),
        path_variables: Some(vec![row("x", "1", false)]),
        ..Default::default()
    };
    let draft = model::request_new_draft(
        &NetworkOptions::default(),
        "ws",
        None,
        None,
        model::RequestTemplate::HttpGet,
        Some(named),
    )
    .unwrap();
    assert_eq!((draft.name.as_str(), draft.url.as_str()), ("Mine", "   "));
    assert_eq!(draft.path_variables, vec![row("x", "1", false)]);
}

#[test]
fn normalize_assigns_workspace_id_and_name() {
    let mut request = model::request_new_draft(
        &NetworkOptions::default(),
        "old",
        None,
        None,
        model::RequestTemplate::HttpGet,
        None,
    )
    .unwrap();
    request.name = "  ".to_owned();
    let normalized = model::draft_normalize(request, "new").unwrap();
    assert_eq!(normalized.workspace_id.as_str(), "new");
    assert_eq!(normalized.name, "Untitled Request");
    assert!(matches!(
        model::draft_normalize(normalized, ""),
        Err(AppError::Validation { .. })
    ));
}

fn history_entry(snapshot: Option<&Request>) -> pingdeck_core::model::ExecutionHistoryEntry {
    serde_json::from_value(json!({
        "id": "h1", "workspace_id": "ws", "request_id": null, "environment_id": null,
        "result_kind": "Success", "request_name": "From history", "method": "Put",
        "request_url": "https://x/users/:id?a=1", "status_code": 200, "status_text": "OK",
        "final_url": "https://x/users/1", "elapsed_ms": 5, "body_size_bytes": 0, "attempt_count": 1,
        "response_body_preview": "", "response_body_truncated": false, "error_kind": null,
        "error_message": null, "executed_at": "2026-01-01T00:00:00Z", "request_snapshot": snapshot,
    }))
    .unwrap()
}

#[test]
fn history_entries_reopen_as_unsaved_drafts() {
    let rebuilt = model::request_draft_from_history(
        &NetworkOptions::default(),
        history_entry(None),
        "ws2",
        None,
    )
    .unwrap();
    assert_eq!(rebuilt.name, "From history");
    assert_eq!(rebuilt.method, HttpMethod::Put);
    assert_eq!(rebuilt.url, "https://x/users/:id?a=1");
    assert_eq!(rebuilt.query_params, vec![row("a", "1", true)]);
    assert_eq!(rebuilt.path_variables, vec![row("id", "", true)]);
    assert_eq!(rebuilt.workspace_id.as_str(), "ws2");
    assert!(rebuilt.collection_id.is_none() && rebuilt.folder_id.is_none());

    let mut saved = model::request_new_draft(
        &NetworkOptions::default(),
        "ws",
        Some("col"),
        None,
        model::RequestTemplate::Graphql,
        None,
    )
    .unwrap();
    saved.url = "https://snapshot/graphql".to_owned();
    let from_snapshot = model::request_draft_from_history(
        &NetworkOptions::default(),
        history_entry(Some(&saved)),
        "ws2",
        None,
    )
    .unwrap();
    assert_eq!(from_snapshot.url, "https://snapshot/graphql");
    assert_eq!(from_snapshot.protocol, ProtocolKind::Graphql);
    assert_eq!(from_snapshot.name, "From history");
    assert_ne!(from_snapshot.id, saved.id);
    assert!(from_snapshot.collection_id.is_none());
}

fn save<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}

fn bearer(token: &str) -> AuthConfig {
    AuthConfig::Bearer {
        token_ref: token.to_owned(),
    }
}

fn effective(state: &AppState, input: Value) -> Result<model::EffectiveAuth, AppError> {
    model::request_effective_auth(state, save(input))
}

#[test]
fn effective_auth_walks_folders_then_collection() {
    let state = AppState::open_in_memory().unwrap();
    workspace_create_impl(&state, "ws".into(), "WS".into()).unwrap();
    collection_save_impl(
        &state,
        save(json!({
            "id": "col", "workspace_id": "ws", "name": "API", "auth": bearer("{{col}}"),
        })),
    )
    .unwrap();
    collection_save_impl(
        &state,
        save(json!({
            "id": "col_inherit", "workspace_id": "ws", "name": "Plain",
        })),
    )
    .unwrap();
    let folder = |id: &str, parent: Option<&str>, auth: AuthConfig| {
        folder_save_impl(&state, save(json!({
            "id": id, "collection_id": "col", "parent_folder_id": parent, "name": id, "auth": auth,
        }))).unwrap();
    };
    folder("root", None, bearer("literal-secret"));
    folder("mid", Some("root"), AuthConfig::Inherit);
    folder("leaf", Some("mid"), AuthConfig::Inherit);
    folder("plain", None, AuthConfig::Inherit);

    let ctx = |collection: Option<&str>, folder: Option<&str>, auth: Value| json!({ "workspace_id": "ws", "collection_id": collection, "folder_id": folder, "auth": auth });

    let from_folder = effective(&state, ctx(Some("col"), Some("leaf"), json!("Inherit"))).unwrap();
    assert_eq!(from_folder.config, bearer("literal-secret"));
    assert_eq!(
        from_folder.source,
        model::AuthSource::Folder {
            id: "root".into(),
            name: "root".into()
        }
    );
    let preview = from_folder.preview.unwrap();
    assert_eq!(
        (preview.name.as_str(), preview.value.as_str()),
        ("Authorization", "Bearer ••••••")
    );

    // The collection is found through the folder when the draft names only the folder.
    let via_folder = effective(&state, ctx(None, Some("plain"), Value::Null)).unwrap();
    assert_eq!(
        via_folder.source,
        model::AuthSource::Collection {
            id: "col".into(),
            name: "API".into()
        }
    );
    assert_eq!(via_folder.preview.unwrap().value, "Bearer {{col}}");

    let own = effective(
        &state,
        ctx(
            Some("col"),
            Some("leaf"),
            json!({"ApiKey": {"key": "", "value_ref": "", "location": "Query"}}),
        ),
    )
    .unwrap();
    assert_eq!(own.source, model::AuthSource::Request);
    let preview = own.preview.unwrap();
    assert_eq!(
        (
            preview.location,
            preview.name.as_str(),
            preview.value.as_str()
        ),
        (model::AuthPreviewLocation::Query, "api_key", "{{apiKey}}")
    );

    let inherit_collection =
        effective(&state, ctx(Some("col_inherit"), None, Value::Null)).unwrap();
    assert_eq!(inherit_collection.config, AuthConfig::None);
    assert_eq!(
        inherit_collection.source,
        model::AuthSource::Collection {
            id: "col_inherit".into(),
            name: "Plain".into()
        }
    );
    assert!(inherit_collection.preview.is_none());

    let orphan = effective(&state, ctx(None, None, Value::Null)).unwrap();
    assert_eq!(
        (orphan.config, orphan.source),
        (AuthConfig::None, model::AuthSource::None)
    );
    let missing = effective(&state, ctx(Some("gone"), Some("gone"), Value::Null)).unwrap();
    assert_eq!(missing.source, model::AuthSource::None);
}

#[test]
fn effective_auth_of_a_saved_request_uses_the_stored_request() {
    let state = AppState::open_in_memory().unwrap();
    workspace_create_impl(&state, "ws".into(), "WS".into()).unwrap();
    collection_save_impl(
        &state,
        save(json!({
            "id": "col", "workspace_id": "ws", "name": "API", "auth": bearer("{{col}}"),
        })),
    )
    .unwrap();
    let draft = model::request_new_draft(
        &NetworkOptions::default(),
        "ws",
        Some("col"),
        None,
        model::RequestTemplate::HttpGet,
        None,
    )
    .unwrap();
    let saved = request_save_impl(&state, save(serde_json::to_value(&draft).unwrap())).unwrap();

    // Draft fields are ignored when a request id is given.
    let resolved = effective(
        &state,
        json!({
            "workspace_id": "ws", "request_id": saved.id.as_str(), "auth": bearer("ignored"),
        }),
    )
    .unwrap();
    assert_eq!(
        resolved.source,
        model::AuthSource::Collection {
            id: "col".into(),
            name: "API".into()
        }
    );

    for (workspace, request) in [("ws", "nope"), ("other", saved.id.as_str())] {
        assert!(matches!(
            effective(
                &state,
                json!({ "workspace_id": workspace, "request_id": request })
            ),
            Err(AppError::NotFound { .. })
        ));
    }
}
