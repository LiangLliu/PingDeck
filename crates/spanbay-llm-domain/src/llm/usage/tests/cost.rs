//! The cost function, fixed prices, request costs and token estimates.

use super::{price, usage};
use crate::llm::hub::HubUsage;
use crate::llm::usage::{
    CostUsage, PriceSource, cost, estimate_tokens, estimate_tokens_bytes, request_cost,
};
use spanbay_model::model_catalog::MAX_PRICE_PER_MTOK;

#[test]
fn cost_rounds_half_up_once() {
    assert_eq!(cost(&usage(18_204, 912), &price(1.2, 2.4)).micro, 24_034);
    assert_eq!(
        cost(&usage(1_000_000, 1_000_000), &price(0.15, 0.6)).micro,
        750_000
    );
    assert_eq!(cost(&CostUsage::default(), &price(5.0, 15.0)).micro, 0);
    assert_eq!(cost(&usage(1, 0), &price(0.5, 0.0)).micro, 1);
    assert_eq!(cost(&usage(1, 0), &price(0.4, 0.0)).micro, 0);
    assert_eq!(cost(&usage(1, 1), &price(0.4, 0.4)).micro, 1);
}

#[test]
fn cache_reads_and_writes_bill_at_their_price_or_the_input_price() {
    let mut p = price(1.0, 2.0);
    p.cache_read_per_mtok = Some(0.1);
    let mut u = usage(1_000, 0);
    u.cached_tokens = 400;
    assert_eq!(cost(&u, &p).micro, 640);
    u.cache_write_tokens = 100;
    assert_eq!(cost(&u, &price(1.0, 2.0)).micro, 1_000);
    p.cache_read_per_mtok = None;
    p.cache_write_per_mtok = Some(1.25);
    u.cached_tokens = 0;
    u.cache_write_tokens = 200;
    assert_eq!(cost(&u, &p).micro, 1_050);
}

#[test]
fn cost_clamps_cache_tokens_to_the_prompt() {
    let mut p = price(1.0, 2.0);
    p.cache_read_per_mtok = Some(0.5);
    p.cache_write_per_mtok = Some(2.0);
    let mut u_clamp = usage(100, 0);
    u_clamp.cached_tokens = 90;
    u_clamp.cache_write_tokens = 90;
    assert_eq!(cost(&u_clamp, &p).micro, 65);
    u_clamp.cached_tokens = 10_000;
    u_clamp.cache_write_tokens = 10_000;
    assert_eq!(cost(&u_clamp, &p).micro, 50);
}

#[test]
fn reasoning_tokens_are_clamped_to_the_output_and_bill_at_their_own_price() {
    let mut u_reason = usage(0, 1_000);
    u_reason.reasoning_tokens = 300;
    assert_eq!(cost(&u_reason, &price(1.0, 2.0)).micro, 2_000);
    u_reason.reasoning_tokens = 5_000;
    assert_eq!(cost(&u_reason, &price(1.0, 2.0)).micro, 2_000);
    u_reason.reasoning_tokens = 300;
    let mut p_reason = price(1.0, 2.0);
    p_reason.reasoning_per_mtok = Some(10.0);
    assert_eq!(cost(&u_reason, &p_reason).micro, 4_400);
}

#[test]
fn cost_saturates_and_prices_are_capped() {
    assert_eq!(
        cost(
            &usage(u64::MAX, u64::MAX),
            &price(MAX_PRICE_PER_MTOK, MAX_PRICE_PER_MTOK)
        )
        .micro,
        i64::MAX
    );
    assert_eq!(
        cost(&usage(1_000_000, 0), &price(1e300, 0.0)).micro,
        1_000_000_000_000
    );
}

#[test]
fn garbage_prices_cost_nothing() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -3.0] {
        assert_eq!(
            cost(&usage(1_000_000, 1_000_000), &price(bad, bad)).micro,
            0
        );
    }
}

#[test]
fn a_cost_is_estimated_for_catalog_prices_and_estimated_tokens() {
    let mut p_cur = price(1.0, 1.0);
    p_cur.currency = "CNY".to_owned();
    let exact = cost(&usage(10, 10), &p_cur);
    assert_eq!(
        (exact.currency.as_str(), exact.source, exact.estimated),
        ("CNY", PriceSource::Override, false)
    );
    let mut est_u = usage(10, 10);
    est_u.tokens_estimated = true;
    assert!(cost(&est_u, &p_cur).estimated);
    p_cur.source = PriceSource::Catalog;
    assert!(cost(&usage(10, 10), &p_cur).estimated);
}

