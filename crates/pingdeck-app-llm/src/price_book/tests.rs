//! Tests of the price book: resolution precedence, offers, aliases and the page views.

use super::*;
use crate::llm::catalog::{ModelRecord, apply_sync};
use crate::llm::store::{
    Provider, effective_priced, save_pricing, save_provider, save_provider_pricing,
};
use crate::llm::wire::AuthStyle;
use pingdeck_model::model_catalog::{PriceBasisKind, PricingMode};

fn offer_record(
    family: &str,
    id: &str,
    canonical: &str,
    prices: Option<(f64, f64)>,
) -> ModelRecord {
    ModelRecord {
        family: family.into(),
        family_name: family.to_ascii_uppercase(),
        model_id: id.into(),
        name: id.into(),
        canonical_id: canonical.into(),
        input_per_mtok: prices.map(|p| p.0),
        output_per_mtok: prices.map(|p| p.1),
        currency: "USD".into(),
        ..ModelRecord::default()
    }
}

fn seed_sonnet_and_extra(c: &Connection, extra: &[ModelRecord]) {
    let mut rows = vec![
        offer_record("anthropic", "claude-sonnet-4-5", "", Some((3.0, 15.0))),
        offer_record("anthropic", "claude-haiku-4-5", "", Some((1.0, 5.0))),
        offer_record(
            "aimlapi",
            "anthropic/claude-sonnet-4.5",
            "anthropic/claude-sonnet-4-5",
            Some((3.174, 15.87)),
        ),
        offer_record(
            "openrouter",
            "anthropic/claude-sonnet-4.5",
            "anthropic/claude-sonnet-4-5",
            Some((3.3, 16.5)),
        ),
    ];
    rows.extend_from_slice(extra);
    apply_sync(c, &rows, 1, 0).unwrap();
}

fn custom(provider: &str, model: &str, input: f64) -> ModelPricing {
    ModelPricing {
        mode: PricingMode::Custom,
        input_per_mtok: Some(input),
        output_per_mtok: Some(input * 2.0),
        ..ModelPricing::dynamic(provider, model)
    }
}

fn fixed(provider: &str, model: &str, per_call: f64) -> ModelPricing {
    ModelPricing {
        mode: PricingMode::Fixed,
        per_call: Some(per_call),
        ..ModelPricing::dynamic(provider, model)
    }
}

fn pricing(provider: &str, model: &str, mode: PricingMode) -> ModelPricing {
    ModelPricing {
        mode,
        ..ModelPricing::dynamic(provider, model)
    }
}

fn saved_provider(c: &Connection, id: &str, edit: impl FnOnce(&mut Provider)) {
    let mut provider = Provider {
        id: id.to_owned(),
        ..crate::test_support::provider_template(id, "https://api.example.com", None)
    };
    edit(&mut provider);
    save_provider(c, &provider, 1).unwrap();
}

#[test]
fn the_book_and_the_single_resolver_agree() {
    let c = crate::storage::migrated_memory_connection();
    seed_sonnet_and_extra(&c, &[offer_record("lonely", "lonely-1", "", None)]);
    save_provider_pricing(&c, "p1", Some("openrouter"), 1).unwrap();
    save_pricing(&c, custom("", "claude-haiku-4-5", 2.0), 1).unwrap();
    save_pricing(&c, fixed("p2", "anthropic/claude-sonnet-4.5", 0.1), 1).unwrap();
    save_pricing(
        &c,
        pricing("p3", "claude-haiku-4-5", PricingMode::Dynamic),
        1,
    )
    .unwrap();
    let book = PriceBook::load(&c).unwrap();
    for provider in ["", "p1", "p2", "p3"] {
        for model in [
            "anthropic/claude-sonnet-4.5",
            "claude-sonnet-4-5-20250929",
            "claude-haiku-4-5",
            "lonely-1",
            "nowhere",
        ] {
            assert_eq!(
                book.priced(provider, model),
                effective_priced(&c, provider, model).unwrap(),
                "{provider} {model}"
            );
            assert_eq!(
                book.resolve(provider, model),
                crate::llm::catalog::resolve(&c, Some(provider), model).unwrap()
            );
        }
    }
}

