//! Tests for patching, validation and stored rows.

use serde_json::json;

use super::*;
use crate::agent_target::AgentTarget;
use crate::stored_setting::StoredSetting;

fn message(error: AppError) -> String {
    assert!(matches!(error, AppError::Validation { .. }), "{error:?}");
    error.message().to_owned()
}

#[test]
fn default_sections_round_trip_through_validation() {
    for section in SettingsSection::ALL {
        Settings::default()
            .with_section_value(*section, &Settings::default().section_value(*section))
            .expect("defaults validate");
    }
}

#[test]
fn a_patch_changes_only_the_named_fields() {
    let settings = Settings::default()
        .patched(
            SettingsSection::Network,
            &json!({"timeout_ms": 5000, "verify_tls": false}),
        )
        .expect("valid patch");
    assert_eq!(settings.network.timeout_ms, 5000);
    assert!(!settings.network.verify_tls);
    assert_eq!(settings.network.max_redirects, 10);
    assert_eq!(settings.data, DataSettings::default());
}

#[test]
fn nested_maps_merge_and_null_removes_a_key() {
    let with_two = Settings::default()
        .patched(
            SettingsSection::Shortcuts,
            &json!({"overrides": {"send": "Mod+Enter", "save": "Mod+S"}}),
        )
        .expect("valid");
    let with_one = with_two
        .patched(
            SettingsSection::Shortcuts,
            &json!({"overrides": {"send": null}}),
        )
        .expect("valid");
    assert_eq!(with_one.shortcuts.overrides.len(), 1);
    assert_eq!(with_one.shortcuts.overrides["save"], "Mod+S");

    let named = json!({"agent": {"default_target": {"kind": "named", "name": "fast"}}});
    let with_target = Settings::default()
        .patched(SettingsSection::Ai, &named)
        .expect("valid");
    assert_eq!(
        with_target.ai.agent.default_target,
        Some(AgentTarget::Named {
            name: "fast".to_owned()
        })
    );
    // A nested patch leaves its siblings alone; `null` puts the default back.
    assert_eq!(with_target.ai.gateway, AiGatewaySettings::default());
    let cleared = with_target
        .patched(
            SettingsSection::Ai,
            &json!({"agent": {"default_target": null}}),
        )
        .expect("valid");
    assert_eq!(cleared.ai.agent.default_target, None);
}

#[test]
fn validation_names_the_field() {
    let rejected =
        |section, patch| message(Settings::default().patched(section, &patch).unwrap_err());
    assert!(
        rejected(SettingsSection::Network, json!({"timeout_ms": 0})).contains("network.timeout_ms")
    );
    assert!(
        rejected(SettingsSection::Network, json!({"proxy_url": "ftp://x"}))
            .contains("network.proxy_url")
    );
    assert!(rejected(SettingsSection::Mock, json!({"first_port": 80})).contains("mock.first_port"));
    assert!(
        rejected(SettingsSection::Ai, json!({"gateway": {"port": 80}})).contains("ai.gateway.port")
    );
    assert!(
        rejected(SettingsSection::Data, json!({"preview_bytes": 2_000_000}))
            .contains("data.preview_bytes")
    );
    assert!(
        rejected(SettingsSection::Sync, json!({"directory": "relative"}))
            .contains("sync.directory")
    );
    assert!(
        rejected(
            SettingsSection::Ai,
            json!({"agent": {"max_tool_iterations": 0}})
        )
        .contains("ai.agent.max_tool_iterations")
    );
}

#[test]
fn unknown_fields_and_wrong_types_are_rejected_by_name() {
    let rejected =
        |section, patch| message(Settings::default().patched(section, &patch).unwrap_err());
    assert!(rejected(SettingsSection::General, json!({"colour": 1})).contains("general.colour"));
    assert!(
        rejected(SettingsSection::Appearance, json!({"theme": "blue"}))
            .contains("appearance.theme")
    );
    assert!(
        rejected(SettingsSection::Editor, json!({"tab_size": "wide"})).contains("editor.tab_size")
    );
    assert!(rejected(SettingsSection::Editor, json!(5)).contains("JSON object"));
    assert!(
        rejected(
            SettingsSection::Ai,
            json!({"agent": {"tool_approval": "ask"}})
        )
        .contains("unknown setting ai.agent.tool_approval")
    );
    // Nested keys are checked too, and the removed v1 fields are gone.
    assert!(
        rejected(SettingsSection::Ai, json!({"gateway": {"bogus": 1}}))
            .contains("unknown setting ai.gateway.bogus")
    );
    for removed in [
        json!({"gateway": {"token": "x"}}),
        json!({"gateway": {"master_token_hint": "x"}}),
        json!({"agent": {"custom_instructions": "x"}}),
        json!({"agent": {"default_system_prompt": "x"}}),
        json!({"search": {"tavily_api_key": "x"}}),
    ] {
        assert!(
            rejected(SettingsSection::Ai, removed.clone()).contains("unknown setting ai."),
            "{removed}"
        );
    }
    assert!(rejected(SettingsSection::Ai, json!({"gateway": 5})).contains("ai.gateway"));
    assert!(
        rejected(SettingsSection::Ai, json!({"gateway": {"port": "x"}}))
            .contains("ai.gateway.port")
    );
    assert!(
        rejected(
            SettingsSection::Ai,
            json!({"gateway": {"capture_mode": "most"}})
        )
        .contains("ai.gateway.capture_mode")
    );
    assert!(
        rejected(
            SettingsSection::Ai,
            json!({"agent": {"default_target": {"kind": "x"}}})
        )
        .contains("ai.agent.default_target")
    );
}

