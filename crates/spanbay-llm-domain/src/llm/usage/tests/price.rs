//! Price overrides, stored pricing rows and the end-to-end resolution of a price.

use super::price;
use crate::llm::usage::{Offer, Price, PriceOverride, PriceSource, PricingRows, decide};
use spanbay_model::model_catalog::{
    MAX_PRICE_PER_MTOK, ModelPricing, ModelRecord, PricingMode, normalize_currency,
};

fn override_row() -> PriceOverride {
    PriceOverride {
        provider_id: "deepseek".to_owned(),
        model_id: "deepseek-chat".to_owned(),
        input_per_mtok: 2.0,
        output_per_mtok: 8.0,
        cache_read_per_mtok: Some(0.5),
        cache_write_per_mtok: None,
        reasoning_per_mtok: None,
        currency: "CNY".to_owned(),
        updated_at_ms: 1,
    }
}

/// `override_row` with a currency that needs normalising, validated.
fn normalised_override() -> PriceOverride {
    let mut row = override_row();
    row.currency = " cny ".to_owned();
    row.validated().unwrap()
}

fn catalog(input: Option<f64>, output: Option<f64>) -> ModelRecord {
    ModelRecord {
        family: "f".to_owned(),
        model_id: "m".to_owned(),
        input_per_mtok: input,
        output_per_mtok: output,
        cache_read_per_mtok: Some(0.25),
        cache_write_per_mtok: Some(1.5),
        reasoning_per_mtok: Some(9.0),
        currency: "USD".to_owned(),
        ..ModelRecord::default()
    }
}

/// A `custom` row of model `m` at `provider` ("" for every provider).
fn custom(provider: &str, input: f64) -> ModelPricing {
    ModelPricing {
        mode: PricingMode::Custom,
        input_per_mtok: Some(input),
        output_per_mtok: Some(input * 2.0),
        ..ModelPricing::dynamic(provider, "m")
    }
}

/// A `fixed` row of model `m` at `provider`.
fn fixed(provider: &str, per_call: f64) -> ModelPricing {
    ModelPricing {
        mode: PricingMode::Fixed,
        per_call: Some(per_call),
        ..ModelPricing::dynamic(provider, "m")
    }
}

/// The price `decide` gives model `m` of publisher `f` with the catalog row `record`, if any.
fn resolved(
    provider: Option<&ModelPricing>,
    global: Option<&ModelPricing>,
    record: Option<ModelRecord>,
) -> Option<Price> {
    let offers: Vec<Offer> = record
        .iter()
        .map(|r| Offer {
            family: r.family.clone(),
            family_name: r.family_name.clone(),
            model_id: r.model_id.clone(),
            price: Price::from_catalog(r),
        })
        .collect();
    decide(
        "m",
        "f",
        &offers,
        PricingRows {
            provider,
            provider_family: None,
            global,
        },
    )
    .map(|p| p.price)
}

#[test]
fn override_validation_normalises_the_currency_and_accepts_zero_prices() {
    let row = normalised_override();
    assert_eq!(row.currency, "CNY");
    assert!(row.validate().is_ok());

    let mut zero = override_row();
    zero.input_per_mtok = 0.0;
    zero.output_per_mtok = 0.0;
    assert!(zero.validated().is_ok());

    let mut lower = override_row();
    lower.currency = "cny".to_owned();
    assert!(lower.validate().is_err());
}

#[test]
fn override_validation_rejects_each_bad_field() {
    let edits: [fn(&mut PriceOverride); 13] = [
        |r| r.input_per_mtok = -0.01,
        |r| r.output_per_mtok = f64::NAN,
        |r| r.input_per_mtok = f64::INFINITY,
        |r| r.output_per_mtok = MAX_PRICE_PER_MTOK + 1.0,
        |r| r.cache_read_per_mtok = Some(-1.0),
        |r| r.cache_write_per_mtok = Some(f64::NAN),
        |r| r.reasoning_per_mtok = Some(f64::INFINITY),
        |r| r.currency = "US".to_owned(),
        |r| r.currency = "US1".to_owned(),
        |r| r.currency = "USDT".to_owned(),
        |r| r.currency = String::new(),
        |r| r.provider_id = " ".to_owned(),
        |r| r.model_id = String::new(),
    ];
    for edit in edits {
        let mut r = override_row();
        edit(&mut r);
        assert!(r.validated().is_err());
    }
}