#[test]
fn a_fixed_price_is_the_whole_cost_whatever_the_tokens() {
    let mut est_u = usage(10, 10);
    est_u.tokens_estimated = true;
    let mut fixed_p = price(0.0, 0.0);
    fixed_p.source = PriceSource::Fixed;
    fixed_p.per_call = Some(0.02);
    fixed_p.currency = "CNY".to_owned();
    for t in [usage(0, 0), usage(1, 1), usage(5_000_000, 5_000_000), est_u] {
        let paid = cost(&t, &fixed_p);
        assert_eq!(
            (
                paid.micro,
                paid.currency.as_str(),
                paid.source,
                paid.estimated
            ),
            (20_000, "CNY", PriceSource::Fixed, false)
        );
    }
    fixed_p.per_call = Some(0.0);
    assert_eq!(cost(&usage(9, 9), &fixed_p).micro, 0);
    fixed_p.per_call = Some(1e300);
    assert_eq!(cost(&usage(9, 9), &fixed_p).micro, 1_000_000_000_000);
}

#[test]
fn request_cost_needs_usage_unless_the_price_is_per_call() {
    let per_tok = price(1.0, 2.0);
    assert_eq!(request_cost(None, &per_tok, true), None);
    let counted = usage(1_000_000, 1_000_000);
    assert_eq!(
        request_cost(Some(&counted), &per_tok, true),
        Some(cost(&counted, &per_tok))
    );
    assert_eq!(
        request_cost(Some(&counted), &per_tok, false),
        Some(cost(&counted, &per_tok))
    );

    let mut fixed_p = price(0.0, 0.0);
    fixed_p.source = PriceSource::Fixed;
    fixed_p.per_call = Some(0.5);
    assert_eq!(request_cost(None, &fixed_p, true).unwrap().micro, 500_000);
    assert_eq!(
        request_cost(Some(&counted), &fixed_p, true).unwrap().micro,
        500_000
    );
    assert_eq!(request_cost(None, &fixed_p, false).unwrap().micro, 0);
}

#[test]
fn hub_usage_converts_only_with_input_or_output_tokens() {
    assert_eq!(CostUsage::from_hub(&HubUsage::default()), None);
    assert_eq!(
        CostUsage::from_hub(&HubUsage {
            output_tokens: Some(7),
            ..HubUsage::default()
        }),
        Some(usage(0, 7))
    );
    let conv = CostUsage::from_hub(&HubUsage {
        input_tokens: Some(100),
        output_tokens: Some(20),
        cached_input_tokens: Some(30),
        cache_write_tokens: Some(5),
        cache_write_long_tokens: None,
        reasoning_tokens: Some(8),
    })
    .unwrap();
    assert_eq!(
        (
            conv.input_tokens,
            conv.output_tokens,
            conv.cached_tokens,
            conv.cache_write_tokens,
            conv.reasoning_tokens,
            conv.tokens_estimated
        ),
        (100, 20, 30, 5, 8, false)
    );
}

/// The estimate itself is tested in `spanbay_model::tokens`; these inputs are not covered there.
#[test]
fn token_estimates_count_text_and_raw_bytes() {
    assert_eq!(estimate_tokens("a"), 1);
    assert_eq!(estimate_tokens(&"x".repeat(4_001)), 1_001);
    assert_eq!(estimate_tokens_bytes("日本語日".as_bytes()), 4);
    assert_eq!(estimate_tokens_bytes(&[]), 0);
}

/// Golden figures from Anthropic's price list (Claude Sonnet 4.5, USD per Mtok): input 3,
/// cache read 0.30 (0.1x), five-minute cache write 3.75 (1.25x), one-hour write 6 (2x), output 15.
#[test]
fn anthropic_cache_reads_and_both_write_tiers_bill_at_their_own_rate() {
    let mut p = price(3.0, 15.0);
    p.cache_read_per_mtok = Some(0.3);
    p.cache_write_per_mtok = Some(3.75);
    let hub = HubUsage {
        input_tokens: Some(100_000),
        output_tokens: Some(1_000),
        cached_input_tokens: Some(50_000),
        cache_write_tokens: Some(40_000),
        cache_write_long_tokens: Some(10_000),
        reasoning_tokens: None,
    };
    let u = CostUsage::from_hub(&hub).unwrap();
    // 10k fresh x 3 + 50k read x 0.3 + 30k short write x 3.75 + 10k long write x 6 + 1k out x 15.
    assert_eq!(
        cost(&u, &p).micro,
        30_000 + 15_000 + 112_500 + 60_000 + 15_000
    );
    // A long-tier figure above the writes is clamped to them, never billed twice.
    let mut over = u;
    over.cache_write_long_tokens = 90_000;
    assert_eq!(cost(&over, &p).micro, 30_000 + 15_000 + 240_000 + 15_000);
}
