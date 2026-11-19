use super::*;
use crate::llm::probe::listing::ModelKind;
use crate::llm::probe::plan::{CheckSelection, CheckSet, plan_run};
use crate::llm::probe::test_support::chat_item;
use crate::llm::usage::PriceSource;
use crate::llm::wire::Protocol;

fn items(count: usize) -> Vec<PlanItem> {
    (0..count).map(|n| chat_item(&format!("m{n}"))).collect()
}

fn price(input: f64, output: f64, currency: &str) -> Price {
    Price {
        input_per_mtok: input,
        output_per_mtok: output,
        cache_read_per_mtok: None,
        cache_write_per_mtok: None,
        reasoning_per_mtok: None,
        currency: currency.to_owned(),
        source: PriceSource::Catalog,
        per_call: None,
    }
}

fn run(
    count: usize,
    set: CheckSet,
    price_of: &dyn Fn(&str, &str) -> Option<Price>,
) -> ProbeEstimate {
    let items = items(count);
    let plan = plan_run(&items, &CheckSelection::Set { set });
    estimate_run(&plan, &items, price_of)
}

fn total(test: ProbeTest) -> u64 {
    let tokens = check_tokens(test);
    tokens.input + tokens.output
}

#[test]
fn the_token_table_matches_the_requirement() {
    assert_eq!(total(ProbeTest::Chat), 50);
    assert_eq!(total(ProbeTest::Stream), 490);
    assert_eq!(total(ProbeTest::Tools), 460);
    assert_eq!(total(ProbeTest::JsonSchema), 100);
    assert_eq!(total(ProbeTest::Vision), 170);
    assert_eq!(total(ProbeTest::Reasoning), 960);
    assert_eq!(total(ProbeTest::Cache), 3_010);
    assert_eq!(total(ProbeTest::Embeddings), 10);
    for free in [
        ProbeTest::Connectivity,
        ProbeTest::Auth,
        ProbeTest::Models,
        ProbeTest::Context,
    ] {
        assert_eq!(total(free), 0, "{free:?}");
    }
}

#[test]
fn free_checks_are_exactly_the_zero_rows_of_the_table() {
    for test in ProbeTest::ALL {
        assert_eq!(
            total(test) > 0,
            test.costs_tokens() && test != ProbeTest::Context,
            "{test:?}"
        );
    }
}

#[test]
fn a_standard_run_is_about_2200_tokens_per_chat_model() {
    let one = run(1, CheckSet::Standard, &|_, _| None);
    assert_eq!(one.tokens, 2_230);
    // "about 85k tokens" for 38 chat models.
    let many = run(38, CheckSet::Standard, &|_, _| None);
    assert_eq!(many.tokens, 84_740);
    assert_eq!(many.models, 38);
}

#[test]
fn a_fixed_price_is_paid_once_per_request_whatever_the_tokens() {
    let fixed = Price {
        source: PriceSource::Fixed,
        per_call: Some(0.01),
        ..price(0.0, 0.0, "CNY")
    };
    // A Standard run sends Chat, Stream, Tools, JSON schema, Vision and the other
    // token-costing checks to a chat model; each is one request. Free checks send none.
    let items = items(1);
    let plan = plan_run(
        &items,
        &CheckSelection::Set {
            set: CheckSet::Standard,
        },
    );
    let requests = plan.rows[0]
        .cells
        .iter()
        .filter(|cell| cell.skip.is_none() && total(cell.test) > 0)
        .count() as i64;
    assert!(requests > 1);
    let estimate = estimate_run(&plan, &items, &|_, _| Some(fixed.clone()));
    assert_eq!(
        estimate.cost,
        vec![CurrencyAmount {
            currency: "CNY".to_owned(),
            micro: 10_000 * requests
        }]
    );
    assert_eq!(estimate.unknown_price_models, 0);
}

/// Two Quick rows: `m0` priced in USD, `m1` in CNY.
fn mixed_currency_run() -> ProbeEstimate {
    run(2, CheckSet::Quick, &|_, model| {
        Some(if model == "m0" {
            price(1.0, 1.0, "USD")
        } else {
            price(7.0, 7.0, "CNY")
        })
    })
}

#[test]
fn prices_turn_tokens_into_money_per_currency() {
    let usd = run(1, CheckSet::Standard, &|_, _| Some(price(1.0, 2.0, "USD")));
    // 530 input at $1 and 1,700 output at $2 per million tokens.
    assert_eq!(
        usd.cost,
        [CurrencyAmount {
            currency: "USD".to_owned(),
            micro: 3_930
        }]
    );
    assert_eq!(usd.unknown_price_models, 0);
    let mixed = mixed_currency_run();
    let currencies: Vec<_> = mixed
        .cost
        .iter()
        .map(|amount| amount.currency.as_str())
        .collect();
    assert_eq!(currencies, ["CNY", "USD"]);
}

