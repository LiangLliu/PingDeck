//! Which price applies: the default (official or reference) offer and the precedence of rows.

use super::price;
use crate::llm::usage::{
    Offer, Price, PriceSource, PricingRows, apply_relay, decide, default_price, official_family,
    preset_family,
};
use pingdeck_model::model_catalog::{ModelPricing, PriceBasisKind, PricingMode, RelayPricing};

/// A catalog price.
fn catalog_price(input: f64, output: f64) -> Price {
    Price {
        source: PriceSource::Catalog,
        ..price(input, output)
    }
}

/// An offer of `family` (display name: the family upper-cased) at a catalog price.
fn offer(family: &str, model_id: &str, prices: Option<(f64, f64)>) -> Offer {
    Offer {
        family: family.to_owned(),
        family_name: family.to_uppercase(),
        model_id: model_id.to_owned(),
        price: prices.map(|(i, o)| catalog_price(i, o)),
    }
}

fn row(provider: &str, mode: PricingMode, family: Option<&str>) -> ModelPricing {
    ModelPricing {
        mode,
        input_per_mtok: Some(7.0),
        output_per_mtok: Some(8.0),
        per_call: Some(0.5),
        catalog_family: family.map(str::to_owned),
        ..ModelPricing::dynamic(provider, "m")
    }
}

fn offers() -> Vec<Offer> {
    vec![
        offer("openai", "gpt-5", Some((1.25, 10.0))),
        offer("openrouter", "openai/gpt-5", Some((1.5, 12.0))),
        offer("aimlapi", "openai/gpt-5", Some((1.3, 10.4))),
        offer("free", "gpt-5:free", Some((0.0, 0.0))),
    ]
}

#[test]
fn the_official_offer_is_the_default() {
    let priced = default_price("openai", "gpt-5", &offers()).unwrap();
    assert_eq!(priced.price.input_per_mtok, 1.25);
    assert_eq!(priced.basis.kind, PriceBasisKind::Official);
    assert_eq!(priced.basis.family.as_deref(), Some("openai"));
}

#[test]
fn without_an_official_offer_the_lower_median_above_zero_is_the_reference() {
    let mut list = offers();
    list.remove(0);
    list.push(offer("cheap", "gpt-5", Some((0.1, 0.2))));
    list.push(offer("unpriced", "x", None));
    // above zero, by total: cheap 0.3, aimlapi 11.7, openrouter 13.5 -> the middle one
    let priced = default_price("openai", "gpt-5", &list).unwrap();
    assert_eq!(priced.basis.kind, PriceBasisKind::Reference);
    assert_eq!(priced.basis.family.as_deref(), Some("aimlapi"));
    assert_eq!(
        priced.basis.reference_from,
        ["AIMLAPI", "CHEAP", "OPENROUTER"]
    );
    // an even count takes the lower of the two middle offers
    list.retain(|o| o.family != "cheap");
    assert_eq!(
        default_price("openai", "gpt-5", &list)
            .unwrap()
            .basis
            .family
            .as_deref(),
        Some("aimlapi")
    );
}

#[test]
fn zero_offers_count_only_alone_and_no_price_is_none() {
    let zero = vec![
        offer("free", "m", Some((0.0, 0.0))),
        offer("none", "m", None),
    ];
    let priced = default_price("lab", "m", &zero).unwrap();
    assert_eq!(
        (priced.price.input_per_mtok, priced.basis.kind),
        (0.0, PriceBasisKind::Reference)
    );
    assert!(default_price("lab", "m", &[offer("none", "m", None)]).is_none());
    assert!(default_price("lab", "m", &[]).is_none());
}

#[test]
fn an_unpriced_official_row_falls_to_the_reference() {
    let list = vec![
        offer("lab", "m", None),
        offer("host", "m", Some((1.0, 2.0))),
    ];
    assert_eq!(
        default_price("lab", "m", &list).unwrap().basis.kind,
        PriceBasisKind::Reference
    );
}

#[test]
fn precedence_provider_row_then_provider_basis_then_model_row_then_default() {
    let list = offers();
    let custom = row("p", PricingMode::Custom, None);
    let global = row("", PricingMode::Fixed, None);
    let all = PricingRows {
        provider: Some(&custom),
        provider_family: Some("openrouter"),
        global: Some(&global),
    };
    let pick = |rows| decide("openai/gpt-5", "openai", &list, rows).unwrap();
    let first = pick(all);
    assert_eq!(
        (first.basis.kind, first.price.input_per_mtok),
        (PriceBasisKind::ProviderModel, 7.0)
    );
    let second = pick(PricingRows {
        provider: None,
        ..all
    });
    assert_eq!(
        (second.basis.kind, second.price.input_per_mtok),
        (PriceBasisKind::Provider, 1.5)
    );
    let third = pick(PricingRows {
        provider: None,
        provider_family: None,
        ..all
    });
    assert_eq!(
        (third.basis.kind, third.price.per_call),
        (PriceBasisKind::Model, Some(0.5))
    );
    let last = pick(PricingRows::default());
    assert_eq!(
        (last.basis.kind, last.basis.fallback),
        (PriceBasisKind::Official, false)
    );
}