#[test]
fn reset_restores_one_section_only() {
    let changed = Settings::default()
        .patched(SettingsSection::Network, &json!({"timeout_ms": 1000}))
        .and_then(|s| s.patched(SettingsSection::Mock, &json!({"first_port": 4000})))
        .expect("valid");
    let reset = changed.reset(SettingsSection::Network).expect("valid");
    assert_eq!(reset.network, NetworkSettings::default());
    assert_eq!(reset.mock.first_port, 4000);
}

#[test]
fn gateway_port_and_first_mock_port_must_differ_whichever_section_changes() {
    let rejected = |settings: &Settings, section, patch| {
        message(settings.patched(section, &patch).unwrap_err())
    };
    let defaults = Settings::default();
    let error = rejected(
        &defaults,
        SettingsSection::Ai,
        json!({"gateway": {"port": 3001}}),
    );
    assert_eq!(error, "ai.gateway.port must differ from mock.first_port");
    let error = rejected(
        &defaults,
        SettingsSection::Mock,
        json!({"first_port": 4141}),
    );
    assert_eq!(error, "ai.gateway.port must differ from mock.first_port");
    let replaced = defaults
        .with_section_value(SettingsSection::Mock, &json!({"first_port": 4141}))
        .unwrap_err();
    assert_eq!(
        message(replaced),
        "ai.gateway.port must differ from mock.first_port"
    );

    // Resetting a section back onto the other one's port is refused too.
    let moved = defaults
        .patched(SettingsSection::Ai, &json!({"gateway": {"port": 5000}}))
        .and_then(|s| s.patched(SettingsSection::Mock, &json!({"first_port": 4141})))
        .expect("distinct ports");
    let error = message(moved.reset(SettingsSection::Ai).unwrap_err());
    assert_eq!(error, "ai.gateway.port must differ from mock.first_port");

    // An unrelated section is not blocked by an existing clash.
    let mut clashing = Settings::default();
    clashing.ai.gateway.port = clashing.mock.first_port;
    clashing
        .patched(SettingsSection::Network, &json!({"timeout_ms": 1000}))
        .expect("unrelated update");
}

#[test]
fn skill_dirs_must_be_anchored_clean_paths() {
    let patch = |dirs: serde_json::Value| {
        Settings::default().patched(
            SettingsSection::Ai,
            &json!({"agent": { "skill_dirs": dirs }}),
        )
    };
    for ok in ["/opt/skills", "~/skills", "~", "/a/b..c/d"] {
        patch(json!([ok])).unwrap_or_else(|e| panic!("{ok}: {e:?}"));
    }
    for bad in [
        "relative/skills",
        "",
        "  ",
        "/a/../b",
        "~/../etc",
        "..",
        "/a\\..\\b",
        "/a\0b",
        "/a\nb",
    ] {
        let error = message(patch(json!([bad])).unwrap_err());
        assert!(error.contains("ai.agent.skill_dirs"), "{bad:?}: {error}");
    }
    let long = format!("/{}", "a".repeat(1024));
    assert!(patch(json!([long])).is_err());
    let longest = format!("/{}", "a".repeat(1023));
    assert!(patch(json!([longest])).is_ok());
    let many: Vec<String> = (0..33).map(|i| format!("/s{i}")).collect();
    assert!(patch(json!(many)).is_err());
}

#[test]
fn default_target_is_a_short_clean_name_or_pinned_model() {
    let patch = |target: serde_json::Value| {
        Settings::default().patched(
            SettingsSection::Ai,
            &json!({"agent": {"default_target": target}}),
        )
    };
    let named = |name: serde_json::Value| json!({"kind": "named", "name": name});
    for ok in [
        named(json!("fast")),
        named(json!("openai/gpt-4o")),
        json!(null),
        named(json!("é".repeat(200))),
        json!({"kind": "pinned", "provider_id": "p", "key_id": null, "model": "m"}),
        json!({"kind": "pinned", "provider_id": "p", "key_id": "k", "model": "m"}),
    ] {
        patch(ok.clone()).unwrap_or_else(|e| panic!("{ok}: {e:?}"));
    }
    for bad in [
        named(json!("")),
        named(json!("   ")),
        named(json!("a\nb")),
        named(json!("a\0b")),
        named(json!("x".repeat(201))),
        json!({"kind": "pinned", "provider_id": "", "key_id": null, "model": "m"}),
        json!({"kind": "pinned", "provider_id": "p", "key_id": "", "model": "m"}),
        json!({"kind": "pinned", "provider_id": "p", "key_id": null, "model": "a\nb"}),
    ] {
        let error = message(patch(bad.clone()).unwrap_err());
        assert!(error.contains("ai.agent.default_target"), "{bad}: {error}");
    }
}

