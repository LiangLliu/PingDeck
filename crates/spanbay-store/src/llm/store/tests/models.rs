//! Models: capability matrix, list refresh and drift, overrides, measurements, bulk flags.

use rusqlite::Connection;

use super::{connection, fast_model, model, provider, seeded, single_endpoint_provider};
use crate::llm::capabilities::{Capability, CapabilitySource, Measurement, ModelCapabilities};
use crate::llm::probe::capability::CapabilityUpdate;
use crate::llm::probe::listing::Presence;
use crate::llm::store::bulk_enable::disabled_models;
use crate::llm::store::models::merge_models;
use crate::llm::store::{
    EnabledChange, FactSource, ListedModel, ModelRef, ModelSource, ProviderModel, ack_model_drift,
    apply_model_listing, delete_legacy_name, get_model, get_provider, insert_models_from_sniff,
    list_fallbacks, list_legacy_names, list_models, record_measured_context, save_legacy_name,
    save_model, save_provider, set_model_capabilities_override, set_model_fallbacks,
    set_models_enabled,
};
use spanbay_base::error::AppError;

fn caps(bits: &[(Capability, bool, CapabilitySource)]) -> ModelCapabilities {
    let mut matrix = ModelCapabilities::default();
    for (cap, sup, src) in bits {
        matrix.apply(*cap, *sup, *src);
    }
    matrix
}

fn listed(id: &str) -> ListedModel {
    ListedModel {
        id: id.into(),
        ..ListedModel::default()
    }
}

fn measured(capability: Capability, supported: bool) -> CapabilityUpdate {
    CapabilityUpdate {
        capability,
        measurement: if supported {
            Measurement::Yes
        } else {
            Measurement::No
        },
        dimension: None,
    }
}

fn rec_meas(
    c: &rusqlite::Connection,
    model_id: &str,
    updates: &[CapabilityUpdate],
) -> Result<bool, AppError> {
    crate::llm::store::record_measured(c, "p1", model_id, updates, 1_000)
}

fn row(c: &rusqlite::Connection, id: &str) -> ProviderModel {
    get_model(c, "p1", id).unwrap().unwrap()
}

#[test]
fn a_model_save_keeps_measured_and_listed_values() {
    let c = connection();
    seeded(&c, "p1", "k1");
    let mut m = model("p1", "m");
    m.vision = true;
    m.first_seen_ms = Some(5);
    save_model(&c, &m).unwrap();
    assert_eq!(
        row(&c, "m").capabilities.vision.source,
        CapabilitySource::Catalog
    );

    rec_meas(&c, "m", &[measured(Capability::Vision, false)]).unwrap();
    apply_model_listing(&c, "p1", &[listed("other")], true, 9).unwrap();
    ack_model_drift(&c, "p1", 10).unwrap();

    let mut stale = model("p1", "m");
    stale.notes = Some("hand edit".into());
    stale.enabled = false;
    stale.vision = true;
    stale.tools = true;
    save_model(&c, &stale).unwrap();
    let stored = row(&c, "m");
    assert!(!stored.vision && stored.tools);
    assert_eq!(
        stored.capabilities.vision.source,
        CapabilitySource::Measured
    );
    assert_eq!(stored.presence, Presence::NotInProvider);
    assert_eq!(
        (stored.first_seen_ms, stored.new_cleared_at_ms),
        (Some(5), Some(10))
    );

    let mut refreshed = model("p1", "m");
    refreshed.context = Some(4096);
    merge_models(
        &c,
        "p1",
        ModelSource::Live,
        &[refreshed, model("p1", "fresh")],
    )
    .unwrap();
    // A save never writes the listed value of a stored row; only the list refresh does.
    assert_eq!(row(&c, "m").context, None);
    assert_eq!(
        row(&c, "m").capabilities.vision.source,
        CapabilitySource::Measured
    );
    assert!(get_model(&c, "p1", "fresh").unwrap().is_some());
}