#[test]
fn a_basis_family_that_does_not_list_the_model_falls_back_and_says_so() {
    let list = offers();
    let global = row("", PricingMode::Custom, None);
    let missing = PricingRows {
        provider_family: Some("groq"),
        global: Some(&global),
        ..PricingRows::default()
    };
    let priced = decide("gpt-5", "openai", &list, missing).unwrap();
    assert_eq!(
        (priced.basis.kind, priced.basis.fallback),
        (PriceBasisKind::Model, true)
    );
    let priced = decide(
        "gpt-5",
        "openai",
        &list,
        PricingRows {
            global: None,
            ..missing
        },
    )
    .unwrap();
    assert_eq!(
        (priced.basis.kind, priced.basis.fallback),
        (PriceBasisKind::Official, true)
    );
}

#[test]
fn a_plain_dynamic_provider_row_skips_the_basis_and_the_model_row() {
    let list = offers();
    let dynamic = row("p", PricingMode::Dynamic, None);
    let global = row("", PricingMode::Custom, None);
    let priced = decide(
        "gpt-5",
        "openai",
        &list,
        PricingRows {
            provider: Some(&dynamic),
            provider_family: Some("openrouter"),
            global: Some(&global),
        },
    )
    .unwrap();
    assert_eq!(priced.basis.kind, PriceBasisKind::Official);
}

#[test]
fn a_row_following_a_family_uses_that_offer_and_prefers_the_same_id() {
    let mut list = offers();
    list.push(offer("openrouter", "gpt-5", Some((9.0, 9.0))));
    let follow = row("p", PricingMode::Dynamic, Some("openrouter"));
    let rows = PricingRows {
        provider: Some(&follow),
        ..PricingRows::default()
    };
    let priced = decide("gpt-5", "openai", &list, rows).unwrap();
    assert_eq!(
        (priced.basis.kind, priced.price.input_per_mtok),
        (PriceBasisKind::ProviderModel, 9.0)
    );
    assert_eq!(priced.basis.family_name.as_deref(), Some("OPENROUTER"));
    let gone = row("p", PricingMode::Dynamic, Some("groq"));
    let priced = decide(
        "gpt-5",
        "openai",
        &list,
        PricingRows {
            provider: Some(&gone),
            ..rows
        },
    )
    .unwrap();
    assert_eq!(
        (priced.basis.kind, priced.basis.fallback),
        (PriceBasisKind::Official, true)
    );
}

#[test]
fn unlisted_models_have_only_their_rows() {
    let custom = row("", PricingMode::Custom, None);
    assert!(decide("x", "", &[], PricingRows::default()).is_none());
    let priced = decide(
        "x",
        "",
        &[],
        PricingRows {
            global: Some(&custom),
            ..PricingRows::default()
        },
    )
    .unwrap();
    assert_eq!(priced.price.source, PriceSource::Override);
}

#[test]
fn a_dynamic_row_following_a_lost_family_skips_the_basis_and_the_model_row() {
    let list = offers();
    let gone = row("p", PricingMode::Dynamic, Some("groq"));
    let global = row("", PricingMode::Custom, None);
    let priced = decide(
        "gpt-5",
        "openai",
        &list,
        PricingRows {
            provider: Some(&gone),
            provider_family: Some("openrouter"),
            global: Some(&global),
        },
    )
    .unwrap();
    assert_eq!(
        (priced.basis.kind, priced.basis.fallback),
        (PriceBasisKind::Official, true)
    );
}

#[test]
fn reference_names_are_listed_once_sorted_case_insensitively() {
    let named = |family: &str, name: &str, input: f64| Offer {
        family_name: name.to_owned(),
        ..offer(family, "m", Some((input, 1.0)))
    };
    let list = [
        named("b", "beta", 3.0),
        named("a2", "Alpha", 2.0),
        named("a1", "Alpha", 1.0),
        named("c", "Gamma", 4.0),
    ];
    let priced = default_price("lab", "m", &list).unwrap();
    assert_eq!(priced.basis.reference_from, ["Alpha", "beta", "Gamma"]);
}

