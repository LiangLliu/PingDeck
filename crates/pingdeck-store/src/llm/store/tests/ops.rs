//! Create-from-sniff, key checks, bulk enable with undo, direct-id listing, measured states and
//! the custom-query rules.

use std::collections::BTreeMap;

use rusqlite::Connection;

use super::{connection, key, model, provider, raw_text, secret, seeded};
use crate::llm::balance_state::{
    BalanceAuth, BalanceReport, BalanceScope, BalanceUnit, CustomBalanceQuery, ResolvedHeaders,
    StoredBalance, UnitName,
};
use crate::llm::capabilities::{Capability, CapabilitySource, Measurement, ModelCapabilities};
use crate::llm::probe::apply::{ApplyReport, PreviousState};
use crate::llm::probe::capability::CapabilityUpdate;
use crate::llm::probe::run::CellRecord;
use crate::llm::probe::{ProbeStatus, ProbeTest};
use crate::llm::status::{ProviderStatus, ReasonCode, provider_status};
use crate::llm::store::sniff_create::record_sniff_checks;
use crate::llm::store::{
    KeyCheck, ListedModel, MAX_APPLY_LOG, ModelSource, NewKeyFromSniff, NewProviderFromSniff,
    ProbeResult, SniffChecks, SniffPersist, create_provider_from_sniff, get_model, get_probe_apply,
    get_provider, insert_models_from_sniff, list_keys, list_models, list_pricing,
    list_probe_results, persist_cell, provider_status_facts, record_measured, record_probe_apply,
    save_key, save_model, set_custom_balance, set_model_capabilities_override,
    set_models_enabled_bulk, undo_bulk,
};
use crate::llm::wire::GatewayFamily;
use pingdeck_base::error::AppError;
use pingdeck_llm_domain::llm::sniff_summary::SniffSummary;
use pingdeck_model::model_catalog::{ModelPricing, PricingMode};

const NOW: i64 = 1_800_000_000_000;

fn listed(id: &str) -> ListedModel {
    ListedModel {
        id: id.into(),
        ..ListedModel::default()
    }
}

fn accepted() -> SniffChecks {
    let ok = KeyCheck {
        ok: true,
        status: Some(200),
        latency_ms: Some(80),
    };
    SniffChecks {
        report_id: "rep1".into(),
        connectivity: ok,
        auth: ok,
    }
}

fn new_provider(checks: SniffChecks) -> NewProviderFromSniff {
    NewProviderFromSniff {
        provider: provider("p1"),
        sniff: SniffPersist {
            family: GatewayFamily::NewApi,
            quota_per_unit: Some(500_000.0),
            display_in_currency: Some(true),
            evidence: Vec::new(),
            summary: SniffSummary {
                family: GatewayFamily::NewApi,
                brand_name: None,
                version: None,
                confidence_score: 90,
                confirmed: true,
                runner_up: None,
                requests_used: 5,
                elapsed_ms: 900,
                tokens_spent: 0,
                quota_is_remaining: false,
                scanned_at_ms: NOW,
            },
        },
        keys: vec![NewKeyFromSniff {
            key: key("k1", "ignored"),
            secret: Some("sk-typed-secret-12".into()),
            balance: Some(StoredBalance {
                last_good: Some(BalanceReport {
                    supported: true,
                    unit: BalanceUnit::Usd,
                    remaining: Some(12.4),
                    checked_at_ms: NOW,
                    ..BalanceReport::default()
                }),
                ..StoredBalance::default()
            }),
            checks: Some(checks),
        }],
        models: vec![
            (listed("chat-1"), true),
            (listed("chat-2"), true),
            (listed("image-1"), false),
        ],
        manual_models: vec![model("ignored", "typed-by-hand")],
        declared_prices: Vec::new(),
    }
}

