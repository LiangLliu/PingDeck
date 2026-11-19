//! Settings use-case tests: persistence, events, search-key secrets and side effects.

use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::app::{
    AppState, SettingsSecret, settings_get_impl, settings_reset_all_impl,
    settings_reset_section_impl, settings_secret_set_impl, settings_subscribe_impl,
    settings_unsubscribe_impl, settings_update_impl,
};
use crate::error::AppError;
use crate::events::EventSink;
use crate::secrets::{SEARCH_BRAVE, SEARCH_TAVILY};
use crate::settings::{SearchBackend, Settings, SettingsEvent, SettingsSection};

#[test]
fn updates_persist_across_reopen_and_reject_bad_values_without_changes() {
    let dir = std::env::temp_dir().join(format!("pd-settings-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("pingdeck.sqlite3");
    {
        let state = AppState::open(&path).expect("opens");
        settings_update_impl(&state, SettingsSection::Mock, &json!({"first_port": 4100}))
            .expect("valid update");
        let error = settings_update_impl(&state, SettingsSection::Mock, &json!({"first_port": 1}))
            .expect_err("invalid update");
        assert!(matches!(error, AppError::Validation { .. }));
        assert_eq!(state.settings().mock.first_port, 4100);
    }
    let reopened = AppState::open(&path).expect("reopens");
    assert_eq!(reopened.settings().mock.first_port, 4100);
    drop(reopened);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn subscribers_hear_each_change_until_they_unsubscribe() {
    let state = AppState::open_in_memory().expect("state opens");
    let heard: Arc<Mutex<Vec<SettingsSection>>> = Arc::default();
    let sink = {
        let heard = Arc::clone(&heard);
        Arc::new(move |event: SettingsEvent| {
            let SettingsEvent::Changed { section, .. } = event;
            heard.lock().expect("lock").push(section);
            true
        }) as Arc<dyn EventSink<SettingsEvent>>
    };
    let id = settings_subscribe_impl(&state, sink).expect("subscribes");
    settings_update_impl(&state, SettingsSection::Editor, &json!({"tab_size": 4})).expect("ok");
    settings_reset_section_impl(&state, SettingsSection::Editor, None).expect("ok");
    assert!(settings_unsubscribe_impl(&state, id));
    settings_update_impl(&state, SettingsSection::Editor, &json!({"tab_size": 3})).expect("ok");
    assert_eq!(
        *heard.lock().expect("lock"),
        vec![SettingsSection::Editor, SettingsSection::Editor]
    );
}

#[test]
fn search_keys_are_stored_as_secrets_and_only_their_hints_are_settings() {
    let state = AppState::open_in_memory().expect("state opens");
    let heard: Arc<Mutex<Vec<Settings>>> = Arc::default();
    let sink = {
        let heard = Arc::clone(&heard);
        Arc::new(move |event: SettingsEvent| {
            let SettingsEvent::Changed { settings, .. } = event;
            heard.lock().expect("lock").push(*settings);
            true
        }) as Arc<dyn EventSink<SettingsEvent>>
    };
    settings_subscribe_impl(&state, sink).expect("subscribes");

    let key = "tvly-abcdefghijklmnop";
    let hint =
        settings_secret_set_impl(&state, SettingsSecret::SearchTavily, &format!("  {key}\n"))
            .expect("stores");
    assert!(hint.has_secret && hint.hint == "…mnop");
    let after = settings_get_impl(&state);
    assert_eq!(after.ai.search.tavily_key_hint, "…mnop");
    assert_eq!(after.ai.search.brave_key_hint, "");
    assert_eq!(
        state.secrets().get(SEARCH_TAVILY).expect("read"),
        Some(key.to_owned())
    );
    assert_eq!(state.settings().ai.search.tavily_key_hint, "…mnop");
    let events = heard.lock().expect("lock");
    assert_eq!(events.len(), 1, "subscribers hear the hint change");
    assert_eq!(events[0].ai.search.tavily_key_hint, "…mnop");
    assert!(
        !serde_json::to_string(&events[0])
            .expect("json")
            .contains(key)
    );
    drop(events);

    settings_secret_set_impl(&state, SettingsSecret::SearchBrave, "short").expect("stores");
    assert_eq!(state.settings().ai.search.brave_key_hint, "••••");
    assert_eq!(
        state.secrets().get(SEARCH_BRAVE).expect("read"),
        Some("short".to_owned())
    );

    // An empty value deletes the secret and clears its hint.
    let gone =
        settings_secret_set_impl(&state, SettingsSecret::SearchTavily, "  ").expect("deletes");
    assert!(!gone.has_secret && gone.hint.is_empty());
    let cleared = settings_get_impl(&state);
    assert_eq!(cleared.ai.search.tavily_key_hint, "");
    assert_eq!(state.secrets().get(SEARCH_TAVILY).expect("read"), None);
    assert_eq!(cleared.ai.search.brave_key_hint, "••••");

    for bad in ["a\nb", "a\0b", &"x".repeat(4097)] {
        let error = settings_secret_set_impl(&state, SettingsSecret::SearchBrave, bad)
            .expect_err("rejected");
        assert!(matches!(error, AppError::Validation { .. }), "{error:?}");
    }
    assert_eq!(state.settings().ai.search.brave_key_hint, "••••");

    // The hints are not settings anyone may write.
    let error = settings_update_impl(
        &state,
        SettingsSection::Ai,
        &json!({"search": {"brave_key_hint": "forged"}}),
    )
    .expect_err("read-only");
    assert!(error.message().contains("ai.search.brave_key_hint"));
}

#[test]
fn a_reset_ai_section_keeps_the_stored_keys_and_their_hints() {
    let state = AppState::open_in_memory().expect("state opens");
    settings_secret_set_impl(
        &state,
        SettingsSecret::SearchTavily,
        "tvly-abcdefghijklmnop",
    )
    .expect("stores");
    settings_update_impl(
        &state,
        SettingsSection::Ai,
        &json!({"agent": {"max_tool_iterations": 7}, "search": {"backend": "tavily"}}),
    )
    .expect("update");
    let reset = settings_reset_section_impl(&state, SettingsSection::Ai, None).expect("resets");
    assert_eq!(reset.ai.agent.max_tool_iterations, 20);
    assert_eq!(reset.ai.search.backend, SearchBackend::Auto);
    assert_eq!(reset.ai.search.tavily_key_hint, "…mnop");
    assert_eq!(
        state.secrets().get(SEARCH_TAVILY).expect("read"),
        Some("tvly-abcdefghijklmnop".to_owned())
    );
    // The hint survives a reopen: it was persisted with the section.
    assert_eq!(state.settings().ai.search.tavily_key_hint, "…mnop");
}

#[test]
fn the_ai_section_persists_with_its_hints_across_a_reopen() {
    let dir = std::env::temp_dir().join(format!("pd-settings-ai-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("pingdeck.sqlite3");
    {
        let state = AppState::open(&path).expect("opens");
        settings_secret_set_impl(&state, SettingsSecret::SearchBrave, "brave-abcdefghijkl")
            .expect("stores");
        settings_update_impl(
            &state,
            SettingsSection::Ai,
            &json!({"gateway": {"port": 4300}, "agent": {"max_concurrent_runs": 2}}),
        )
        .expect("update");
    }
    let reopened = AppState::open(&path).expect("reopens");
    let ai = reopened.settings().ai.clone();
    assert_eq!((ai.gateway.port, ai.agent.max_concurrent_runs), (4300, 2));
    assert_eq!(ai.search.brave_key_hint, "…ijkl");
    assert_eq!(
        reopened.secrets().get(SEARCH_BRAVE).expect("read"),
        Some("brave-abcdefghijkl".to_owned())
    );
    drop(reopened);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn history_retention_takes_effect_on_the_store() {
    let state = AppState::open_in_memory().expect("state opens");
    settings_update_impl(
        &state,
        SettingsSection::Data,
        &json!({"history_retention": 10}),
    )
    .expect("valid");
    assert_eq!(state.store().history_retention(), 10);
}

#[test]
fn reset_with_keys_and_reset_all_persist_and_notify() {
    let dir = std::env::temp_dir().join(format!("pd-settings-reset-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("pingdeck.sqlite3");
    let heard: Arc<Mutex<Vec<SettingsSection>>> = Arc::default();
    {
        let state = AppState::open(&path).expect("opens");
        settings_secret_set_impl(&state, SettingsSecret::SearchBrave, "brave-abcdefghijkl")
            .expect("stores");
        settings_update_impl(
            &state,
            SettingsSection::Appearance,
            &json!({"ui_font_size": 20, "theme": "dark"}),
        )
        .expect("update");
        settings_update_impl(&state, SettingsSection::Editor, &json!({"tab_size": 4})).expect("ok");
        let sink = {
            let heard = Arc::clone(&heard);
            Arc::new(move |event: SettingsEvent| {
                let SettingsEvent::Changed { section, .. } = event;
                heard.lock().expect("lock").push(section);
                true
            }) as Arc<dyn EventSink<SettingsEvent>>
        };
        settings_subscribe_impl(&state, sink).expect("subscribes");

        let only_font = settings_reset_section_impl(
            &state,
            SettingsSection::Appearance,
            Some(vec!["ui_font_size".to_owned()]),
        )
        .expect("resets one key");
        assert_eq!(only_font.appearance.ui_font_size, 13);
        assert_eq!(only_font.appearance.theme, crate::settings::Theme::Dark);
        let error = settings_reset_section_impl(
            &state,
            SettingsSection::Appearance,
            Some(vec!["nope".to_owned()]),
        )
        .expect_err("unknown key");
        assert!(matches!(error, AppError::Validation { .. }));
        assert_eq!(*heard.lock().expect("lock"), [SettingsSection::Appearance]);
    }
    {
        // The one-key reset was persisted.
        let state = AppState::open(&path).expect("reopens");
        assert_eq!(state.settings().appearance.ui_font_size, 13);
        assert_eq!(
            state.settings().appearance.theme,
            crate::settings::Theme::Dark
        );
        let all = settings_reset_all_impl(&state).expect("resets everything");
        assert_eq!(all.appearance, Default::default());
        assert_eq!(all.editor, Default::default());
        assert_eq!(all.ai.search.brave_key_hint, "…ijkl");
    }
    let reopened = AppState::open(&path).expect("reopens");
    assert_eq!(reopened.settings().editor.tab_size, 2);
    assert_eq!(reopened.settings().appearance, Default::default());
    drop(reopened);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_old_export_naming_bind_lan_cannot_loosen_serve_lan() {
    let state = AppState::open_in_memory().expect("state opens");
    let mut incoming = json!({"gateway": {"bind_lan": true, "max_retries": 4}});
    crate::app::settings_service::keep_local_fields(
        &state.settings(),
        SettingsSection::Ai,
        &mut incoming,
    );
    let ai = state
        .settings()
        .with_section_value(SettingsSection::Ai, &incoming)
        .expect("loads as an ordinary section");
    assert!(!ai.ai.gateway.serve_lan);
    assert_eq!(ai.ai.gateway.max_retries, 4);
}

#[test]
fn reset_all_keeps_retention_limits_and_the_sync_binding() {
    let state = AppState::open_in_memory().expect("state opens");
    let dir = std::env::temp_dir().join("pd-sync-keep");
    let patches = [
        (
            SettingsSection::Data,
            json!({"history_retention": 500, "chat_retention_days": 30}),
        ),
        (
            SettingsSection::Ai,
            json!({"providers": {"usage_retention_days": 90}}),
        ),
        (SettingsSection::Mock, json!({"log_capacity": 50})),
        (
            SettingsSection::Sync,
            json!({"directory": dir.to_str(), "auto_export": true}),
        ),
        (SettingsSection::Editor, json!({"tab_size": 4})),
    ];
    for (section, patch) in patches {
        settings_update_impl(&state, section, &patch).expect("update");
    }
    let before = settings_get_impl(&state);
    let after = settings_reset_all_impl(&state).expect("reset");
    assert_eq!(after.editor, Settings::default().editor);
    assert_eq!(after.data, before.data);
    assert_eq!(after.ai.providers.usage_retention_days, 90);
    assert_eq!(after.mock.log_capacity, 50);
    assert_eq!(
        (after.sync.directory.as_str(), after.sync.auto_export),
        (before.sync.directory.as_str(), true)
    );
}