#[test]
fn the_preset_family_follows_the_region_of_the_host() {
    assert_eq!(
        preset_family(Some("siliconflow"), "https://api.siliconflow.cn/v1").as_deref(),
        Some("siliconflow-cn")
    );
    assert_eq!(
        preset_family(Some("siliconflow"), "https://api.siliconflow.com").as_deref(),
        Some("siliconflow")
    );
    assert_eq!(
        preset_family(Some("siliconflow"), "https://relay.example.com"),
        None
    );
    assert_eq!(preset_family(None, "https://api.siliconflow.cn"), None);
    assert_eq!(preset_family(Some("siliconflow"), "not a url"), None);
    assert_eq!(
        preset_family(Some("no-such-preset"), "https://api.siliconflow.cn"),
        None
    );
}

#[test]
fn the_official_family_is_the_preset_only_when_it_prices_the_model() {
    let list = offers();
    assert_eq!(
        official_family("lab", Some("openrouter"), &list),
        "openrouter"
    );
    assert_eq!(official_family("lab", Some("groq"), &list), "lab");
    assert_eq!(official_family("lab", None, &list), "lab");
    let unpriced = [offer("cn", "m", None)];
    assert_eq!(official_family("lab", Some("cn"), &unpriced), "lab");
}

#[test]
fn a_variant_keyed_with_its_base_model_keeps_its_own_price() {
    // `kimi-k2-turbo` and `-highspeed` share a catalog key with `kimi-k2`.
    let offer = |family: &str, id: &str, input: f64, output: f64| Offer {
        family: family.to_owned(),
        family_name: family.to_owned(),
        model_id: id.to_owned(),
        price: Some(price(input, output)),
    };
    let offers = [
        offer("moonshotai", "kimi-k2", 0.6, 2.5),
        offer("moonshotai", "kimi-k2-highspeed", 2.4, 10.0),
        offer("openrouter", "moonshotai/kimi-k2", 0.5, 2.0),
        offer("openrouter", "moonshotai/kimi-k2-highspeed", 2.0, 8.0),
        offer("relay", "moonshotai/kimi-k2-highspeed", 3.0, 12.0),
    ];
    let official = |id: &str| default_price("moonshotai", id, &offers).unwrap().price;
    assert_eq!(official("kimi-k2-highspeed").output_per_mtok, 10.0);
    assert_eq!(official("kimi-k2").output_per_mtok, 2.5);
    // Without the publisher's rows the reference is taken among the same id.
    let reference = default_price("moonshotai", "kimi-k2-highspeed", &offers[2..]).unwrap();
    assert_eq!(reference.price.output_per_mtok, 8.0);
    assert_eq!(reference.basis.reference_from, ["openrouter", "relay"]);
}

// Interleaved case variants were listed more than once.
#[test]
fn reference_names_differing_only_in_case_are_listed_once() {
    let named = |family: &str, name: &str, input: f64| Offer {
        family_name: name.to_owned(),
        ..offer(family, "m", Some((input, 1.0)))
    };
    let list = [
        named("a", "OpenAI", 1.0),
        named("b", "openai", 2.0),
        named("c", "OpenAI", 3.0),
        named("d", "Groq", 4.0),
    ];
    let priced = default_price("lab", "m", &list).unwrap();
    assert_eq!(priced.basis.reference_from, ["Groq", "OpenAI"]);
}

#[test]
fn a_relay_multiplies_the_official_price_but_not_its_own_row() {
    let relay = RelayPricing {
        multiplier: 0.5,
        currency: Some("CNY".to_owned()),
    };
    let official = default_price("openai", "gpt-5", &offers()).unwrap();
    let billed = apply_relay(official, Some(&relay));
    assert_eq!(
        (billed.price.input_per_mtok, billed.price.output_per_mtok),
        (0.625, 5.0)
    );
    assert_eq!(billed.price.currency, "CNY");
    assert_eq!(billed.basis.relay.as_deref(), Some(&relay));
    // The provider's own row is the relay's price already.
    let own = row("p1", PricingMode::Custom, None);
    let mine = decide(
        "gpt-5",
        "openai",
        &offers(),
        PricingRows {
            provider: Some(&own),
            ..PricingRows::default()
        },
    )
    .unwrap();
    let kept = apply_relay(mine.clone(), Some(&relay));
    assert_eq!(kept, mine);
}

#[test]
fn a_relay_multiplier_must_be_positive_finite_and_bounded() {
    let with = |multiplier: f64| RelayPricing {
        multiplier,
        currency: None,
    };
    assert!(with(1.5).check().is_ok());
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, 1_001.0] {
        assert!(with(bad).check().is_err(), "{bad}");
    }
    let lower = RelayPricing {
        multiplier: 1.0,
        currency: Some("cny".to_owned()),
    };
    assert!(lower.check().is_err());
}
