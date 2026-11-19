use rusqlite::Connection;

use super::*;
use crate::llm::store::test_support::connection;
use crate::llm::store::tests::provider;
use crate::llm::store::{Provider, save_provider};
use crate::llm::usage::{CostUsage, PriceSource, cost, request_cost};
use crate::llm::wire::AuthStyle;
use pingdeck_model::model_catalog::{ModelAlias, PriceBasisKind};

fn override_for(provider: &str, model: &str, input: f64) -> PriceOverride {
    PriceOverride {
        provider_id: provider.into(),
        model_id: model.into(),
        input_per_mtok: input,
        output_per_mtok: input * 2.0,
        cache_read_per_mtok: None,
        cache_write_per_mtok: None,
        reasoning_per_mtok: None,
        currency: "usd".into(),
        updated_at_ms: 0,
    }
}

fn catalog_row(c: &Connection, family: &str, id: &str, input: Option<f64>, output: Option<f64>) {
    c.execute(
        "INSERT INTO model_catalog (family, model_id, input_per_mtok, output_per_mtok, source) \
         VALUES (?1, ?2, ?3, ?4, 'models.dev')",
        params![family, id, input, output],
    )
    .unwrap();
    crate::llm::catalog::regroup(c).unwrap();
}

fn pricing(provider: &str, model: &str, mode: PricingMode) -> ModelPricing {
    ModelPricing {
        mode,
        ..ModelPricing::dynamic(provider, model)
    }
}

fn custom(provider: &str, model: &str, input: f64) -> ModelPricing {
    ModelPricing {
        input_per_mtok: Some(input),
        output_per_mtok: Some(input * 2.0),
        ..pricing(provider, model, PricingMode::Custom)
    }
}

fn fixed(provider: &str, model: &str, per_call: f64) -> ModelPricing {
    ModelPricing {
        per_call: Some(per_call),
        ..pricing(provider, model, PricingMode::Fixed)
    }
}

fn price(c: &Connection, provider: &str, model: &str) -> Option<Price> {
    effective_price(c, provider, model).unwrap()
}

#[test]
fn overrides_are_validated_normalised_and_replaced() {
    let c = connection();
    let saved = save_price_override(&c, override_for("p1", "m1", 1.0), 77).unwrap();
    assert_eq!((saved.currency.as_str(), saved.updated_at_ms), ("USD", 77));
    save_price_override(&c, override_for("p1", "m1", 3.0), 88).unwrap();
    save_price_override(&c, override_for("p2", "m1", 5.0), 88).unwrap();
    let map = price_overrides(&c).unwrap();
    assert_eq!(map.len(), 2);
    let stored = &map[&("p1".to_owned(), "m1".to_owned())];
    assert_eq!((stored.input_per_mtok, stored.updated_at_ms), (3.0, 88));
    for bad in [
        override_for("p1", "m1", -1.0),
        override_for("p1", "m1", f64::NAN),
        override_for("", "m1", 1.0),
        PriceOverride {
            currency: "dollars".into(),
            ..override_for("p1", "m1", 1.0)
        },
        PriceOverride {
            cache_read_per_mtok: Some(2_000_000.0),
            ..override_for("p1", "m1", 1.0)
        },
    ] {
        assert!(matches!(
            save_price_override(&c, bad, 1),
            Err(AppError::Validation { .. })
        ));
    }
    assert_eq!(price_overrides(&c).unwrap().len(), 2);
}

#[test]
fn the_override_view_lists_only_provider_scoped_custom_rows() {
    let c = connection();
    save_pricing(&c, custom("", "m1", 1.0), 1).unwrap();
    save_pricing(&c, fixed("p1", "m1", 0.5), 1).unwrap();
    save_pricing(&c, pricing("p2", "m1", PricingMode::Dynamic), 1).unwrap();
    save_pricing(&c, custom("p3", "m1", 2.0), 1).unwrap();
    let overrides = price_overrides(&c).unwrap();
    assert_eq!(overrides.len(), 1);
    assert_eq!(
        overrides[&("p3".to_owned(), "m1".to_owned())].input_per_mtok,
        2.0
    );
    // Deleting an override never removes the other kinds of row.
    assert!(!delete_price_override(&c, "p1", "m1").unwrap());
    assert!(!delete_price_override(&c, "", "m1").unwrap());
    assert!(delete_price_override(&c, "p3", "m1").unwrap());
    assert!(!delete_price_override(&c, "p3", "m1").unwrap());
    assert_eq!(list_pricing(&c, None).unwrap().len(), 3);
}