#[test]
fn unkeyed_pricing_rows_in_snapshot_are_backfilled_on_book_load() {
    let c = crate::storage::migrated_memory_connection();
    seed_sonnet_and_extra(&c, &[]);
    let mut snapshot = load_price_book_snapshot(&c).unwrap();
    snapshot.pricing.push(KeyedPricing {
        pricing: custom("", "claude-sonnet-4-5", 5.0),
        model_key: String::new(),
    });
    let book = PriceBook::from_snapshot(snapshot);
    assert_eq!(
        book.priced("p", "anthropic/claude-sonnet-4.5")
            .unwrap()
            .price
            .input_per_mtok,
        5.0
    );
}

#[test]
fn unkeyed_pricing_rows_in_the_db_are_keyed_on_book_load() {
    let c = crate::storage::migrated_memory_connection();
    seed_sonnet_and_extra(&c, &[]);
    c.execute_batch(
        "INSERT INTO model_pricing (provider_id, model_id, mode, input_per_mtok, output_per_mtok, \
             currency, updated_at_ms) VALUES ('', 'claude-sonnet-4-5', 'custom', 5, 6, 'USD', 1);",
    )
    .unwrap();
    assert_eq!(
        PriceBook::load(&c)
            .unwrap()
            .priced("p", "anthropic/claude-sonnet-4.5")
            .unwrap()
            .price
            .input_per_mtok,
        5.0
    );
}

// The model table's price is known, free (a keyless local runtime) or unknown, and the wire
// tells the three apart.
#[test]
fn the_model_price_has_three_states() {
    use EffectivePriceState as S;
    let c = crate::storage::migrated_memory_connection();
    seed_sonnet_and_extra(&c, &[]);
    saved_provider(&c, "paid", |_| {});
    saved_provider(&c, "local", |p| {
        p.base_url = "http://127.0.0.1:11434".into();
        p.auth = AuthStyle::None;
    });
    let book = PriceBook::load(&c).unwrap();
    for (provider, model, want) in [
        ("paid", "claude-sonnet-4-5", "known"),
        ("local", "llama3", "free"),
        ("paid", "llama3", "unknown"),
    ] {
        let state = book.price_state(provider, model);
        let wire = serde_json::to_value(&state).unwrap();
        assert_eq!(wire["state"], want, "{provider} {model}");
        if let S::Known(view) = state {
            assert_eq!(wire["price"]["input_per_mtok"], 3.0);
            assert_eq!(view.basis.kind, PriceBasisKind::Official);
        }
    }
}

// A provider made from a preset's China region bills at its `-cn` distributor's list, the
// global region at the publisher's.
#[test]
fn a_cn_region_bills_at_its_distributor_price() {
    let c = crate::storage::migrated_memory_connection();
    apply_sync(
        &c,
        &[
            offer_record("alibaba", "qwen-max", "", Some((1.6, 6.4))),
            offer_record(
                "alibaba-cn",
                "qwen-max",
                "alibaba/qwen-max",
                Some((0.33, 1.32)),
            ),
        ],
        1,
        0,
    )
    .unwrap();
    saved_provider(&c, "intl", |p| {
        p.preset_id = Some("qwen_dashscope".into());
        p.base_url = "https://dashscope-intl.aliyuncs.com".into();
    });
    saved_provider(&c, "cn", |p| {
        p.preset_id = Some("qwen_dashscope".into());
        p.base_url = "https://dashscope.aliyuncs.com".into();
    });
    let book = PriceBook::load(&c).unwrap();
    for (provider, input, family) in [("intl", 1.6, "alibaba"), ("cn", 0.33, "alibaba-cn")] {
        let got = effective_priced(&c, provider, "qwen-max").unwrap().unwrap();
        assert_eq!(
            (got.price.input_per_mtok, got.basis.family.as_deref()),
            (input, Some(family)),
            "{provider}"
        );
        assert_eq!(book.priced(provider, "qwen-max"), Some(got));
    }
}