#[test]
fn cached_input_is_priced_at_the_cache_read_price_when_there_is_one() {
    let cached = Price {
        cache_read_per_mtok: Some(0.1),
        ..price(10.0, 0.0, "USD")
    };
    let plain = price(10.0, 0.0, "USD");
    let with = run(1, CheckSet::Full, &|_, _| Some(cached.clone()));
    let without = run(1, CheckSet::Full, &|_, _| Some(plain.clone()));
    assert!(with.cost[0].micro < without.cost[0].micro);
}

#[test]
fn unpriced_models_count_tokens_only() {
    let estimate = run(3, CheckSet::Standard, &|_, model| {
        (model == "m0").then(|| price(1.0, 1.0, "USD"))
    });
    assert_eq!(estimate.unknown_price_models, 2);
    assert_eq!(estimate.tokens, 3 * 2_230);
    assert_eq!(estimate.cost.len(), 1);
}

#[test]
fn free_runs_estimate_nothing() {
    let items = items(5);
    let plan = plan_run(
        &items,
        &CheckSelection::Custom {
            tests: vec![ProbeTest::Connectivity, ProbeTest::Auth, ProbeTest::Models],
        },
    );
    let estimate = estimate_run(&plan, &items, &|_, _| None);
    assert_eq!(estimate.tokens, 0);
    assert_eq!(estimate.models, 0);
    assert!(!estimate.needs_confirmation);
}

#[test]
fn skipped_cells_cost_nothing() {
    let items = [PlanItem {
        kind: ModelKind::Embedding,
        ..items(1).remove(0)
    }];
    let plan = plan_run(
        &items,
        &CheckSelection::Set {
            set: CheckSet::Full,
        },
    );
    assert_eq!(estimate_run(&plan, &items, &|_, _| None).tokens, 10);
}

#[test]
fn a_model_tested_with_two_keys_is_one_model_and_two_rows_of_tokens() {
    let mut rows = items(1);
    rows.push(PlanItem {
        key_id: "k2".to_owned(),
        ..rows[0].clone()
    });
    let plan = plan_run(
        &rows,
        &CheckSelection::Set {
            set: CheckSet::Quick,
        },
    );
    let estimate = estimate_run(&plan, &rows, &|_, _| None);
    assert_eq!(estimate.models, 1);
    assert_eq!(estimate.tokens, 2 * 540);
}

fn usd(micro: i64) -> Vec<CurrencyAmount> {
    vec![CurrencyAmount {
        currency: "USD".to_owned(),
        micro,
    }]
}

#[test]
fn confirmation_thresholds_match_every_rule_boundary() {
    let cny = |micro| {
        vec![CurrencyAmount {
            currency: "CNY".to_owned(),
            micro,
        }]
    };
    assert!(!needs_confirmation(100_000, 3, &[], 0));
    assert!(needs_confirmation(100_001, 3, &[], 0));
    assert!(!needs_confirmation(5_000, 3, &usd(500_000), 0));
    assert!(needs_confirmation(5_000, 3, &usd(500_001), 0));
    assert!(!needs_confirmation(5_000, 3, &cny(3_500_000), 0));
    assert!(needs_confirmation(5_000, 3, &cny(3_500_001), 0));
    // 0.60 CNY is far below 3.50 CNY, though above 0.50 in a currency without its own rule.
    assert!(!needs_confirmation(5_000, 3, &cny(600_000), 0));
    assert!(!needs_confirmation(5_000, 50, &[], 0));
    assert!(needs_confirmation(5_000, 51, &[], 0));
    assert!(!needs_confirmation(20_000, 3, &[], 1));
    assert!(needs_confirmation(20_001, 3, &[], 1));
    assert!(!needs_confirmation(20_001, 3, &[], 0));
    assert!(!needs_confirmation(0, 500, &usd(10_000_000), 9));
}

#[test]
fn the_acceptance_runs_of_the_requirement() {
    // A 38-model Standard run with unknown prices shows the dialog; a 3-row run does not.
    assert!(run(38, CheckSet::Standard, &|_, _| None).needs_confirmation);
    assert!(!run(3, CheckSet::Standard, &|_, _| None).needs_confirmation);
    // 38 cheap priced models stay under every threshold.
    assert!(!run(38, CheckSet::Standard, &|_, _| Some(price(0.1, 0.4, "USD"))).needs_confirmation);
    // 60 models trip rule three whatever they cost.
    assert!(run(60, CheckSet::Quick, &|_, _| Some(price(0.0, 0.0, "USD"))).needs_confirmation);
}