#[test]
fn a_global_dynamic_save_deletes_the_row_and_a_provider_dynamic_row_stays() {
    let c = connection();
    save_pricing(&c, custom("", "m1", 1.0), 5).unwrap();
    assert_eq!(list_pricing(&c, Some("m1")).unwrap().len(), 1);
    let back = save_pricing(&c, pricing("", "m1", PricingMode::Dynamic), 9).unwrap();
    assert_eq!((back.mode, back.updated_at_ms), (PricingMode::Dynamic, 9));
    assert!(list_pricing(&c, Some("m1")).unwrap().is_empty());
    let kept = save_pricing(&c, pricing("p1", "m1", PricingMode::Dynamic), 9).unwrap();
    assert_eq!(kept.provider_id, "p1");
    assert_eq!(list_pricing(&c, None).unwrap().len(), 1);
    assert!(delete_pricing(&c, "p1", "m1").unwrap());
    assert!(!delete_pricing(&c, "p1", "m1").unwrap());
}

#[test]
fn pricing_lists_by_model_and_validates_per_mode() {
    let c = connection();
    save_pricing(&c, fixed("p1", "m1", 0.02), 1).unwrap();
    save_pricing(&c, custom("p1", "m2", 1.0), 1).unwrap();
    assert_eq!(list_pricing(&c, Some("m2")).unwrap().len(), 1);
    assert_eq!(list_pricing(&c, None).unwrap().len(), 2);
    for bad in [
        pricing("p1", "m1", PricingMode::Fixed),
        pricing("p1", "m1", PricingMode::Custom),
        fixed("p1", "m1", -1.0),
        ModelPricing {
            currency: "x".into(),
            ..fixed("p1", "m1", 1.0)
        },
    ] {
        assert!(matches!(
            save_pricing(&c, bad, 1),
            Err(AppError::Validation { .. })
        ));
    }
}

#[test]
fn the_effective_price_walks_provider_then_global_then_catalog_then_none() {
    let c = connection();
    catalog_row(&c, "deepseek", "deepseek-chat", Some(0.27), Some(1.1));
    // ④ none: no row anywhere, and a catalog row without a full price.
    assert_eq!(price(&c, "p1", "unknown-model"), None);
    catalog_row(&c, "x", "half-priced", Some(1.0), None);
    assert_eq!(price(&c, "p1", "half-priced"), None);

    // ③ the catalog.
    let catalog = price(&c, "p1", "deepseek-chat").unwrap();
    assert_eq!(
        (
            catalog.input_per_mtok,
            catalog.source,
            catalog.currency.as_str()
        ),
        (0.27, PriceSource::Catalog, "USD")
    );

    // ② a global row applies to every provider.
    save_pricing(&c, custom("", "deepseek-chat", 3.0), 1).unwrap();
    for provider in ["p1", "p2"] {
        let global = price(&c, provider, "deepseek-chat").unwrap();
        assert_eq!(
            (global.input_per_mtok, global.source),
            (3.0, PriceSource::Override)
        );
    }

    // ① the provider's own row wins over the global one, for that provider only.
    save_pricing(&c, custom("p1", "deepseek-chat", 1.0), 1).unwrap();
    assert_eq!(
        price(&c, "p1", "deepseek-chat").unwrap().input_per_mtok,
        1.0
    );
    assert_eq!(
        price(&c, "p2", "deepseek-chat").unwrap().input_per_mtok,
        3.0
    );

    // An explicit dynamic row opts that provider out of the global override: catalog again.
    save_pricing(&c, pricing("p2", "deepseek-chat", PricingMode::Dynamic), 1).unwrap();
    let opted_out = price(&c, "p2", "deepseek-chat").unwrap();
    assert_eq!(
        (opted_out.input_per_mtok, opted_out.source),
        (0.27, PriceSource::Catalog)
    );
    // ... and a global row prices a model with no catalog row at all.
    save_pricing(&c, custom("", "local-model", 0.0), 1).unwrap();
    assert!(price(&c, "p1", "local-model").is_some());
    // The legacy helper and the resolver agree on a plain override.
    save_price_override(&c, override_for("p9", "deepseek-chat", 7.0), 1).unwrap();
    assert_eq!(
        price(&c, "p9", "deepseek-chat").unwrap().input_per_mtok,
        7.0
    );
}