#[test]
fn a_list_refresh_reports_drift_presence_and_metadata() {
    let c2 = connection();
    seeded(&c2, "p1", "k1");
    for id in ["a", "b", "c", "d"] {
        save_model(&c2, &model("p1", id)).unwrap();
    }
    assert!(matches!(
        apply_model_listing(&c2, "nope", &[], true, 1),
        Err(AppError::NotFound { .. })
    ));

    rec_meas(&c2, "a", &[measured(Capability::Vision, false)]).unwrap();
    set_model_capabilities_override(
        &c2,
        "p1",
        "a",
        &caps(&[(Capability::Tools, false, CapabilitySource::UserOverride)]),
    )
    .unwrap();
    set_models_enabled(
        &c2,
        "p1",
        &[EnabledChange {
            model_id: "a".into(),
            enabled: false,
            only_if_current: None,
        }],
    )
    .unwrap();

    let mut entry_a = listed("a");
    entry_a.context = Some(128_000);
    entry_a.max_output = Some(4_096);
    entry_a.capabilities = caps(&[
        (Capability::Vision, true, CapabilitySource::Declared),
        (Capability::Tools, true, CapabilitySource::Declared),
        (Capability::Reasoning, true, CapabilitySource::UserOverride),
    ]);
    let mut dup_a = listed("a");
    dup_a.context = Some(999);
    let drift = apply_model_listing(
        &c2,
        "p1",
        &[
            entry_a,
            dup_a,
            listed("b"),
            listed("c"),
            listed("x"),
            listed("x"),
        ],
        true,
        500,
    )
    .unwrap();
    assert_eq!(
        (drift.added, drift.removed),
        (vec!["x".to_owned()], vec!["d".to_owned()])
    );
    assert_eq!(row(&c2, "d").presence, Presence::NotInProvider);

    let a = row(&c2, "a");
    assert!(!a.enabled && a.context == Some(128_000) && a.max_output == Some(4_096));
    assert!(!a.capabilities.vision.supported && !a.capabilities.tools.supported);
    assert_eq!(a.capabilities.reasoning.source, CapabilitySource::Declared);

    ack_model_drift(&c2, "p1", 600).unwrap();
    assert!(
        get_provider(&c2, "p1")
            .unwrap()
            .unwrap()
            .model_drift
            .is_none()
    );
    assert_eq!(
        (row(&c2, "d").presence, row(&c2, "a").new_cleared_at_ms),
        (Presence::NotInProvider, Some(600))
    );
}

#[test]
fn a_declared_context_survives_a_list_refresh_until_restored_to_automatic() {
    let c = connection();
    seeded(&c, "p1", "k1");
    let mut entry = listed("m");
    entry.context = Some(128_000);
    insert_models_from_sniff(&c, "p1", &[(entry.clone(), true)], 1).unwrap();
    let mut edited = row(&c, "m");
    edited.context = Some(64_000);
    edited.context_source = Some(FactSource::Declared);
    save_model(&c, &edited).unwrap();

    entry.context = Some(200_000);
    apply_model_listing(&c, "p1", &[entry], true, 2).unwrap();
    let kept = row(&c, "m");
    assert_eq!(
        (kept.context, kept.context_source, kept.listed_context),
        (Some(64_000), Some(FactSource::Declared), Some(200_000))
    );

    // "Restore to automatic": the payload says the value is not the user's.
    let mut auto = kept;
    auto.context_source = Some(FactSource::Listed);
    save_model(&c, &auto).unwrap();
    let restored = row(&c, "m");
    assert_eq!(
        (restored.context, restored.context_source),
        (Some(200_000), Some(FactSource::Listed))
    );
    // A measured window beats the list, never the user.
    record_measured_context(&c, "p1", "m", 150_000).unwrap();
    assert_eq!(row(&c, "m").context_source, Some(FactSource::Measured));
}

#[test]
fn a_synced_model_keeps_who_set_its_facts() {
    // The sync file carries `ProviderModel` as JSON (`spanbay-app-sync` docs and apply).
    let (a, b) = (connection(), connection());
    for c in [&a, &b] {
        seeded(c, "p1", "k1");
    }
    let mut entry = listed("m");
    entry.max_output = Some(8_192);
    insert_models_from_sniff(&a, "p1", &[(entry, true)], 1).unwrap();
    let mut edited = row(&a, "m");
    edited.context = Some(32_000);
    edited.context_source = Some(FactSource::Declared);
    save_model(&a, &edited).unwrap();
    let file = serde_json::to_string(&row(&a, "m")).unwrap();
    save_model(&b, &serde_json::from_str(&file).unwrap()).unwrap();
    let back = serde_json::to_string(&row(&b, "m")).unwrap();
    save_model(&a, &serde_json::from_str(&back).unwrap()).unwrap();
    for c in [&a, &b] {
        let m = row(c, "m");
        assert_eq!(
            (
                m.context,
                m.context_source,
                m.max_output,
                m.max_output_source
            ),
            (
                Some(32_000),
                Some(FactSource::Declared),
                Some(8_192),
                Some(FactSource::Listed)
            )
        );
    }
}