#[test]
fn every_ai_range_is_enforced_at_both_ends() {
    // (patch group, field, lowest ok, highest ok), the neighbours just outside must fail.
    let ranges: &[(&str, &str, u64, u64)] = &[
        ("gateway", "port", 1024, 65_535),
        ("gateway", "max_retries", 0, 10),
        ("gateway", "first_byte_timeout_s", 1, 600),
        ("gateway", "idle_timeout_s", 1, 3600),
        ("gateway", "circuit_failure_threshold", 1, 100),
        ("gateway", "circuit_window_s", 1, 3600),
        ("gateway", "circuit_open_s", 5, 3600),
        ("gateway", "circuit_open_max_s", 5, 86_400),
        ("providers", "probe_concurrency", 1, 16),
        ("providers", "probe_timeout_s", 5, 300),
        ("providers", "usage_retention_days", 30, 3650),
        ("agent", "max_tool_iterations", 1, 100),
        ("agent", "max_concurrent_runs", 1, 8),
        ("agent", "max_subagents", 1, 8),
        ("agent", "max_image_mb", 1, 20),
        ("agent", "max_file_mb", 1, 100),
        ("agent", "image_max_edge_px", 512, 2048),
        ("agent", "compact_threshold_pct", 50, 95),
        ("agent", "keep_recent_turns", 1, 20),
        ("agent", "shell_timeout_s", 1, 3600),
    ];
    for (group, field, low, high) in ranges {
        let patch = |value: u64| {
            Settings::default().patched(SettingsSection::Ai, &json!({*group: {*field: value}}))
        };
        for ok in [*low, *high] {
            patch(ok).unwrap_or_else(|e| panic!("{group}.{field}={ok}: {e:?}"));
        }
        for bad in [low.wrapping_sub(1), high + 1] {
            let error = patch(bad).expect_err(&format!("{group}.{field}={bad}"));
            assert!(
                matches!(error, AppError::Validation { .. }),
                "{group}.{field}={bad}: {error:?}"
            );
        }
    }
}

#[test]
fn zero_means_off_for_the_optional_ranges() {
    // (group, field, off, lowest ok, highest ok): 0 disables, otherwise the range applies.
    let ranges: &[(&str, &str, u64, u64, u64)] =
        &[("providers", "balance_refresh_min", 0, 5, 1440)];
    for (group, field, off, low, high) in ranges {
        let patch = |value: u64| {
            Settings::default().patched(SettingsSection::Ai, &json!({*group: {*field: value}}))
        };
        for ok in [*off, *low, *high] {
            patch(ok).unwrap_or_else(|e| panic!("{group}.{field}={ok}: {e:?}"));
        }
        for bad in [low - 1, high + 1] {
            let error = message(patch(bad).expect_err(&format!("{group}.{field}={bad}")));
            assert!(error.contains(&format!("ai.{group}.{field}")), "{error}");
        }
    }
}

#[test]
fn serve_lan_requires_a_token_and_the_other_ai_rules_hold() {
    let defaults = Settings::default();
    let error = message(
        defaults
            .patched(
                SettingsSection::Ai,
                &json!({"gateway": {"serve_lan": true, "require_token": false}}),
            )
            .unwrap_err(),
    );
    assert_eq!(
        error,
        "ai.gateway.serve_lan requires ai.gateway.require_token"
    );
    // Either alone is fine; turning the token off while on the LAN is refused too.
    let lan = defaults
        .patched(
            SettingsSection::Ai,
            &json!({"gateway": {"serve_lan": true}}),
        )
        .expect("token still required");
    let error = message(
        lan.patched(
            SettingsSection::Ai,
            &json!({"gateway": {"require_token": false}}),
        )
        .unwrap_err(),
    );
    assert!(error.contains("serve_lan"), "{error}");
    defaults
        .patched(
            SettingsSection::Ai,
            &json!({"gateway": {"require_token": false}}),
        )
        .expect("loopback only may run open");

    for (patch, field) in [
        (
            json!({"providers": {"default_test_prompt": "  "}}),
            "ai.providers.default_test_prompt",
        ),
        (
            json!({"search": {"searxng_base_url": "ftp://x"}}),
            "ai.search.searxng_base_url",
        ),
    ] {
        assert!(
            message(defaults.patched(SettingsSection::Ai, &patch).unwrap_err()).contains(field)
        );
    }
    for backend in ["auto", "duckduckgo", "brave", "tavily", "searxng"] {
        defaults
            .patched(
                SettingsSection::Ai,
                &json!({"search": {"backend": backend}}),
            )
            .unwrap_or_else(|e| panic!("{backend}: {e:?}"));
    }
    // Schema-2 names still deserialize.
    for old in ["none", "provider_native"] {
        let ai: AiSettings = serde_json::from_value(json!({"search": {"backend": old}})).unwrap();
        assert_eq!(ai.search.backend, SearchBackend::Auto, "{old}");
    }
    for mode in ["off", "errors_only", "all"] {
        defaults
            .patched(
                SettingsSection::Ai,
                &json!({"gateway": {"capture_mode": mode}}),
            )
            .unwrap_or_else(|e| panic!("{mode}: {e:?}"));
    }
}

