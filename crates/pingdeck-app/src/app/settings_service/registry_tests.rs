//! The registry against the validators it describes: every row is probed at both bounds.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use crate::app::AppState;
use crate::app::settings_service::{SettingsSecret, settings_secret_set_impl};
use pingdeck_base::error::AppError;
use pingdeck_model::settings::policy::{UNREGISTERED_FIELDS, local_only_fields};
use pingdeck_model::settings::registry::*;
use pingdeck_model::settings::registry::{KIB, MIB, split_id};
use pingdeck_model::settings::{Settings, SettingsSection};

/// The nested merge patch that sets dotted `rest` (relative to its section) to `value`.
fn nested(rest: &str, value: Value) -> Value {
    rest.rsplit('.').fold(value, |inner, key| {
        let mut object = Map::new();
        object.insert(key.to_owned(), inner);
        Value::Object(object)
    })
}

/// `base` with the field `id` set to `value`, validated like any update.
fn set(base: &Settings, id: &str, value: Value) -> Result<Settings, AppError> {
    let (section, rest) = split_id(id).expect("a known section");
    base.patched(section, &nested(rest, value))
}

/// Fields that must be moved first so a probe of `id` does not trip a cross-field rule.
fn companions(id: &str, at_max: bool) -> Vec<(&'static str, Value)> {
    match (id, at_max) {
        ("data.preview_bytes", true) => vec![("data.inline_threshold_bytes", json!(64 * MIB))],
        ("data.inline_threshold_bytes", false) => vec![("data.preview_bytes", json!(KIB))],
        _ => Vec::new(),
    }
}

fn base_for(id: &str, at_max: bool) -> Settings {
    companions(id, at_max)
        .into_iter()
        .fold(Settings::default(), |settings, (other, value)| {
            set(&settings, other, value).expect("a companion value is valid")
        })
}

/// A list entry of exactly `len` bytes that passes the field's entry rules.
fn entry(id: &str, len: usize) -> String {
    match id {
        "ai.agent.skill_dirs" => format!("~/{}", "a".repeat(len - 2)),
        _ => "a".repeat(len),
    }
}

fn accepts(id: &str, base: &Settings, value: Value) {
    if let Err(error) = set(base, id, value.clone()) {
        panic!("{id} must accept {value}: {error}");
    }
}

fn rejects(id: &str, base: &Settings, value: Value) {
    assert!(
        set(base, id, value.clone()).is_err(),
        "{id} must reject {value}"
    );
}

fn registry_field(registry: &SettingsRegistryDto, id: &str) -> RegistryField {
    registry
        .fields
        .iter()
        .find(|field| field.id == id)
        .unwrap_or_else(|| panic!("no registry field {id}"))
        .clone()
}

fn usize_of(value: Option<u32>) -> usize {
    usize::try_from(value.expect("the row sets it")).expect("fits")
}

#[test]
fn every_numeric_bound_is_the_edge_of_what_the_validator_accepts() {
    let registry = settings_registry();
    let numeric = registry.fields.iter().filter(|field| {
        matches!(
            field.kind,
            RegistryKind::Int
                | RegistryKind::SizeBytes
                | RegistryKind::DurationMs
                | RegistryKind::DurationS
        )
    });
    let mut probed = 0;
    for field in numeric {
        let (min, max) = (field.min.expect("min"), field.max.expect("max"));
        accepts(&field.id, &base_for(&field.id, false), json!(min));
        accepts(&field.id, &base_for(&field.id, true), json!(max));
        rejects(&field.id, &base_for(&field.id, false), json!(min - 1));
        rejects(&field.id, &base_for(&field.id, true), json!(max + 1));
        if let Some(off) = field.off {
            accepts(&field.id, &Settings::default(), json!(off));
            assert!(off < min, "{}: `off` lies below `min`", field.id);
        }
        probed += 1;
    }
    assert!(probed >= 40, "only {probed} numeric rows were probed");
}

#[test]
fn a_cost_cap_must_be_above_its_exclusive_minimum() {
    let registry = settings_registry();
    for id in ["ai.agent.cost_cap_usd", "ai.agent.cost_cap_cny"] {
        let field = registry_field(&registry, id);
        assert!(field.exclusive_min && field.max.is_none());
        rejects(id, &Settings::default(), json!(field.min.expect("min")));
        accepts(id, &Settings::default(), json!(0.01));
        rejects(id, &Settings::default(), json!(-1.5));
    }
}

#[test]
fn every_enum_lists_exactly_the_values_the_settings_accept() {
    let registry = settings_registry();
    for field in registry
        .fields
        .iter()
        .filter(|field| field.kind == RegistryKind::Enum)
    {
        let options = field.options.as_ref().expect("options");
        for option in options {
            accepts(&field.id, &Settings::default(), json!(option));
        }
        rejects(&field.id, &Settings::default(), json!("no-such-option"));
    }
}