#[test]
fn a_drift_marked_as_seen_is_not_reported_again() {
    let c = connection();
    seeded(&c, "p1", "k1");
    insert_models_from_sniff(&c, "p1", &[(listed("gone"), true)], 1).unwrap();
    crate::llm::store::replace_key_models(&c, "k1", &[("new".to_owned(), None)], None, 2).unwrap();
    let first = apply_model_listing(&c, "p1", &[listed("new")], true, 3).unwrap();
    assert_eq!(
        (first.added, first.removed),
        (vec!["new".to_owned()], vec!["gone".to_owned()])
    );
    ack_model_drift(&c, "p1", 4).unwrap();
    crate::llm::store::replace_key_models(&c, "k1", &[("new".to_owned(), None)], None, 5).unwrap();
    let again = apply_model_listing(&c, "p1", &[listed("new")], true, 6).unwrap();
    assert!(again.added.is_empty() && again.removed.is_empty());
}

#[test]
fn only_a_complete_list_removes_and_only_live_rows() {
    let c = connection();
    seeded(&c, "p1", "k1");
    save_model(&c, &model("p1", "live")).unwrap();
    for (id, source) in [
        ("hand", ModelSource::Manual),
        ("preset", ModelSource::Preset),
    ] {
        save_model(
            &c,
            &ProviderModel {
                source,
                ..model("p1", id)
            },
        )
        .unwrap();
    }
    // A truncated list, or an empty one, decides no removal.
    for (list, complete) in [(vec![listed("x")], false), (Vec::new(), true)] {
        let drift = apply_model_listing(&c, "p1", &list, complete, 5).unwrap();
        assert!(!drift.listing_complete && drift.removed.is_empty());
        assert_eq!(row(&c, "live").presence, Presence::Listed);
    }
    let drift = apply_model_listing(&c, "p1", &[listed("x")], true, 6).unwrap();
    assert!(drift.listing_complete);
    assert_eq!(drift.removed, ["live"]);
    assert_eq!(
        ["live", "hand", "preset"].map(|id| row(&c, id).presence),
        [Presence::NotInProvider, Presence::Listed, Presence::Listed]
    );
}

#[test]
fn capability_hierarchy_sniff_insert_and_conditional_enable() {
    let c = connection();
    seeded(&c, "p1", "k1");
    for id in ["a", "b", "c", "d"] {
        save_model(&c, &model("p1", id)).unwrap();
    }
    assert!(matches!(
        set_model_capabilities_override(&c, "p1", "nope", &ModelCapabilities::default()),
        Err(AppError::NotFound { .. })
    ));
    assert!(!rec_meas(&c, "ghost", &[measured(Capability::Vision, true)]).unwrap());
    assert!(rec_meas(&c, "a", &[measured(Capability::Vision, true)]).unwrap());
    assert!(!rec_meas(&c, "a", &[measured(Capability::Vision, true)]).unwrap());

    let updated = set_model_capabilities_override(
        &c,
        "p1",
        "a",
        &caps(&[
            (Capability::Vision, true, CapabilitySource::Measured),
            (Capability::Tools, true, CapabilitySource::UserOverride),
        ]),
    )
    .unwrap();
    assert_eq!(
        (
            updated.capabilities.vision.source,
            updated.capabilities.tools.source
        ),
        (CapabilitySource::Measured, CapabilitySource::UserOverride)
    );
    assert!(!rec_meas(&c, "a", &[measured(Capability::Tools, false)]).unwrap());

    let reset =
        set_model_capabilities_override(&c, "p1", "a", &ModelCapabilities::default()).unwrap();
    assert_eq!(
        (
            reset.capabilities.known(Capability::Tools),
            reset.capabilities.vision.source
        ),
        (None, CapabilitySource::Measured)
    );

    // Sniff model insertion, conditional bulk enable, and disabled_models list.
    assert!(matches!(
        insert_models_from_sniff(&c, "nope", &[(listed("a"), true)], 1),
        Err(AppError::NotFound { .. })
    ));
    assert!(matches!(
        insert_models_from_sniff(&c, "p1", &[(listed("ok"), true), (listed("  "), true)], 1),
        Err(AppError::Validation { .. })
    ));
    assert!(get_model(&c, "p1", "ok").unwrap().is_none());

    let mut chat = listed("chat-1");
    chat.context = Some(200_000);
    let created = insert_models_from_sniff(
        &c,
        "p1",
        &[
            (listed("a"), false),
            (chat, true),
            (listed("image-1"), false),
        ],
        900,
    )
    .unwrap();
    assert_eq!(created, 2);
    assert!(row(&c, "a").enabled && !row(&c, "image-1").enabled);

    let changed = set_models_enabled(
        &c,
        "p1",
        &[
            EnabledChange {
                model_id: "a".into(),
                enabled: false,
                only_if_current: Some(true),
            },
            EnabledChange {
                model_id: "b".into(),
                enabled: false,
                only_if_current: Some(false),
            },
        ],
    )
    .unwrap();
    assert_eq!(changed, vec!["a"]);
    save_provider(&c, &provider("p2"), 1).unwrap();
    save_model(&c, &model("p2", "x")).unwrap();
    assert_eq!(
        disabled_models(&c).unwrap().into_iter().collect::<Vec<_>>(),
        vec![
            ("p1".to_owned(), "a".to_owned()),
            ("p1".to_owned(), "image-1".to_owned())
        ]
    );
}