#[test]
fn secret_hints_are_read_only_for_patches_and_replacements() {
    let mut settings = Settings::default();
    settings.ai.search.tavily_key_hint = "tvl…mnop".to_owned();
    for (patch, field) in [
        (
            json!({"search": {"tavily_key_hint": "forged"}}),
            "ai.search.tavily_key_hint",
        ),
        (
            json!({"search": {"brave_key_hint": "forged"}}),
            "ai.search.brave_key_hint",
        ),
        (
            json!({"search": {"tavily_key_hint": null}}),
            "ai.search.tavily_key_hint",
        ),
    ] {
        let error = message(settings.patched(SettingsSection::Ai, &patch).unwrap_err());
        assert_eq!(error, format!("{field} is read-only"), "{patch}");
    }
    // Restating the current value is no change; the section is otherwise editable.
    let same = settings
        .patched(
            SettingsSection::Ai,
            &json!({"search": {"tavily_key_hint": "tvl…mnop", "backend": "tavily"}}),
        )
        .expect("unchanged hint");
    assert_eq!(same.ai.search.tavily_key_hint, "tvl…mnop");
    // A whole-section replacement that drops or changes a hint is refused as well.
    let error = message(
        settings
            .with_section_value(
                SettingsSection::Ai,
                &json!({"search": {"backend": "brave"}}),
            )
            .unwrap_err(),
    );
    assert!(error.contains("ai.search.tavily_key_hint"), "{error}");
}

#[test]
fn reset_of_the_ai_section_keeps_the_key_hints() {
    let mut settings = Settings::default();
    settings.ai.search.tavily_key_hint = "tvl…mnop".to_owned();
    settings.ai.search.brave_key_hint = "••••".to_owned();
    settings.ai.agent.max_tool_iterations = 7;
    let reset = settings.reset(SettingsSection::Ai).expect("valid");
    assert_eq!(reset.ai.agent, AiAgentSettings::default());
    assert_eq!(reset.ai.search.tavily_key_hint, "tvl…mnop");
    assert_eq!(reset.ai.search.brave_key_hint, "••••");
}