#[test]
fn create_from_sniff_rollback_and_key_checks() {
    let c = connection();
    let mut bad = new_provider(accepted());
    bad.models.push((listed("  "), true));
    assert!(matches!(
        create_provider_from_sniff(&c, &bad, NOW),
        Err(AppError::Validation { .. })
    ));
    assert!(get_provider(&c, "p1").unwrap().is_none());
    assert_eq!(secret(&c, "llm.key.k1"), None);

    let mut priced = new_provider(accepted());
    priced.declared_prices = vec![ModelPricing {
        mode: PricingMode::Custom,
        input_per_mtok: Some(1.25),
        output_per_mtok: Some(10.0),
        ..ModelPricing::dynamic("", "chat-1")
    }];
    let created = create_provider_from_sniff(&c, &priced, NOW).unwrap();
    assert_eq!(created.gateway_family, GatewayFamily::NewApi);
    // The price the host's list declared is the provider's own row of the model.
    let rows = list_pricing(&c, Some("chat-1")).unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| (row.provider_id.as_str(), row.input_per_mtok))
            .collect::<Vec<_>>(),
        [("p1", Some(1.25))]
    );
    assert_eq!(
        secret(&c, "llm.key.k1").as_deref(),
        Some("sk-typed-secret-12")
    );
    assert_eq!(
        (
            list_keys(&c, Some("p1")).unwrap().len(),
            list_models(&c, Some("p1")).unwrap().len()
        ),
        (1, 4)
    );
    assert!(!get_model(&c, "p1", "image-1").unwrap().unwrap().enabled);
    assert_eq!(
        get_model(&c, "p1", "typed-by-hand")
            .unwrap()
            .unwrap()
            .source,
        ModelSource::Manual
    );
    let results = list_probe_results(&c, Some("k1")).unwrap();
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .all(|r| r.run_id.as_deref() == Some("sniff:rep1"))
    );
    assert!(matches!(
        create_provider_from_sniff(&c, &new_provider(accepted()), NOW),
        Err(AppError::Conflict { .. })
    ));

    // Key checks record rejected and unreachable states.
    let c2 = connection();
    let rejected = SniffChecks {
        auth: KeyCheck {
            ok: false,
            status: Some(401),
            latency_ms: Some(80),
        },
        ..accepted()
    };
    create_provider_from_sniff(&c2, &new_provider(rejected), NOW).unwrap();
    let facts = provider_status_facts(&c2, NOW + 1_000)
        .unwrap()
        .into_iter()
        .find(|f| f.provider_id == "p1")
        .unwrap();
    let st = provider_status(&facts, NOW + 1_000);
    assert_eq!(
        (st.status, st.reasons[0].code),
        (ProviderStatus::NeedsAttention, ReasonCode::AllKeysRejected)
    );

    save_key(&c2, &key("k2", "p1"), Some("sk-secret-123456"), 1).unwrap();
    let down = SniffChecks {
        connectivity: KeyCheck {
            ok: false,
            status: None,
            latency_ms: None,
        },
        ..accepted()
    };
    record_sniff_checks(&c2, "p1", "k2", &down, NOW).unwrap();
    let conn = list_probe_results(&c2, Some("k2"))
        .unwrap()
        .into_iter()
        .find(|r| r.test == "connectivity")
        .unwrap();
    assert_eq!(conn.status_word, "unreachable");
    assert!(record_sniff_checks(&c2, "p1", "nope", &accepted(), NOW).is_err());
}

#[test]
fn bulk_switches_and_probe_applies_are_undone() {
    let c = connection();
    seeded(&c, "p1", "k1");
    for id in ["m0", "m1", "m2"] {
        let mut m = model("p1", id);
        m.enabled = false;
        save_model(&c, &m).unwrap();
    }

    let noop = set_models_enabled_bulk(
        &c,
        "p1",
        &[("m0".to_owned(), false), ("ghost".to_owned(), true)],
        NOW,
    )
    .unwrap();
    assert!(noop.previous.is_empty() && get_probe_apply(&c, &noop.id).unwrap().is_none());

    let report = set_models_enabled_bulk(
        &c,
        "p1",
        &[
            ("m0".to_owned(), true),
            ("m0".to_owned(), false),
            ("m1".to_owned(), true),
            ("m2".to_owned(), true),
        ],
        NOW,
    )
    .unwrap();
    assert_eq!(report.previous.len(), 3);
    set_models_enabled_bulk(&c, "p1", &[("m1".to_owned(), false)], NOW + 1).unwrap();
    let undone = undo_bulk(&c, &report.id).unwrap();
    assert_eq!(
        (undone.restored, undone.kept),
        (
            vec!["m0".to_owned(), "m2".to_owned()],
            vec!["m1".to_owned()]
        )
    );
    assert!(matches!(
        undo_bulk(&c, &report.id),
        Err(AppError::NotFound { .. })
    ));

    insert_models_from_sniff(&c, "p1", &[(listed("fresh"), true)], NOW).unwrap();
    let apply = ApplyReport {
        enabled: vec![],
        disabled: vec![],
        created: vec!["fresh".into()],
        previous: vec![PreviousState {
            model_id: "fresh".into(),
            previous_enabled: false,
            applied_enabled: true,
        }],
        capabilities: Vec::new(),
    };
    record_probe_apply(&c, "apply-0", "run-1", "p1", &apply, NOW).unwrap();
    undo_bulk(&c, "apply-0").unwrap();
    assert!(!get_model(&c, "p1", "fresh").unwrap().unwrap().enabled);

    for n in 0..=MAX_APPLY_LOG {
        record_probe_apply(&c, &format!("a-{n}"), "run", "p1", &apply, NOW + n as i64).unwrap();
    }
    assert!(get_probe_apply(&c, "a-0").unwrap().is_none());
    assert!(
        get_probe_apply(&c, &format!("a-{MAX_APPLY_LOG}"))
            .unwrap()
            .is_some()
    );
}