#[test]
fn list_and_map_limits_match_the_validators() {
    let registry = settings_registry();
    for field in registry
        .fields
        .iter()
        .filter(|field| field.kind == RegistryKind::List)
    {
        let (items, len) = (usize_of(field.max_items), usize_of(field.max_len));
        let full: Vec<String> = (0..items).map(|_| entry(&field.id, 3)).collect();
        accepts(&field.id, &Settings::default(), json!(full));
        let over: Vec<String> = (0..=items).map(|_| entry(&field.id, 3)).collect();
        rejects(&field.id, &Settings::default(), json!(over));
        accepts(
            &field.id,
            &Settings::default(),
            json!([entry(&field.id, len)]),
        );
        rejects(
            &field.id,
            &Settings::default(),
            json!([entry(&field.id, len + 1)]),
        );
    }
    let shortcuts = registry_field(&registry, "shortcuts.overrides");
    let (items, len) = (usize_of(shortcuts.max_items), usize_of(shortcuts.max_len));
    let overrides = |count: usize, keys: usize| {
        let map: Map<String, Value> = (0..count)
            .map(|index| (format!("cmd.{index}"), json!("k".repeat(keys))))
            .collect();
        Value::Object(map)
    };
    let id = "shortcuts.overrides";
    accepts(id, &Settings::default(), overrides(items, len));
    rejects(id, &Settings::default(), overrides(items + 1, 3));
    rejects(id, &Settings::default(), overrides(1, len + 1));
}

#[test]
fn text_limits_match_the_validators() {
    let registry = settings_registry();
    let prompt = registry_field(&registry, "ai.providers.default_test_prompt");
    let (min, max) = (usize_of(prompt.min_len), usize_of(prompt.max_len));
    accepts(&prompt.id, &Settings::default(), json!("a".repeat(max)));
    rejects(&prompt.id, &Settings::default(), json!("a".repeat(max + 1)));
    rejects(&prompt.id, &Settings::default(), json!("a".repeat(min - 1)));

    let ca = registry_field(&registry, "network.ca_bundle_path");
    let max = usize_of(ca.max_len);
    accepts(
        &ca.id,
        &Settings::default(),
        json!(format!("/{}", "a".repeat(max - 1))),
    );
    rejects(
        &ca.id,
        &Settings::default(),
        json!(format!("/{}", "a".repeat(max))),
    );

    let workspace = registry_field(&registry, "ai.agent.default_workspace");
    let (min, max) = (usize_of(workspace.min_len), usize_of(workspace.max_len));
    accepts(&workspace.id, &Settings::default(), json!("a".repeat(max)));
    rejects(
        &workspace.id,
        &Settings::default(),
        json!("a".repeat(max + 1)),
    );
    rejects(&workspace.id, &Settings::default(), json!(" ".repeat(min)));
}

#[test]
fn cross_rules_are_enforced_by_the_settings() {
    let registry = settings_registry();
    let mut checked = 0;
    for field in &registry.fields {
        for rule in &field.cross {
            checked += 1;
            let other = registry_field(&registry, &rule.other);
            match rule.rule {
                CrossRuleKind::Ne => {
                    let same = set(&Settings::default(), &other.id, json!(5000)).expect("valid");
                    rejects(&field.id, &same, json!(5000));
                }
                CrossRuleKind::Le => {
                    // This value goes to its smallest first (the other one still lets it), then
                    // the other drops to its smallest and this one goes just above it.
                    let low =
                        set(&Settings::default(), &field.id, json!(field.min)).expect("valid");
                    let low = set(&low, &other.id, json!(other.min)).expect("valid");
                    rejects(&field.id, &low, json!(field.min.expect("min") + 1));
                }
            }
        }
    }
    assert_eq!(checked, 3, "ne in both directions, one le");
}

#[test]
fn defaults_come_from_settings_default_and_fit_their_kind() {
    let defaults = serde_json::to_value(Settings::default()).expect("serializes");
    for field in settings_registry().fields {
        let expected = defaults
            .pointer(&format!("/{}", field.id.replace('.', "/")))
            .expect("the settings carry the field");
        assert_eq!(&field.default, expected, "{}", field.id);
        let fits = match field.kind {
            RegistryKind::Bool => field.default.is_boolean(),
            RegistryKind::Enum => field.default.as_str().is_some_and(|default| {
                field
                    .options
                    .as_ref()
                    .is_some_and(|options| options.iter().any(|option| option == default))
            }),
            RegistryKind::Int
            | RegistryKind::SizeBytes
            | RegistryKind::DurationMs
            | RegistryKind::DurationS => field.default.as_i64().is_some_and(|default| {
                field.off == Some(default)
                    || (field.min.expect("min")..=field.max.expect("max")).contains(&default)
            }),
            RegistryKind::Decimal | RegistryKind::Text => {
                field.default.is_null() || field.default.is_number() || field.default.is_string()
            }
            RegistryKind::List => field.default.is_array(),
            RegistryKind::Map => field.default.is_object() || field.default.is_null(),
        };
        assert!(
            fits,
            "{}: default {} does not fit {:?}",
            field.id, field.default, field.kind
        );
    }
}