#[test]
fn stored_rows_load_and_bad_rows_fall_back_to_defaults() {
    let row = |section: &str, version: u32, value: &str| StoredSetting {
        section: section.to_owned(),
        version,
        value_json: value.to_owned(),
    };
    let settings = Settings::from_rows(&[
        row(
            "network",
            1,
            r#"{"timeout_ms": 1234, "future_field": true}"#,
        ),
        row("mock", 1, r#"{"first_port": 1}"#),
        row("data", 1, "not json"),
        row("general", SETTINGS_VERSION + 1, r#"{"ui_font_size": 20}"#),
        row("unknown_section", 1, "{}"),
        // Schema-1 rows that migration v22 replaced are not sections any more.
        row("gateway", 1, r#"{"port": 4999}"#),
        row("ai", 2, r#"{"gateway": {"port": 4999}, "future": 1}"#),
    ]);
    assert_eq!(settings.network.timeout_ms, 1234);
    assert_eq!(settings.mock, MockSettings::default());
    assert_eq!(settings.data, DataSettings::default());
    assert_eq!(settings.general, GeneralSettings::default());
    assert_eq!(settings.ai.gateway.port, 4999);
    let invalid = Settings::from_rows(&[row(
        "ai",
        2,
        r#"{"gateway": {"bind_lan": true, "require_token": false}}"#,
    )]);
    assert_eq!(invalid.ai, AiSettings::default());
}

#[test]
fn appearance_has_its_own_section_and_ranges() {
    assert_eq!(
        SettingsSection::ALL
            .iter()
            .map(|section| section.as_str())
            .collect::<Vec<_>>()[..2],
        ["general", "appearance"]
    );
    let general = Settings::default().section_value(SettingsSection::General);
    for moved in ["theme", "ui_font_size", "mono_font_size"] {
        assert!(general.get(moved).is_none(), "{moved} left general");
    }
    let appearance = Settings::default().section_value(SettingsSection::Appearance);
    assert_eq!(
        appearance,
        json!({
            "theme": "system", "ui_font_size": 13, "mono_font_size": 12,
            "density": "comfortable", "reduce_motion": "system", "sidebar": "auto"
        })
    );
    for (field, low, high) in [("ui_font_size", 10, 26), ("mono_font_size", 10, 24)] {
        for ok in [low, high] {
            Settings::default()
                .patched(SettingsSection::Appearance, &json!({field: ok}))
                .unwrap_or_else(|e| panic!("{field}={ok}: {e:?}"));
        }
        for bad in [low - 1, high + 1] {
            let error = Settings::default()
                .patched(SettingsSection::Appearance, &json!({field: bad}))
                .unwrap_err();
            assert!(message(error).contains(&format!("appearance.{field}")));
        }
    }
    let patched = Settings::default()
        .patched(
            SettingsSection::Appearance,
            &json!({"density": "compact", "reduce_motion": "on", "sidebar": "rail", "theme": "dark"}),
        )
        .expect("valid");
    assert_eq!(patched.appearance.density, Density::Compact);
    assert_eq!(patched.appearance.reduce_motion, ReduceMotion::On);
    assert_eq!(patched.appearance.sidebar, SidebarMode::Rail);
    assert_eq!(patched.appearance.theme, Theme::Dark);
}

#[test]
fn network_sync_and_new_ai_lists_are_validated() {
    let rejected = |section, patch| {
        message(
            Settings::default()
                .patched(section, &patch)
                .expect_err("rejected"),
        )
    };
    let many = |n: usize| (0..n).map(|i| format!("h{i}")).collect::<Vec<_>>();
    assert!(
        rejected(SettingsSection::Network, json!({"no_proxy": many(65)}))
            .contains("network.no_proxy")
    );
    assert!(
        rejected(
            SettingsSection::Network,
            json!({"ca_bundle_path": "rel.pem"})
        )
        .contains("network.ca_bundle_path")
    );
    assert!(
        rejected(
            SettingsSection::Ai,
            json!({"gateway": {"extra_allowed_hosts": many(17)}})
        )
        .contains("ai.gateway.extra_allowed_hosts")
    );
    assert!(
        rejected(
            SettingsSection::Ai,
            json!({"agent": {"shell_env_passthrough": ["A=b"]}})
        )
        .contains("ai.agent.shell_env_passthrough")
    );
    let ok = Settings::default()
        .patched(
            SettingsSection::Network,
            &json!({"no_proxy": many(64), "ca_bundle_path": "/etc/ssl/ca.pem"}),
        )
        .and_then(|s| s.patched(SettingsSection::Sync, &json!({"auto_export": true})))
        .and_then(|s| {
            s.patched(
                SettingsSection::Ai,
                &json!({"gateway": {"extra_allowed_hosts": many(16)},
                        "agent": {"default_effort": "high", "default_workspace": "ws1", "auto_title": false}}),
            )
        })
        .expect("valid");
    assert!(ok.sync.auto_export);
    assert_eq!(ok.ai.agent.default_effort, ReasoningEffortOrAuto::High);
    // The extra safety patterns start empty (the pinned rules are not a setting), at most 64.
    assert!(Settings::default().ai.agent.blocked_commands.is_empty());
    assert!(
        Settings::default()
            .patched(
                SettingsSection::Ai,
                &json!({"agent": {"blocked_commands": vec!["x *"; 65]}})
            )
            .is_err()
    );
}

#[test]
fn reset_with_keys_resets_only_those_keys() {
    let changed = Settings::default()
        .patched(
            SettingsSection::Ai,
            &json!({"gateway": {"port": 5000, "max_retries": 7}, "agent": {"max_tool_iterations": 9}}),
        )
        .and_then(|s| s.patched(SettingsSection::Shortcuts, &json!({"overrides": {"a": "b"}})))
        .expect("valid");
    let reset = changed
        .reset_keys(SettingsSection::Ai, &["gateway.port".to_owned()])
        .expect("valid");
    assert_eq!(reset.ai.gateway.port, 4141);
    assert_eq!(reset.ai.gateway.max_retries, 7);
    assert_eq!(reset.ai.agent.max_tool_iterations, 9);
    // A key naming an object resets everything inside it.
    let reset = changed
        .reset_keys(SettingsSection::Ai, &["gateway".to_owned()])
        .expect("valid");
    assert_eq!(reset.ai.gateway, AiGatewaySettings::default());
    assert_eq!(reset.ai.agent.max_tool_iterations, 9);
    // Free-form maps and optional values reset as well.
    let reset = changed
        .reset_keys(SettingsSection::Shortcuts, &["overrides".to_owned()])
        .expect("valid");
    assert!(reset.shortcuts.overrides.is_empty());
    // Unknown keys, empty lists and read-only hints are refused.
    for (section, keys) in [
        (SettingsSection::Ai, vec!["gateway.bogus".to_owned()]),
        (SettingsSection::Ai, vec![]),
        (
            SettingsSection::Ai,
            vec!["search.tavily_key_hint".to_owned()],
        ),
    ] {
        let mut with_hint = changed.clone();
        with_hint.ai.search.tavily_key_hint = "tvl…mnop".to_owned();
        message(with_hint.reset_keys(section, &keys).unwrap_err());
    }
}

#[test]
fn a_schema_2_row_loads_with_renamed_and_removed_fields() {
    let row = |section: &str, value: &str| StoredSetting {
        section: section.to_owned(),
        version: 2,
        value_json: value.to_owned(),
    };
    // Shapes written before this change: `bind_lan`, the removed agent/provider fields and the
    // old search backend names all still load; the old general row seeds the appearance section.
    let settings = Settings::from_rows(&[
        row(
            "ai",
            r#"{"gateway": {"bind_lan": true, "port": 4999, "circuit_failure_threshold": 5},
                "providers": {"probe_concurrency": 4, "auto_balance_refresh_s": 3600},
                "agent": {"tool_approval": "ask", "max_subagent_depth": 2, "max_attachment_mb": 20,
                          "auto_compact_threshold_pct": 85, "shell_allow_network": true,
                          "max_tool_iterations": 25, "max_concurrent_runs": 4},
                "search": {"backend": "provider_native", "searxng_base_url": "https://s.test"}}"#,
        ),
        row(
            "general",
            r#"{"theme": "dark", "language": "en", "ui_font_size": 18, "mono_font_size": 11}"#,
        ),
    ]);
    let gateway = &settings.ai.gateway;
    assert!(gateway.serve_lan && gateway.require_token);
    assert_eq!((gateway.port, gateway.circuit_failure_threshold), (4999, 5));
    assert_eq!(settings.ai.providers.probe_concurrency, 4);
    assert_eq!(settings.ai.providers.balance_refresh_min, 0);
    assert_eq!(settings.ai.agent.max_tool_iterations, 25);
    assert_eq!(settings.ai.agent.max_subagents, 3);
    assert_eq!(settings.ai.search.backend, SearchBackend::Auto);
    assert_eq!(settings.ai.search.searxng_base_url, "https://s.test");
    assert_eq!(settings.general.language, Language::En);
    assert_eq!(settings.appearance.theme, Theme::Dark);
    assert_eq!(
        (
            settings.appearance.ui_font_size,
            settings.appearance.mono_font_size
        ),
        (18, 11)
    );
    // Once an appearance row exists the general row no longer seeds it.
    let explicit = Settings::from_rows(&[
        row("appearance", r#"{"theme": "light"}"#),
        row("general", r#"{"theme": "dark"}"#),
    ]);
    assert_eq!(explicit.appearance.theme, Theme::Light);
}

#[test]
fn a_stored_mock_log_capacity_is_clamped_not_dropped() {
    let row = |value: &str| StoredSetting {
        section: "mock".to_owned(),
        version: 2,
        value_json: value.to_owned(),
    };
    let high = Settings::from_rows(&[row(r#"{"first_port": 3500, "log_capacity": 50000}"#)]);
    assert_eq!(
        (high.mock.first_port, high.mock.log_capacity),
        (3500, 5_000)
    );
    let low = Settings::from_rows(&[row(r#"{"log_capacity": 1}"#)]);
    assert_eq!(low.mock.log_capacity, 10);
    // Updates are not clamped: out of range is an error.
    let error = Settings::default()
        .patched(SettingsSection::Mock, &json!({"log_capacity": 5_001}))
        .unwrap_err();
    assert!(message(error).contains("mock.log_capacity"));
}

#[test]
fn removed_gateway_and_agent_keys_are_unknown_settings() {
    for removed in [
        json!({"gateway": {"allowed_origins": []}}),
        json!({"gateway": {"log_max_rows": 100}}),
        json!({"gateway": {"log_max_days": 7}}),
        json!({"gateway": {"capture_max_mb": 200}}),
        json!({"gateway": {"capture_body_cap_kb": 256}}),
        json!({"gateway": {"log_retention_days": 30}}),
        json!({"agent": {"shell_block_network": true}}),
    ] {
        let error = message(
            Settings::default()
                .patched(SettingsSection::Ai, &removed)
                .unwrap_err(),
        );
        assert!(error.contains("unknown setting ai."), "{removed}: {error}");
    }
    // A stored row that still carries them loads (the keys are ignored).
    let row = StoredSetting {
        section: "ai".to_owned(),
        version: 2,
        value_json: r#"{"gateway": {"port": 4999, "log_max_rows": 100, "allowed_origins": ["x"]},
                        "agent": {"shell_block_network": true, "max_tool_iterations": 9}}"#
            .to_owned(),
    };
    let loaded = Settings::from_rows(&[row]);
    assert_eq!(loaded.ai.gateway.port, 4999);
    assert_eq!(loaded.ai.agent.max_tool_iterations, 9);
}

#[test]
fn a_stored_approval_timeout_of_zero_reads_as_600_and_old_values_are_clamped() {
    let load = |value: &str| {
        Settings::from_rows(&[StoredSetting {
            section: "ai".to_owned(),
            version: 2,
            value_json: format!(
                r#"{{"agent": {{"approval_timeout_s": {value}, "max_tool_iterations": 9}}}}"#
            ),
        }])
    };
    for (stored, read) in [(0, 600), (30, 60), (86_400, 3600), (120, 120)] {
        let settings = load(&stored.to_string());
        assert_eq!(settings.ai.agent.approval_timeout_s, read, "{stored}");
        assert_eq!(
            settings.ai.agent.max_tool_iterations, 9,
            "{stored} keeps the row"
        );
    }
    // Updates are not normalised: 0 and out-of-range are errors.
    for bad in [0, 59, 3601] {
        let error = Settings::default()
            .patched(
                SettingsSection::Ai,
                &json!({"agent": {"approval_timeout_s": bad}}),
            )
            .unwrap_err();
        assert!(message(error).contains("ai.agent.approval_timeout_s"));
    }
}

#[test]
fn stored_sub_agent_and_image_edge_values_above_the_prd_range_are_clamped() {
    let settings = Settings::from_rows(&[StoredSetting {
        section: "ai".to_owned(),
        version: 2,
        value_json: r#"{"agent": {"max_subagents": 16, "image_max_edge_px": 4096}}"#.to_owned(),
    }]);
    assert_eq!(settings.ai.agent.max_subagents, 8);
    assert_eq!(settings.ai.agent.image_max_edge_px, 2048);
    for patch in [
        json!({"max_subagents": 9}),
        json!({"image_max_edge_px": 2049}),
    ] {
        assert!(
            Settings::default()
                .patched(SettingsSection::Ai, &json!({ "agent": patch }))
                .is_err()
        );
    }
}

#[test]
fn extra_allowed_hosts_are_host_or_host_port_even_with_lan_off() {
    let patch = |hosts: serde_json::Value| {
        Settings::default().patched(
            SettingsSection::Ai,
            &json!({"gateway": {"extra_allowed_hosts": hosts}}),
        )
    };
    let ok = patch(json!([
        "gw.example",
        "gw.example:4141",
        "192.168.1.9:80",
        "[::1]:4141",
        "mac.local"
    ]))
    .expect("valid with serve_lan off");
    assert!(!ok.ai.gateway.serve_lan);
    for bad in [
        "",
        " ",
        "https://gw.example",
        "gw.example/x",
        "a b",
        "gw:0",
        "gw:99999",
        "user@gw",
        "[::1",
    ] {
        let error = patch(json!([bad])).expect_err(bad);
        assert!(
            message(error).contains("ai.gateway.extra_allowed_hosts"),
            "{bad:?}"
        );
    }
}

#[test]
fn cost_caps_are_off_or_positive_numbers() {
    let patch = |value: serde_json::Value| {
        Settings::default().patched(
            SettingsSection::Ai,
            &json!({"agent": {"cost_cap_usd": value, "cost_cap_cny": value}}),
        )
    };
    let on = patch(json!(2.5)).expect("positive");
    assert_eq!(on.ai.agent.cost_cap_usd, Some(2.5));
    assert_eq!(on.ai.agent.cost_cap_cny, Some(2.5));
    // `null` removes the key, which reads as the default: off.
    let off = on
        .patched(
            SettingsSection::Ai,
            &json!({"agent": {"cost_cap_usd": null, "cost_cap_cny": null}}),
        )
        .expect("valid");
    assert_eq!(
        (off.ai.agent.cost_cap_usd, off.ai.agent.cost_cap_cny),
        (None, None)
    );
    for bad in [json!(0), json!(-1.0)] {
        let error = message(patch(bad.clone()).unwrap_err());
        assert!(error.contains("ai.agent.cost_cap_"), "{bad}: {error}");
    }
    assert!(patch(json!("x")).is_err());
}

#[test]
fn global_instructions_and_default_permission_mode_patch_persist_and_validate() {
    let settings = Settings::default()
        .patched(
            SettingsSection::Ai,
            &json!({"agent": {"global_instructions": "Be brief."}}),
        )
        .and_then(|s| {
            s.patched(
                SettingsSection::Ai,
                &json!({"agent": {"default_permission_mode": "ask"}}),
            )
        })
        .unwrap();
    assert_eq!(settings.ai.agent.global_instructions, "Be brief.");
    assert_eq!(settings.ai.agent.default_permission_mode, ToolApproval::Ask);
    // The stored row round-trips.
    let row = StoredSetting {
        section: "ai".to_owned(),
        version: SETTINGS_VERSION,
        value_json: settings.section_value(SettingsSection::Ai).to_string(),
    };
    assert_eq!(Settings::from_rows(&[row]).ai, settings.ai);
    // `null` and the empty string read as the defaults again.
    let cleared = settings
        .patched(
            SettingsSection::Ai,
            &json!({"agent": {"global_instructions": "", "default_permission_mode": null}}),
        )
        .unwrap();
    assert_eq!(cleared.ai.agent, AiAgentSettings::default());

    // 4,000 characters (not bytes) fit once trimmed; 4,001 do not.
    let patch = |text: String| {
        Settings::default().patched(
            SettingsSection::Ai,
            &json!({"agent": {"global_instructions": text}}),
        )
    };
    assert!(patch(format!("  {}\n", "é".repeat(4000))).is_ok());
    let error = message(patch("é".repeat(4001)).unwrap_err());
    assert!(error.contains("ai.agent.global_instructions"), "{error}");
    let error = message(
        Settings::default()
            .patched(
                SettingsSection::Ai,
                &json!({"agent": {"default_permission_mode": "sometimes"}}),
            )
            .unwrap_err(),
    );
    assert!(
        error.contains("ai.agent.default_permission_mode"),
        "{error}"
    );
}

#[test]
fn notify_when_away_patches_persist_and_the_default_is_left_out() {
    assert!(
        Settings::default()
            .section_value(SettingsSection::General)
            .get("notify_when_away")
            .is_none()
    );
    let off = Settings::default()
        .patched(
            SettingsSection::General,
            &json!({"notify_when_away": "off"}),
        )
        .unwrap();
    assert_eq!(off.general.notify_when_away, NotifyWhenAway::Off);
    let row = StoredSetting {
        section: "general".to_owned(),
        version: SETTINGS_VERSION,
        value_json: off.section_value(SettingsSection::General).to_string(),
    };
    assert_eq!(Settings::from_rows(&[row]).general, off.general);
    let error = message(
        Settings::default()
            .patched(
                SettingsSection::General,
                &json!({"notify_when_away": "always"}),
            )
            .unwrap_err(),
    );
    assert!(error.contains("general.notify_when_away"), "{error}");
}

#[test]
fn agent_fallbacks_patch_persist_and_validate() {
    let patch = |chains: serde_json::Value| {
        Settings::default().patched(
            SettingsSection::Ai,
            &json!({"agent": {"fallbacks": chains}}),
        )
    };
    let settings = patch(json!({"main": ["p1/a", "p2/b"], "deep": ["p1/d"]})).unwrap();
    let chains = &settings.ai.agent.fallbacks;
    assert_eq!(chains.for_role(None), ["p1/a", "p2/b"]);
    assert_eq!(
        chains.for_role(Some(crate::agent::SubagentRole::Deep)),
        ["p1/d"]
    );
    assert!(
        chains
            .for_role(Some(crate::agent::SubagentRole::Fast))
            .is_empty()
    );
    let row = StoredSetting {
        section: "ai".to_owned(),
        version: SETTINGS_VERSION,
        value_json: settings.section_value(SettingsSection::Ai).to_string(),
    };
    assert_eq!(Settings::from_rows(&[row]).ai, settings.ai);

    let six: Vec<String> = (0..6).map(|i| format!("p/m{i}")).collect();
    let error = message(patch(json!({"fast": six})).unwrap_err());
    assert!(error.contains("ai.agent.fallbacks.fast"), "{error}");
    let error = message(patch(json!({"vision": ["p/a", "p/a"]})).unwrap_err());
    assert!(error.contains("ai.agent.fallbacks.vision"), "{error}");
    let error = message(patch(json!({"main": ["no-slash"]})).unwrap_err());
    assert!(error.contains("ai.agent.fallbacks.main"), "{error}");
}

#[test]
fn gwy3_extra_hosts_and_tokens_off_cannot_be_patched_together() {
    let code = |error: AppError| error.code();
    let open = Settings::default()
        .patched(
            SettingsSection::Ai,
            &json!({"gateway": {"require_token": false}}),
        )
        .expect("loopback only may run open");
    let hosts = json!({"gateway": {"extra_allowed_hosts": ["abc.ngrok.app"]}});
    assert_eq!(
        code(open.patched(SettingsSection::Ai, &hosts).unwrap_err()),
        Some(spanbay_base::error::ErrorCode::GatewayTokenRequired)
    );
    let tunnel = Settings::default()
        .patched(SettingsSection::Ai, &hosts)
        .expect("tokens on");
    assert_eq!(
        code(
            tunnel
                .patched(
                    SettingsSection::Ai,
                    &json!({"gateway": {"require_token": false}}),
                )
                .unwrap_err()
        ),
        Some(spanbay_base::error::ErrorCode::GatewayTokenRequired)
    );
}

#[test]
fn mock_extra_allowed_hosts_are_patchable_validated_and_omitted_while_empty() {
    let defaults = Settings::default();
    assert!(
        defaults
            .section_value(SettingsSection::Mock)
            .get("extra_allowed_hosts")
            .is_none()
    );
    let patched = defaults
        .patched(
            SettingsSection::Mock,
            &json!({"extra_allowed_hosts": ["10.0.2.2", "host.docker.internal:3001"]}),
        )
        .unwrap();
    assert_eq!(
        patched.mock.extra_allowed_hosts,
        ["10.0.2.2", "host.docker.internal:3001"]
    );
    for bad in [json!(["a b"]), json!(["host:0"]), json!(["evil.example/x"])] {
        let error = message(
            Settings::default()
                .patched(SettingsSection::Mock, &json!({"extra_allowed_hosts": bad}))
                .unwrap_err(),
        );
        assert!(error.contains("mock.extra_allowed_hosts"), "{error}");
    }
}