// ----- the one cost string -----

#[test]
fn ten_chat_models_at_one_and_four_dollars_read_exactly_the_documented_line() {
    let estimate = run(10, CheckSet::Standard, &|_, _| Some(price(1.0, 4.0, "USD")));
    assert_eq!(
        estimate.summary(),
        "about 22k tokens, ≈ $0.07 for 10 models"
    );
    assert!(!estimate.needs_confirmation);
}

#[test]
fn the_31_chat_models_of_the_relay_fixture_cost_84_cents_and_ask_first() {
    let estimate = run(31, CheckSet::Standard, &|_, _| {
        Some(price(3.0, 15.0, "USD"))
    });
    assert_eq!(estimate.tokens, 69_130);
    assert_eq!(estimate.cost[0].micro, 839_790);
    assert_eq!(
        estimate.summary(),
        "about 69k tokens, ≈ $0.84 for 31 models"
    );
    assert!(estimate.needs_confirmation);
}

#[test]
fn unknown_prices_are_named_and_partial_prices_count_the_priced_models() {
    let none = run(38, CheckSet::Standard, &|_, _| None);
    assert_eq!(
        none.summary(),
        "about 85k tokens, price unknown for 38 models"
    );
    let some = run(10, CheckSet::Standard, &|_, model| {
        (model != "m0" && model != "m1" && model != "m2").then(|| price(1.0, 4.0, "USD"))
    });
    assert_eq!(
        some.summary(),
        "about 22k tokens, ≈ $0.05 for 7 models, price unknown for 3 models"
    );
    let one = run(1, CheckSet::Standard, &|_, _| None);
    assert_eq!(
        one.summary(),
        "about 2.2k tokens, price unknown for 1 model"
    );
}

#[test]
fn money_formats_by_currency_and_small_amounts() {
    let usd = |micro| CurrencyAmount {
        currency: "USD".to_owned(),
        micro,
    };
    assert_eq!(money_text(&usd(70_000)), "≈ $0.07");
    assert_eq!(money_text(&usd(4_000)), "< $0.01");
    assert_eq!(money_text(&usd(12_345_678)), "≈ $12.35");
    let cny = CurrencyAmount {
        currency: "CNY".to_owned(),
        micro: 3_500_000,
    };
    assert_eq!(money_text(&cny), "≈ ¥3.50");
    let eur = CurrencyAmount {
        currency: "EUR".to_owned(),
        micro: 400_000,
    };
    assert_eq!(money_text(&eur), "≈ EUR 0.40");
    assert_eq!(token_text(850), "850 tokens");
    assert_eq!(token_text(3_100), "3.1k tokens");
    assert_eq!(token_text(22_300), "22k tokens");
}

#[test]
fn a_free_run_has_its_own_line_and_two_currencies_are_joined() {
    let items = items(2);
    let plan = plan_run(
        &items,
        &CheckSelection::Custom {
            tests: vec![ProbeTest::Connectivity],
        },
    );
    assert_eq!(
        estimate_run(&plan, &items, &|_, _| None).summary(),
        "no tokens, free checks only"
    );
    let mixed = mixed_currency_run();
    assert_eq!(
        mixed.summary(),
        "about 1.1k tokens, < ¥0.01 · < $0.01 for 2 models"
    );
}

#[test]
fn image_audio_rerank_and_moderation_models_add_nothing() {
    let mut rows = items(2);
    for (row, kind) in rows.iter_mut().zip([ModelKind::Chat, ModelKind::Image]) {
        row.kind = kind;
    }
    rows.push(PlanItem {
        kind: ModelKind::Embedding,
        model_id: "e".to_owned(),
        protocols: vec![Protocol::Embeddings],
        ..rows[0].clone()
    });
    for kind in [ModelKind::Audio, ModelKind::Rerank, ModelKind::Moderation] {
        rows.push(PlanItem {
            kind,
            model_id: format!("{kind:?}"),
            ..rows[0].clone()
        });
    }
    let plan = plan_run(
        &rows,
        &CheckSelection::Set {
            set: CheckSet::Standard,
        },
    );
    let estimate = estimate_run(&plan, &rows, &|_, _| None);
    // One chat model (2,230 tokens) and one embedding model (10): two models.
    assert_eq!(estimate.tokens, 2_240);
    assert_eq!(estimate.models, 2);
}