#[test]
fn merge_models_keeps_manual_entries_and_enabled_flags() {
    let c = connection();
    save_provider(&c, &single_endpoint_provider("p1"), 1).unwrap();
    save_model(&c, &fast_model("p1", "manual-one", ModelSource::Manual)).unwrap();
    merge_models(
        &c,
        "p1",
        ModelSource::Live,
        &[
            fast_model("p1", "a", ModelSource::Live),
            fast_model("p1", "b", ModelSource::Live),
        ],
    )
    .unwrap();
    let mut disabled = list_models(&c, Some("p1"))
        .unwrap()
        .into_iter()
        .find(|m| m.model_id == "a")
        .unwrap();
    disabled.enabled = false;
    save_model(&c, &disabled).unwrap();
    merge_models(
        &c,
        "p1",
        ModelSource::Live,
        &[
            fast_model("p1", "a", ModelSource::Live),
            fast_model("p1", "manual-one", ModelSource::Live),
        ],
    )
    .unwrap();
    let models = list_models(&c, Some("p1")).unwrap();
    let ids: Vec<&str> = models.iter().map(|m| m.model_id.as_str()).collect();
    assert_eq!(ids, vec!["a", "manual-one"]);
    assert!(!models[0].enabled);
    assert_eq!(models[1].source, ModelSource::Manual);
}

#[test]
fn fallback_chains_keep_their_order_and_refuse_bad_shapes() {
    let c = connection();
    save_provider(&c, &single_endpoint_provider("p1"), 1).unwrap();
    let to = |model: &str| ModelRef {
        provider_id: "p1".into(),
        model: model.into(),
    };
    set_model_fallbacks(&c, &to("m"), &[to("b"), to("a")]).unwrap();
    assert_eq!(
        list_fallbacks(&c).unwrap()[&to("m")],
        vec![to("b"), to("a")]
    );
    let six: Vec<ModelRef> = (0..6).map(|n| to(&format!("x{n}"))).collect();
    for bad in [vec![to("m")], vec![to("a"), to("a")], six] {
        assert!(matches!(
            set_model_fallbacks(&c, &to("m"), &bad),
            Err(AppError::Validation { .. })
        ));
    }
    assert_eq!(
        list_fallbacks(&c).unwrap()[&to("m")],
        vec![to("b"), to("a")]
    );
}

#[test]
fn legacy_names_map_to_one_model_and_can_be_forgotten() {
    let c = connection();
    save_provider(&c, &single_endpoint_provider("p1"), 1).unwrap();
    let to = |model: &str| ModelRef {
        provider_id: "p1".into(),
        model: model.into(),
    };
    save_legacy_name(&c, "fast", &to("a")).unwrap();
    save_legacy_name(&c, "fast", &to("b")).unwrap();
    save_legacy_name(&c, "deep", &to("a")).unwrap();
    let names = |c: &Connection| -> Vec<(String, ModelRef)> {
        list_legacy_names(c)
            .unwrap()
            .into_iter()
            .map(|legacy| (legacy.name, legacy.target))
            .collect()
    };
    assert_eq!(
        names(&c),
        [("deep".to_owned(), to("a")), ("fast".to_owned(), to("b"))]
    );
    delete_legacy_name(&c, "deep").unwrap();
    assert_eq!(names(&c), [("fast".to_owned(), to("b"))]);
}