#[test]
fn measured_capabilities_move_only_by_measurement() {
    let c = connection();
    seeded(&c, "p1", "k1");
    save_model(&c, &model("p1", "m")).unwrap();
    let upd = |measurement| {
        [CapabilityUpdate {
            capability: Capability::Tools,
            measurement,
            dimension: None,
        }]
    };
    let tools = |c: &Connection| get_model(c, "p1", "m").unwrap().unwrap().capabilities.tools;
    record_measured(&c, "p1", "m", &upd(Measurement::NotObserved), 1).unwrap();
    record_measured(&c, "p1", "m", &upd(Measurement::NotObserved), 2).unwrap();
    assert_eq!((tools(&c).samples, tools(&c).not_observed), (2, true));

    let cell = CellRecord {
        provider_id: "p1".into(),
        key_id: "k1".into(),
        model_id: "m".into(),
        test: ProbeTest::Tools,
        status: ProbeStatus::Unsupported,
        reason: String::new(),
        skip: None,
        result: Some(ProbeResult {
            key_id: "k1".into(),
            model_id: "m".into(),
            endpoint: "openai_chat".into(),
            test: "tools".into(),
            ok: false,
            checked_at_ms: 9_000,
            ..ProbeResult::default()
        }),
        capability: Some(upd(Measurement::No)[0]),
    };
    // A probe result is evidence only: persisting it leaves the matrix alone.
    persist_cell(&c, "run-1", &cell, 9_500).unwrap();
    assert_eq!((tools(&c).samples, tools(&c).not_observed), (2, true));

    record_measured(&c, "p1", "m", &upd(Measurement::Yes), 10_000).unwrap();
    assert!(!record_measured(&c, "p1", "m", &upd(Measurement::NotObserved), 11_000).unwrap());
    assert!(tools(&c).supported);

    let mut matrix = ModelCapabilities::default();
    matrix.apply(Capability::Tools, false, CapabilitySource::UserOverride);
    set_model_capabilities_override(&c, "p1", "m", &matrix).unwrap();
    assert!(!record_measured(&c, "p1", "m", &upd(Measurement::Yes), 12_000).unwrap());
}

#[test]
fn a_custom_balance_needs_a_scope_and_a_confirmed_other_host() {
    let c = connection();
    seeded(&c, "p1", "k1");
    let make = |url: &str, scope: Option<BalanceScope>, cross_host: bool| CustomBalanceQuery {
        url: url.into(),
        headers: vec![],
        auth: BalanceAuth::Bearer,
        remaining_path: "$.data.available".into(),
        used_path: None,
        total_path: None,
        unit: UnitName::Usd,
        scope,
        unit_label: None,
        divisor: None,
        cross_host,
        resolved_headers: ResolvedHeaders::default(),
    };
    assert!(matches!(
        set_custom_balance(
            &c,
            "p1",
            Some(&make("https://example.test/b", None, false)),
            &BTreeMap::new(),
            1
        ),
        Err(AppError::Validation { .. })
    ));
    assert!(matches!(
        set_custom_balance(
            &c,
            "p1",
            Some(&make(
                "https://billing.other.test/b",
                Some(BalanceScope::Account),
                false
            )),
            &BTreeMap::new(),
            1
        ),
        Err(AppError::ConfirmationRequired { .. })
    ));
    set_custom_balance(
        &c,
        "p1",
        Some(&make(
            "https://billing.other.test/b",
            Some(BalanceScope::Account),
            true,
        )),
        &BTreeMap::new(),
        2,
    )
    .unwrap();
    assert!(
        get_provider(&c, "p1")
            .unwrap()
            .unwrap()
            .custom_balance
            .unwrap()
            .cross_host
    );
    set_custom_balance(
        &c,
        "p1",
        Some(&make(
            "https://example.test/b",
            Some(BalanceScope::Key),
            true,
        )),
        &BTreeMap::new(),
        3,
    )
    .unwrap();
    assert!(
        raw_text(&c, "SELECT custom_balance_json FROM llm_providers")
            .contains("\"cross_host\":false")
    );
}