#[test]
fn currencies_normalise_to_upper_case_iso_codes() {
    assert_eq!(normalize_currency("eur").unwrap(), "EUR");
    for bad in ["€", "EU", "EU R", "XYZ"] {
        assert!(normalize_currency(bad).is_err());
    }
}

#[test]
fn an_override_is_a_provider_scoped_custom_row() {
    let row = normalised_override();
    let as_pricing = row.to_pricing();
    assert_eq!(PriceOverride::from_pricing(&as_pricing), Some(row));
    assert_eq!(PriceOverride::from_pricing(&custom("", 1.0)), None);
    assert_eq!(PriceOverride::from_pricing(&fixed("p", 1.0)), None);
    assert_eq!(
        PriceOverride::from_pricing(&ModelPricing::dynamic("p", "m")),
        None
    );
}

#[test]
fn resolution_takes_the_provider_row_then_the_global_row_then_the_catalog() {
    let rec = catalog(Some(1.0), Some(2.0));
    let prov = custom("p", 5.0);
    let glob = custom("", 3.0);
    assert_eq!(
        resolved(Some(&prov), Some(&glob), Some(rec.clone())).map(|p| (p.input_per_mtok, p.source)),
        Some((5.0, PriceSource::Override))
    );
    assert_eq!(
        resolved(None, Some(&glob), Some(rec.clone())).map(|p| (p.input_per_mtok, p.source)),
        Some((3.0, PriceSource::Override))
    );
    assert_eq!(
        resolved(None, None, Some(rec)).map(|p| (p.input_per_mtok, p.source)),
        Some((1.0, PriceSource::Catalog))
    );
    assert_eq!(resolved(None, None, None), None);
    assert_eq!(resolved(None, None, Some(catalog(Some(1.0), None))), None);
    assert_eq!(resolved(None, None, Some(catalog(None, Some(1.0)))), None);
}

#[test]
fn a_dynamic_provider_row_opts_out_of_the_global_row() {
    let dyn_row = ModelPricing::dynamic("p", "m");
    let glob = custom("", 3.0);
    assert_eq!(
        resolved(
            Some(&dyn_row),
            Some(&glob),
            Some(catalog(Some(1.0), Some(2.0)))
        )
        .unwrap()
        .source,
        PriceSource::Catalog
    );
    assert_eq!(resolved(Some(&dyn_row), Some(&glob), None), None);
}

#[test]
fn only_a_fixed_price_serializes_per_call() {
    let fp = resolved(
        Some(&fixed("p", 0.02)),
        None,
        Some(catalog(Some(1.0), Some(2.0))),
    )
    .unwrap();
    assert_eq!((fp.source, fp.per_call), (PriceSource::Fixed, Some(0.02)));
    let fp_json = serde_json::to_value(&fp).unwrap();
    assert_eq!(
        (&fp_json["per_call"], &fp_json["source"]),
        (&serde_json::json!(0.02), &serde_json::json!("fixed"))
    );
    let plain_json = serde_json::to_value(price(1.0, 2.0)).unwrap();
    assert!(plain_json.get("per_call").is_none());
}

#[test]
fn a_catalog_price_keeps_its_currency_and_extra_rates() {
    let mut cny_rec = catalog(Some(1.0), Some(2.0));
    cny_rec.currency = "CNY".to_owned();
    let cat_p = resolved(None, None, Some(cny_rec)).unwrap();
    assert_eq!(
        (
            cat_p.currency.as_str(),
            cat_p.cache_read_per_mtok,
            cat_p.cache_write_per_mtok,
            cat_p.reasoning_per_mtok
        ),
        ("CNY", Some(0.25), Some(1.5), Some(9.0))
    );
    assert!(Price::from_catalog(&catalog(Some(0.0), Some(0.0))).is_some());
}

#[test]
fn corrupt_rows_fall_through_to_the_next_rule() {
    let rec = catalog(Some(1.0), Some(2.0));
    let glob = custom("", 3.0);
    let mut bad = custom("p", 1.0);
    bad.input_per_mtok = Some(f64::NAN);
    assert_eq!(
        resolved(Some(&bad), Some(&glob), Some(rec.clone()))
            .unwrap()
            .input_per_mtok,
        3.0
    );
    let broken_fixed = ModelPricing {
        mode: PricingMode::Fixed,
        ..ModelPricing::dynamic("p", "m")
    };
    assert_eq!(
        resolved(Some(&broken_fixed), None, Some(rec))
            .unwrap()
            .source,
        PriceSource::Catalog
    );
}