#[test]
fn the_catalog_lookup_forms_apply_to_the_resolver() {
    let c = connection();
    catalog_row(&c, "anthropic", "claude-sonnet-4-5", Some(3.0), Some(15.0));
    let dated = price(&c, "p1", "Claude-Sonnet-4-5-20250929").unwrap();
    assert_eq!(
        (dated.input_per_mtok, dated.source),
        (3.0, PriceSource::Catalog)
    );
}

#[test]
fn a_fixed_price_is_per_call_and_needs_no_token_counts() {
    let c = connection();
    catalog_row(&c, "openai", "gpt-4o", Some(2.5), Some(10.0));
    save_pricing(
        &c,
        ModelPricing {
            currency: "cny".into(),
            ..fixed("relay", "gpt-4o", 0.02)
        },
        1,
    )
    .unwrap();
    let fixed = price(&c, "relay", "gpt-4o").unwrap();
    assert_eq!(
        (fixed.source, fixed.per_call, fixed.currency.as_str()),
        (PriceSource::Fixed, Some(0.02), "CNY")
    );
    let usage = CostUsage {
        input_tokens: 1_000_000,
        output_tokens: 1_000_000,
        ..CostUsage::default()
    };
    let paid = cost(&usage, &fixed);
    assert_eq!(
        (paid.micro, paid.estimated, paid.currency.as_str()),
        (20_000, false, "CNY")
    );
    // Known without counts, only for a successful request.
    assert_eq!(request_cost(None, &fixed, true).unwrap().micro, 20_000);
    assert_eq!(request_cost(None, &fixed, false).unwrap().micro, 0);
    // The other providers keep the catalog price.
    assert_eq!(
        price(&c, "other", "gpt-4o").unwrap().source,
        PriceSource::Catalog
    );
}

#[test]
fn a_corrupt_row_is_skipped_and_the_walk_continues() {
    let c = connection();
    catalog_row(&c, "f", "m1", Some(1.0), Some(2.0));
    c.execute_batch(
        "INSERT INTO model_pricing (provider_id, model_id, mode, input_per_mtok, output_per_mtok, updated_at_ms) \
             VALUES ('p1', 'm1', 'custom', -5, 1, 0); \
         INSERT INTO model_pricing (provider_id, model_id, mode, updated_at_ms) \
             VALUES ('', 'm1', 'fixed', 0);",
    )
    .unwrap();
    let resolved = price(&c, "p1", "m1").unwrap();
    assert_eq!(resolved.source, PriceSource::Catalog);
    // A corrupt global row under a healthy provider row changes nothing for that provider.
    save_price_override(&c, override_for("p1", "m1", 4.0), 1).unwrap();
    assert_eq!(price(&c, "p1", "m1").unwrap().source, PriceSource::Override);
}

/// A catalog row with a display name and a models.dev canonical id, then the grouping.
fn offer_row(c: &Connection, family: &str, id: &str, canonical: &str, prices: (f64, f64)) {
    c.execute(
        "INSERT INTO model_catalog (family, family_name, model_id, canonical_id, input_per_mtok, \
             output_per_mtok, source) VALUES (?1, upper(?1), ?2, ?3, ?4, ?5, 'models.dev')",
        params![family, id, canonical, prices.0, prices.1],
    )
    .unwrap();
    crate::llm::catalog::regroup(c).unwrap();
}