/// Collects the dotted paths of `value` that no registry row covers; a row ends the descent.
fn uncovered(value: &Value, path: &str, ids: &BTreeSet<String>, out: &mut Vec<String>) {
    if ids.contains(path) {
        return;
    }
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (key, inner) in map {
                let next = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                uncovered(inner, &next, ids, out);
            }
        }
        _ => out.push(path.to_owned()),
    }
}

#[test]
fn every_settings_field_has_a_row_and_every_row_a_field() {
    let registry = settings_registry();
    let ids: BTreeSet<String> = registry
        .fields
        .iter()
        .map(|field| field.id.clone())
        .collect();
    assert_eq!(ids.len(), registry.fields.len(), "ids are unique");
    let defaults = serde_json::to_value(Settings::default()).expect("serializes");
    let mut missing = Vec::new();
    uncovered(&defaults, "", &ids, &mut missing);
    assert!(
        missing.is_empty(),
        "settings fields without a registry row: {missing:?}"
    );
    for id in &ids {
        assert!(
            defaults
                .pointer(&format!("/{}", id.replace('.', "/")))
                .is_some(),
            "{id} names no settings field"
        );
    }
}

#[test]
fn local_only_follows_the_sync_rules() {
    let registry = settings_registry();
    let unregistered: std::collections::BTreeMap<&str, bool> =
        UNREGISTERED_FIELDS.iter().copied().collect();
    for (id, _) in UNREGISTERED_FIELDS {
        assert!(
            !registry.fields.iter().any(|field| field.id == *id),
            "{id} is in the registry: drop it from UNREGISTERED_FIELDS"
        );
    }
    let mut local = BTreeSet::new();
    for section in SettingsSection::ALL {
        for path in local_only_fields(*section) {
            let id = format!("{}.{path}", section.as_str());
            match unregistered.get(id.as_str()) {
                Some(stays) => assert!(stays, "{id} is local-only but classified as synced"),
                None => assert!(registry_field(&registry, &id).local_only, "{id}"),
            }
            local.insert(id);
        }
    }
    for (id, stays) in UNREGISTERED_FIELDS {
        assert_eq!(*stays, local.contains(*id), "{id} is classified wrongly");
    }
    for field in &registry.fields {
        if field.id.starts_with("sync.") {
            assert!(field.local_only, "{}", field.id);
        }
    }
    assert!(!registry_field(&registry, "network.timeout_ms").local_only);
    assert!(registry_field(&registry, "ai.gateway.port").restart_required);
}

#[test]
fn constants_state_what_the_core_enforces() {
    let registry = settings_registry();
    assert_eq!(
        registry.constants["log.max_rows"],
        RegistryConstant::Number(5_000)
    );
    assert_eq!(
        registry.constants["capture.max_mb"],
        RegistryConstant::Number(200)
    );
    // The secret limit: exactly this many bytes are stored, one more is refused.
    let RegistryConstant::Number(limit) = registry.constants["secret.max_bytes"] else {
        panic!("a number");
    };
    let limit = usize::try_from(limit).expect("positive");
    let state = AppState::open_in_memory().expect("state");
    settings_secret_set_impl(&state, SettingsSecret::SearchTavily, &"k".repeat(limit))
        .expect("the limit itself is stored");
    settings_secret_set_impl(&state, SettingsSecret::SearchTavily, &"k".repeat(limit + 1))
        .expect_err("one byte more is refused");
}

#[test]
fn a_choice_of_numbers_lists_exactly_the_values_the_settings_accept() {
    let registry = settings_registry();
    let choices: Vec<&RegistryField> = registry
        .fields
        .iter()
        .filter(|field| field.values.is_some())
        .collect();
    assert_eq!(
        choices
            .iter()
            .map(|field| field.id.as_str())
            .collect::<Vec<_>>(),
        ["data.chat_retention_days"]
    );
    for field in choices {
        let values = field.values.as_ref().expect("values");
        let (min, max) = (field.min.expect("min"), field.max.expect("max"));
        assert!(values.iter().all(|value| (min..=max).contains(value)));
        assert!(values.contains(&field.default.as_i64().expect("a number default")));
        for value in values {
            accepts(&field.id, &Settings::default(), json!(value));
        }
        for other in (min..=max).filter(|number| !values.contains(number)) {
            rejects(&field.id, &Settings::default(), json!(other));
        }
    }
}
