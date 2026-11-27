//! Tab-session tests: the version-1 corpus written by the React workbench, tolerance rules,
//! restore/merge projection and store round trips.

use std::{fs, path::Path};

use serde_json::{Value, json};

use super::*;
use crate::app::{AppState, workspace_create_impl};
use crate::error::AppError;

fn corpus() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/tab-session-v1");
    let mut files: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| {
            (
                path.file_stem().unwrap().to_string_lossy().into_owned(),
                fs::read_to_string(path).unwrap(),
            )
        })
        .collect();
    files.sort();
    files
}

fn snake(key: &str) -> String {
    key.chars().fold(String::new(), |mut out, c| {
        if c.is_ascii_uppercase() {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
        out
    })
}

fn request(id: &str, name: &str) -> Request {
    let mut request = Request::new(id, "ws_s", "Named", "https://x.test/a").unwrap();
    request.name = name.into();
    request
}

fn tab(tab_id: &str, request_id: Option<&str>, dirty: bool, draft: Option<Request>) -> SessionTab {
    SessionTab {
        tab_id: tab_id.into(),
        request_id: request_id.map(Into::into),
        history_entry_id: None,
        dirty,
        draft,
        editor_tab: None,
        preview: false,
        proxy_password_lost: false,
    }
}

fn session(active: Option<&str>, tabs: Vec<SessionTab>) -> TabSession {
    TabSession {
        version: TAB_SESSION_VERSION,
        active_tab_id: active.map(Into::into),
        tabs,
    }
}

fn ws() -> WorkspaceId {
    WorkspaceId::new("ws_s").unwrap()
}

#[test]
fn every_version_one_payload_written_by_the_workbench_parses_and_maps_field_by_field() {
    let corpus = corpus();
    assert!(corpus.len() >= 25, "corpus is missing: {}", corpus.len());
    for (name, text) in corpus {
        let v1: Value = serde_json::from_str(&text).unwrap();
        let parsed = tab_session_parse(&text).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(parsed.version, TAB_SESSION_VERSION, "{name}");
        assert_eq!(
            parsed.active_tab_id.as_deref(),
            v1["active"].as_str(),
            "{name}"
        );
        let v1_tabs = v1["tabs"].as_array().unwrap();
        assert_eq!(parsed.tabs.len(), v1_tabs.len(), "{name}");
        for (tab, v1_tab) in parsed.tabs.iter().zip(v1_tabs) {
            assert_eq!(
                Some(tab.tab_id.as_str()),
                v1_tab["tabId"].as_str(),
                "{name}"
            );
            assert_eq!(
                tab.request_id.as_deref(),
                v1_tab["requestId"].as_str(),
                "{name}"
            );
            assert_eq!(
                tab.history_entry_id.as_deref(),
                v1_tab["historyEntryId"].as_str(),
                "{name}"
            );
            assert_eq!(Some(tab.dirty), v1_tab["dirty"].as_bool(), "{name}");
            match (&tab.draft, v1_tab.get("draft")) {
                (None, None) => {}
                (Some(draft), Some(Value::Object(v1_draft))) => {
                    let renamed: Value = v1_draft
                        .iter()
                        .map(|(key, value)| (snake(key), value.clone()))
                        .collect::<serde_json::Map<_, _>>()
                        .into();
                    // `mode` and `ai` postdate the frozen v1 payloads: a v1 draft is an HTTP
                    // request, so it must read back with exactly their defaults.
                    let mut current = serde_json::to_value(draft).unwrap();
                    let fields = current.as_object_mut().unwrap();
                    assert_eq!(fields.remove("mode"), Some(Value::from("http")), "{name}");
                    assert_eq!(fields.remove("ai"), Some(Value::Null), "{name}");
                    assert_eq!(current, renamed, "{name}");
                }
                other => panic!("{name}: draft mismatch {other:?}"),
            }
        }
        // Re-serializing yields a current payload that reads back to the same session.
        let current = tab_session_serialize(&parsed).unwrap();
        assert_eq!(tab_session_parse(&current).unwrap(), parsed, "{name}");
        assert_eq!(
            serde_json::from_str::<Value>(&current).unwrap()["version"],
            2,
            "{name}"
        );
    }
}

#[test]
fn serialize_embeds_only_drafts_that_cannot_be_rebuilt() {
    let draft = request("req_a", "A");
    let text = tab_session_serialize(&session(
        None,
        vec![
            tab("clean", Some("req_a"), false, Some(draft.clone())),
            tab("dirty", Some("req_a"), true, Some(draft.clone())),
            tab("new", None, false, Some(draft)),
        ],
    ))
    .unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    let has_draft: Vec<bool> = value["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tab| tab.get("draft").is_some())
        .collect();
    assert_eq!(has_draft, [false, true, true]);
}

#[test]
fn serialize_rejects_a_foreign_version_and_parse_explains_newer_ones() {
    let mut current = session(None, vec![]);
    current.version = 1;
    assert!(matches!(
        tab_session_serialize(&current),
        Err(AppError::Validation { .. })
    ));
    let Err(AppError::Unsupported { message, .. }) =
        tab_session_parse(r#"{"version":3,"tabs":[]}"#)
    else {
        panic!("a newer version must be Unsupported");
    };
    assert!(message.contains("newer SpanBay"), "{message}");
}

#[test]
fn parse_rejects_what_the_workbench_rejected() {
    for text in [
        "not json",
        "null",
        "[]",
        "{}",
        r#"{"version":0,"tabs":[]}"#,
        r#"{"version":"1","tabs":[]}"#,
        r#"{"version":1}"#,
        r#"{"version":1,"tabs":{}}"#,
    ] {
        assert!(
            matches!(tab_session_parse(text), Err(AppError::Validation { .. })),
            "{text}"
        );
    }
}

#[test]
fn parse_skips_invalid_entries_and_fixes_bad_draft_fields_like_the_workbench() {
    let text = json!({
        "version": 1,
        "active": 5,
        "tabs": [
            {"tabId": "ok", "requestId": null, "dirty": false, "draft": {}},
            {"tabId": 1, "requestId": null, "dirty": false},
            {"tabId": "no-request-id", "dirty": false},
            {"tabId": "bad-request-id", "requestId": 3, "dirty": false},
            {"tabId": "bad-dirty", "requestId": null, "dirty": "yes"},
            {"tabId": "null-draft", "requestId": null, "dirty": true, "draft": null},
            {"tabId": "string-draft", "requestId": null, "dirty": true, "draft": "x"},
            "junk",
            {"tabId": "array-draft", "requestId": "r", "dirty": true, "draft": []},
            {"tabId": "messy", "requestId": null, "dirty": true, "historyEntryId": "h", "draft": {
                "id": 7, "workspaceId": "other", "collectionId": 1, "name": null, "protocol": "Smoke",
                "method": "Post", "url": 4, "queryParams": [{"key": "a", "value": "1", "enabled": true}, {"key": 1}],
                "pathVariables": "x", "headers": null, "auth": {"Nope": {}}, "body": {"Json": {"content": "{}"}},
                "networkOptions": {"timeout_ms": 5, "retry_count": "many", "disabled_tls_versions": 3, "extra": 1},
                "assertions": [{"StatusEquals": {"value": 200}}, "bad"], "description": false,
                "preRequestScript": "p", "testScript": 2, "position": 2.9
            }}
        ]
    })
    .to_string();
    let parsed = tab_session_parse(&text).unwrap();
    assert_eq!(parsed.active_tab_id, None);
    let ids: Vec<&str> = parsed.tabs.iter().map(|tab| tab.tab_id.as_str()).collect();
    assert_eq!(ids, ["ok", "array-draft", "messy"]);
    let draft = parsed.tabs[2].draft.as_ref().unwrap();
    assert_eq!(parsed.tabs[2].history_entry_id.as_deref(), Some("h"));
    assert_eq!(draft.id.as_str(), "");
    assert_eq!(draft.workspace_id.as_str(), "other");
    assert_eq!(draft.collection_id, None);
    assert_eq!(draft.name, "Untitled Request");
    assert_eq!(draft.protocol, ProtocolKind::HttpRest);
    assert_eq!(draft.method, HttpMethod::Post);
    assert_eq!(draft.url, "");
    assert_eq!(draft.query_params.len(), 1);
    assert!(draft.path_variables.is_empty() && draft.headers.is_empty());
    assert_eq!(draft.auth, AuthConfig::Inherit);
    assert_eq!(draft.network_options.timeout_ms, 5);
    assert_eq!(draft.network_options.retry_count, 0);
    assert!(draft.network_options.disabled_tls_versions.is_empty());
    assert_eq!(draft.assertions.len(), 1);
    assert_eq!(draft.description, "");
    assert_eq!(draft.pre_request_script, "p");
    assert_eq!(draft.test_script, "");
    assert_eq!(draft.position, 2);
    assert_eq!(
        parsed.tabs[0].draft.as_ref().unwrap().name,
        "Untitled Request"
    );
}

#[test]
fn restore_keeps_unsaved_edits_of_deleted_requests_and_drops_the_rest() {
    let mut edited = request("req_gone", "Edited");
    edited.workspace_id = WorkspaceId::new("elsewhere").unwrap();
    let mut blank = request("req_x", "No id");
    blank.id = serde_json::from_value(json!("")).unwrap();
    let mut history_tab = tab("hist", None, false, Some(request("req_h", "H")));
    history_tab.history_entry_id = Some("h1".into());
    let live = request("req_live", "Live");
    let input = session(
        Some("gone-clean"),
        vec![
            tab("gone-dirty", Some("req_gone"), true, Some(edited)),
            tab("gone-clean", Some("req_gone"), false, None),
            tab("gone-dirty-nodraft", Some("req_gone"), true, None),
            tab("live-clean", Some("req_live"), false, None),
            tab(
                "live-dirty",
                Some("req_live"),
                true,
                Some(request("req_live", "Mine")),
            ),
            tab("new-nodraft", None, true, None),
            tab("blank-id", None, true, Some(blank)),
            history_tab,
        ],
    );
    let restored = restore_tabs(&input, &ws(), std::slice::from_ref(&live)).unwrap();
    let ids: Vec<&str> = restored
        .tabs
        .iter()
        .map(|tab| tab.tab_id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["gone-dirty", "live-clean", "live-dirty", "blank-id", "hist"]
    );
    let orphan = &restored.tabs[0];
    assert_eq!((orphan.request_id.as_deref(), orphan.dirty), (None, true));
    assert_eq!(orphan.draft.workspace_id, ws());
    assert_eq!(restored.tabs[1].draft, live);
    assert!(!restored.tabs[1].dirty);
    assert_eq!(restored.tabs[2].draft.name, "Mine");
    assert!(restored.tabs[3].draft.id.as_str().starts_with("req_"));
    assert_eq!(restored.tabs[4].history_entry_id.as_deref(), Some("h1"));
    let dropped: Vec<(&str, DropReason)> = restored
        .dropped
        .iter()
        .map(|d| (d.tab_id.as_str(), d.reason))
        .collect();
    assert_eq!(
        dropped,
        [
            ("gone-clean", DropReason::RequestDeleted),
            ("gone-dirty-nodraft", DropReason::RequestDeleted),
            ("new-nodraft", DropReason::MissingDraft),
        ]
    );
    // The saved active tab was dropped: fall back to the first restored tab.
    assert_eq!(restored.active_tab_id.as_deref(), Some("gone-dirty"));
    let kept = restore_tabs(&session(Some("hist"), input.tabs.clone()), &ws(), &[live]).unwrap();
    assert_eq!(kept.active_tab_id.as_deref(), Some("hist"));
    assert_eq!(
        restore_tabs(&session(Some("x"), vec![]), &ws(), &[])
            .unwrap()
            .active_tab_id,
        None
    );
}

fn restored_tab(tab_id: &str, request_id: Option<&str>) -> RestoredTab {
    RestoredTab {
        tab_id: tab_id.into(),
        request_id: request_id.map(Into::into),
        history_entry_id: None,
        dirty: false,
        draft: request("req_d", "D"),
        editor_tab: None,
        preview: false,
        proxy_password_lost: false,
    }
}

#[test]
fn a_typed_proxy_password_is_never_stored_and_its_restored_tab_is_flagged() {
    let proxied = |id: &str, url: &str| {
        let mut draft = request(id, id);
        draft.network_options.proxy_url = Some(url.into());
        draft
    };
    let stored = tab_session_serialize(&session(
        None,
        vec![
            // A dirty saved request with a typed, unsaved password.
            tab(
                "t1",
                Some("req_a"),
                true,
                Some(proxied("req_a", "http://u:typed-pw@proxy:8080")),
            ),
            // A new tab: no saved request holds a password for its masked URL.
            tab(
                "t2",
                None,
                true,
                Some(proxied("new_1", "http://u:****@proxy:8080")),
            ),
            // A dirty saved request whose proxy password is the saved one.
            tab(
                "t3",
                Some("req_b"),
                true,
                Some(proxied("req_b", "http://u:****@proxy:8080")),
            ),
        ],
    ))
    .unwrap();
    assert!(!stored.contains("typed-pw"), "{stored}");
    let saved = [request("req_a", "A"), request("req_b", "B")];
    let restored = restore_tabs(&tab_session_parse(&stored).unwrap(), &ws(), &saved).unwrap();
    let summary: Vec<_> = restored
        .tabs
        .iter()
        .map(|tab| {
            (
                tab.tab_id.as_str(),
                tab.proxy_password_lost,
                tab.draft.network_options.proxy_url.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("t1", true, Some("http://u@proxy:8080")),
            ("t2", true, Some("http://u@proxy:8080")),
            ("t3", false, Some("http://u:****@proxy:8080")),
        ]
    );
}

#[test]
fn merge_skips_tabs_already_open_and_only_fills_a_missing_active_tab() {
    let restored = RestoredSession {
        tabs: vec![
            restored_tab("t1", Some("r1")),
            restored_tab("t2", Some("r2")),
            restored_tab("t3", None),
            restored_tab("t4", Some("r4")),
        ],
        active_tab_id: Some("t4".into()),
        dropped: vec![],
    };
    let open = || {
        vec![
            restored_tab("t1", Some("r9")),
            restored_tab("open", Some("r2")),
        ]
    };
    let merged = tab_session_merge(open(), None, restored.clone());
    let ids: Vec<&str> = merged.tabs.iter().map(|tab| tab.tab_id.as_str()).collect();
    assert_eq!(ids, ["t1", "open", "t3", "t4"]);
    assert_eq!(merged.active_tab_id.as_deref(), Some("t4"));
    let reasons: Vec<_> = merged
        .dropped
        .iter()
        .map(|d| (&*d.tab_id, d.reason))
        .collect();
    assert_eq!(
        reasons,
        [
            ("t1", DropReason::AlreadyOpen),
            ("t2", DropReason::AlreadyOpen)
        ]
    );
    let kept = tab_session_merge(open(), Some("open".into()), restored.clone());
    assert_eq!(kept.active_tab_id.as_deref(), Some("open"));
    let mut elsewhere = restored;
    elsewhere.active_tab_id = Some("t1".into());
    assert_eq!(
        tab_session_merge(vec![], None, elsewhere)
            .active_tab_id
            .as_deref(),
        Some("t1")
    );
}

fn state_with_request() -> AppState {
    let state = AppState::open_in_memory().unwrap();
    workspace_create_impl(&state, "ws_s".into(), "S".into()).unwrap();
    state
        .store()
        .save_request(&request("req_live", "Live"))
        .unwrap();
    state
}

#[test]
fn save_and_load_round_trip_through_the_store() {
    let state = state_with_request();
    assert_eq!(tab_session_load(&state, "ws_s").unwrap(), None);
    let draft = request("req_live", "Edited");
    tab_session_save(
        &state,
        "ws_s",
        &session(
            Some("a"),
            vec![
                tab("a", Some("req_live"), true, Some(draft.clone())),
                tab("b", Some("req_live"), false, None),
                tab("c", Some("req_deleted"), false, None),
            ],
        ),
    )
    .unwrap();
    let loaded = tab_session_load(&state, "ws_s").unwrap().unwrap();
    assert_eq!(loaded.active_tab_id.as_deref(), Some("a"));
    assert_eq!(loaded.tabs[0].draft.name, "Edited");
    assert_eq!(loaded.tabs[1].draft.name, "Live");
    assert_eq!(loaded.dropped.len(), 1);
    let stored = load_tab_session_impl(&state, "ws_s".into())
        .unwrap()
        .unwrap();
    assert!(stored.contains(r#""version":2"#));
    assert!(matches!(
        tab_session_save(&state, "ws_missing", &session(None, vec![])),
        Err(AppError::NotFound { .. })
    ));
}

#[test]
fn load_reads_a_payload_the_opaque_command_stored_and_rejects_unreadable_ones() {
    let state = state_with_request();
    let (_, text) = corpus()
        .into_iter()
        .find(|(name, _)| name == "clean-saved")
        .unwrap();
    // The fixture's request id is not in this store, so the clean tab is dropped.
    save_tab_session_impl(&state, "ws_s".into(), text).unwrap();
    let loaded = tab_session_load(&state, "ws_s").unwrap().unwrap();
    assert!(loaded.tabs.is_empty());
    assert_eq!(loaded.dropped.len(), 1);
    save_tab_session_impl(&state, "ws_s".into(), String::new()).unwrap();
    assert_eq!(tab_session_load(&state, "ws_s").unwrap(), None);
    save_tab_session_impl(&state, "ws_s".into(), r#"{"version":9,"tabs":[]}"#.into()).unwrap();
    assert!(matches!(
        tab_session_load(&state, "ws_s"),
        Err(AppError::Unsupported { .. })
    ));
}

#[test]
fn parse_refuses_a_payload_over_the_storage_limit_before_reading_it() {
    let text = format!(
        r#"{{"version":2,"tabs":[],"pad":"{}"}}"#,
        "x".repeat(crate::workspace::MAX_TAB_SESSION_BYTES)
    );
    let Err(AppError::Validation { message, .. }) = tab_session_parse(&text) else {
        panic!("an oversized payload must be a Validation error");
    };
    assert!(message.contains("byte limit"), "{message}");
}

#[test]
fn a_clean_tab_is_rebuilt_from_the_stored_request_not_from_its_stale_draft() {
    let live = request("req_live", "Stored name");
    let stale = request("req_live", "Stale draft");
    let input = session(
        None,
        vec![
            tab("clean", Some("req_live"), false, Some(stale.clone())),
            tab("dirty", Some("req_live"), true, Some(stale)),
        ],
    );
    let restored = restore_tabs(&input, &ws(), &[live]).unwrap();
    assert_eq!(restored.tabs[0].draft.name, "Stored name");
    assert_eq!(restored.tabs[1].draft.name, "Stale draft");
}

#[test]
fn restored_drafts_get_a_name_and_an_id_when_blank_or_whitespace() {
    let mut blank = request("req_x", "  ");
    blank.id = serde_json::from_value(json!("   ")).unwrap();
    let input = session(None, vec![tab("t", None, true, Some(blank))]);
    let restored = restore_tabs(&input, &ws(), &[]).unwrap();
    let draft = &restored.tabs[0].draft;
    assert_eq!(draft.name, "Untitled Request");
    assert!(draft.id.as_str().starts_with("req_"), "{:?}", draft.id);
    assert_eq!(draft.workspace_id, ws());
}

#[test]
fn merge_drops_duplicates_inside_the_restored_list_and_always_picks_an_active_tab() {
    let restored = RestoredSession {
        tabs: vec![
            restored_tab("t1", Some("r1")),
            restored_tab("t1", Some("r2")),
            restored_tab("t2", Some("r1")),
            restored_tab("t3", None),
        ],
        active_tab_id: Some("gone".into()),
        dropped: vec![],
    };
    let merged = tab_session_merge(vec![], None, restored.clone());
    let ids: Vec<&str> = merged.tabs.iter().map(|tab| tab.tab_id.as_str()).collect();
    assert_eq!(ids, ["t1", "t3"]);
    let dropped: Vec<_> = merged.dropped.iter().map(|d| &*d.tab_id).collect();
    assert_eq!(dropped, ["t1", "t2"]);
    assert_eq!(merged.active_tab_id.as_deref(), Some("t1"));
    // A stale active id from the shell does not leave tabs without an active one.
    let stale = tab_session_merge(vec![], Some("stale".into()), restored);
    assert_eq!(stale.active_tab_id.as_deref(), Some("t1"));
    assert_eq!(
        tab_session_merge(
            vec![],
            None,
            RestoredSession {
                tabs: vec![],
                active_tab_id: None,
                dropped: vec![],
            }
        )
        .active_tab_id,
        None
    );
}

#[test]
fn legacy_network_options_keep_valid_keys_and_drop_invalid_ones_at_any_size() {
    let mut options = serde_json::Map::new();
    for index in 0..3000 {
        options.insert(format!("unknown_{index}"), json!(index));
    }
    options.insert("timeout_ms".into(), json!(7));
    options.insert("retry_count".into(), json!("many"));
    options.insert("follow_redirects".into(), json!(false));
    let draft = json!({ "networkOptions": options });
    let parsed = legacy_draft(draft.as_object().unwrap()).unwrap();
    assert_eq!(parsed.network_options.timeout_ms, 7);
    assert_eq!(parsed.network_options.retry_count, 0);
    assert!(!parsed.network_options.follow_redirects);
}

fn stored_v2(tab_extras: Value) -> String {
    let mut entry = json!({
        "tab_id": "a",
        "request_id": null,
        "history_entry_id": null,
        "dirty": false,
        "draft": request("req_a", "A"),
    });
    entry
        .as_object_mut()
        .unwrap()
        .extend(tab_extras.as_object().unwrap().clone());
    json!({ "version": 2, "active_tab_id": "a", "tabs": [entry] }).to_string()
}

#[test]
fn a_session_saved_before_editor_tab_and_preview_existed_still_restores() {
    let parsed = tab_session_parse(&stored_v2(json!({}))).unwrap();
    assert_eq!(parsed.tabs.len(), 1);
    assert_eq!(parsed.tabs[0].editor_tab, None);
    assert!(!parsed.tabs[0].preview);
    let restored = restore_tabs(&parsed, &ws(), &[]).unwrap();
    assert_eq!(restored.tabs[0].editor_tab, None);
    assert!(!restored.tabs[0].preview);
}

#[test]
fn editor_tab_and_preview_round_trip_through_the_stored_text_and_the_restore() {
    let mut shown = tab("a", None, false, Some(request("req_a", "A")));
    shown.editor_tab = Some(EditorTab::RawJson);
    shown.preview = true;
    let text = tab_session_serialize(&session(Some("a"), vec![shown.clone()])).unwrap();
    assert!(text.contains(r#""editor_tab":"raw_json""#), "{text}");
    assert!(text.contains(r#""preview":true"#), "{text}");
    let parsed = tab_session_parse(&text).unwrap();
    assert_eq!(parsed.tabs, vec![shown]);
    let restored = restore_tabs(&parsed, &ws(), &[]).unwrap();
    assert_eq!(restored.tabs[0].editor_tab, Some(EditorTab::RawJson));
    assert!(restored.tabs[0].preview);
    // The restored tab goes back to the core as is (merge) and keeps both fields.
    let echoed: RestoredTab =
        serde_json::from_value(serde_json::to_value(&restored.tabs[0]).unwrap()).unwrap();
    assert_eq!(echoed, restored.tabs[0]);

    // Defaults are not written, so the stored text stays as small as before.
    let plain_tab = tab("b", None, false, Some(request("req_b", "B")));
    let plain = tab_session_serialize(&session(None, vec![plain_tab])).unwrap();
    assert!(
        !plain.contains("editor_tab") && !plain.contains("preview"),
        "{plain}"
    );
}

#[test]
fn an_unknown_editor_tab_or_a_malformed_preview_reads_as_the_default_and_keeps_the_tab() {
    for extras in [
        json!({ "editor_tab": "telemetry", "preview": "yes" }),
        json!({ "editor_tab": 3, "preview": null }),
        json!({ "editor_tab": null }),
    ] {
        let parsed = tab_session_parse(&stored_v2(extras.clone())).unwrap();
        assert_eq!(parsed.tabs.len(), 1, "{extras}");
        assert_eq!(parsed.tabs[0].editor_tab, None, "{extras}");
        assert!(!parsed.tabs[0].preview, "{extras}");
    }
    let known = tab_session_parse(&stored_v2(
        json!({ "editor_tab": "params", "preview": true }),
    ))
    .unwrap();
    assert_eq!(known.tabs[0].editor_tab, Some(EditorTab::Params));
    assert!(known.tabs[0].preview);
}