/// Claude Sonnet 4.5 as its publisher and two distributors list it.
fn sonnet(c: &Connection) {
    offer_row(c, "anthropic", "claude-sonnet-4-5", "", (3.0, 15.0));
    offer_row(c, "anthropic", "claude-haiku-4-5", "", (1.0, 5.0));
    offer_row(
        c,
        "aimlapi",
        "anthropic/claude-sonnet-4.5",
        "anthropic/claude-sonnet-4-5",
        (3.174, 15.87),
    );
    offer_row(
        c,
        "openrouter",
        "anthropic/claude-sonnet-4.5",
        "anthropic/claude-sonnet-4-5",
        (3.3, 16.5),
    );
}

fn priced(c: &Connection, provider: &str, model: &str) -> Priced {
    effective_priced(c, provider, model).unwrap().unwrap()
}

#[test]
fn a_distributors_id_bills_at_the_official_price() {
    let c = connection();
    sonnet(&c);
    let got = priced(&c, "p1", "anthropic/claude-sonnet-4.5");
    assert_eq!(
        (got.price.input_per_mtok, got.basis.kind),
        (3.0, PriceBasisKind::Official)
    );
    assert_eq!(got.basis.family.as_deref(), Some("anthropic"));
}

#[test]
fn the_precedence_end_to_end() {
    let c = connection();
    sonnet(&c);
    let id = "anthropic/claude-sonnet-4.5";
    // Provider basis: p1 follows OpenRouter's catalog prices; p2 does not.
    save_provider_pricing(&c, "p1", Some("openrouter"), 1).unwrap();
    let basis = priced(&c, "p1", id);
    assert_eq!(
        (basis.price.input_per_mtok, basis.basis.kind),
        (3.3, PriceBasisKind::Provider)
    );
    assert_eq!(priced(&c, "p2", id).price.input_per_mtok, 3.0);
    // OpenRouter does not list Haiku: official, marked as a fallback.
    let haiku = priced(&c, "p1", "claude-haiku-4-5");
    assert_eq!(
        (haiku.basis.kind, haiku.basis.fallback),
        (PriceBasisKind::Official, true)
    );
    // A model row saved under the publisher's id prices every id of the model, below the basis.
    save_pricing(&c, custom("", "claude-sonnet-4-5", 9.0), 2).unwrap();
    assert_eq!(priced(&c, "p2", id).basis.kind, PriceBasisKind::Model);
    assert_eq!(priced(&c, "p2", id).price.input_per_mtok, 9.0);
    assert_eq!(priced(&c, "p1", id).basis.kind, PriceBasisKind::Provider);
    // A model save under another id of the same model replaces that row.
    save_pricing(&c, custom("", "anthropic/claude-sonnet-4-5", 8.0), 3).unwrap();
    assert_eq!(
        list_pricing(&c, Some("anthropic/claude-sonnet-4-5"))
            .unwrap()
            .len(),
        1
    );
    // The provider's row of the model wins over everything: here, another distributor's offer.
    let follow = ModelPricing {
        catalog_family: Some("aimlapi".into()),
        ..pricing("p2", id, PricingMode::Dynamic)
    };
    save_pricing(&c, follow, 4).unwrap();
    let row = priced(&c, "p2", id);
    assert_eq!(
        (row.price.input_per_mtok, row.basis.kind),
        (3.174, PriceBasisKind::ProviderModel)
    );
    // A global dynamic save removes the model rows of every id.
    save_pricing(
        &c,
        pricing("", "claude-sonnet-4-5", PricingMode::Dynamic),
        5,
    )
    .unwrap();
    assert!(
        list_pricing(&c, None)
            .unwrap()
            .iter()
            .all(|p| !p.provider_id.is_empty())
    );
    // Back to official.
    save_provider_pricing(&c, "p1", None, 6).unwrap();
    assert_eq!(priced(&c, "p1", id).basis.kind, PriceBasisKind::Official);
    assert!(matches!(
        save_provider_pricing(&c, "p1", Some("nobody"), 7),
        Err(AppError::Validation { .. })
    ));
}

#[test]
fn an_alias_moves_an_unlisted_id_and_its_rows_to_the_model() {
    let c = connection();
    sonnet(&c);
    assert_eq!(effective_priced(&c, "relay", "sonnet-fast").unwrap(), None);
    save_pricing(&c, custom("relay", "sonnet-fast", 2.0), 1).unwrap();
    assert_eq!(rows_keys(&c), ["relay|sonnet-fast|id:sonnet-fast"]);
    let alias = ModelAlias {
        provider_id: "relay".into(),
        model_id: "sonnet-fast".into(),
        model_key: "anthropic/claude-sonnet-4-5".into(),
        updated_at_ms: 0,
    };
    save_alias(&c, alias.clone(), 9).unwrap();
    assert_eq!(
        rows_keys(&c),
        ["relay|sonnet-fast|anthropic/claude-sonnet-4-5"]
    );
    delete_pricing(&c, "relay", "sonnet-fast").unwrap();
    assert_eq!(
        priced(&c, "relay", "sonnet-fast").basis.kind,
        PriceBasisKind::Official
    );
    // Another provider's same id is not aliased.
    assert_eq!(effective_priced(&c, "other", "sonnet-fast").unwrap(), None);
    assert!(matches!(
        save_alias(
            &c,
            ModelAlias {
                model_key: "id:x".into(),
                ..alias.clone()
            },
            1
        ),
        Err(AppError::Validation { .. })
    ));
    assert!(delete_alias(&c, "relay", "sonnet-fast").unwrap());
    assert_eq!(effective_priced(&c, "relay", "sonnet-fast").unwrap(), None);
}

fn rows_keys(c: &Connection) -> Vec<String> {
    let mut statement = c
        .prepare("SELECT provider_id || '|' || model_id || '|' || model_key FROM model_pricing")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn rows_without_a_model_key_still_apply_and_regroup_keys_them() {
    let c = connection();
    sonnet(&c);
    c.execute_batch(
        "INSERT INTO model_pricing (provider_id, model_id, mode, input_per_mtok, output_per_mtok, \
             currency, updated_at_ms) VALUES ('', 'claude-sonnet-4-5', 'custom', 5, 6, 'USD', 1);",
    )
    .unwrap();
    assert_eq!(
        priced(&c, "p", "claude-sonnet-4-5").price.input_per_mtok,
        5.0
    );
    let snapshot = load_price_book_snapshot(&c).unwrap();
    assert_eq!(snapshot.pricing.len(), 1);
    assert_eq!(snapshot.pricing[0].model_key, "");
    assert_eq!(snapshot.pricing[0].pricing.model_id, "claude-sonnet-4-5");
    crate::llm::catalog::regroup(&c).unwrap();
    assert_eq!(
        rows_keys(&c),
        ["|claude-sonnet-4-5|anthropic/claude-sonnet-4-5"]
    );
    assert_eq!(
        priced(&c, "p", "anthropic/claude-sonnet-4.5")
            .price
            .input_per_mtok,
        5.0
    );
}

#[test]
fn the_price_resolution_has_three_states() {
    let c = connection();
    sonnet(&c);
    save_provider(&c, &provider("paid"), 1).unwrap();
    let local = Provider {
        base_url: "http://127.0.0.1:11434".into(),
        auth: AuthStyle::None,
        ..provider("local")
    };
    save_provider(&c, &local, 1).unwrap();
    let resolve = |provider: &str, model: &str| resolve_price(&c, provider, model).unwrap();
    assert!(matches!(
        resolve("paid", "claude-sonnet-4-5"),
        PriceResolution::Known(price) if price.input_per_mtok == 3.0
    ));
    assert_eq!(resolve("local", "llama3"), PriceResolution::Free);
    assert_eq!(
        resolve("paid", "llama3"),
        PriceResolution::Unknown(UnknownReason::NotInCatalog)
    );
    catalog_row(&c, "x", "priceless", None, None);
    assert_eq!(
        resolve("paid", "priceless"),
        PriceResolution::Unknown(UnknownReason::CatalogWithoutPrice)
    );
}
