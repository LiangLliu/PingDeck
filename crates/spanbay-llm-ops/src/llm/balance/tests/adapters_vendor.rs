//! End-to-end readings of the vendor code adapters (OpenRouter, DeepSeek) through
//! `query_balance`: every field of the report, the labels in order, the raw body and the failure
//! classes with their request counts.

use super::*;

const CREDITS: &str = "/api/v1/credits";
const KEY_PATH: &str = "/api/v1/key";

fn openrouter() -> BalanceTarget<'static> {
    vendor("openrouter", "https://openrouter.ai")
}

#[tokio::test]
async fn openrouter_reports_credits_and_the_optional_key_cap() {
    let credits = json!({"data": {"total_credits": 20, "total_usage": 7.6}});
    let key = json!({"data": {"limit": 5, "limit_remaining": 2.5}});
    let account = |raw: Value, key_limit: Option<f64>, labels: Vec<String>| {
        sealed(
            Evidence::UpstreamSource,
            raw,
            BalanceReport {
                scope: BalanceScope::Account,
                source_endpoint: Some(CREDITS.to_owned()),
                unit: BalanceUnit::Usd,
                remaining: Some(20.0 - 7.6),
                remaining_usd: Some(20.0 - 7.6),
                total: Some(20.0),
                used: Some(7.6),
                key_limit_remaining_usd: key_limit,
                labels,
                ..BalanceReport::default()
            },
        )
    };

    let both = routes(&[(CREDITS, ok(&credits)), (KEY_PATH, ok(&key))]);
    assert_eq!(
        reported(check(&both, &openrouter()).await),
        (
            account(
                json!({CREDITS: credits, KEY_PATH: key}),
                Some(2.5),
                vec!["Key limit".to_owned()]
            ),
            2
        )
    );

    let no_cap = json!({"data": {"limit": null}});
    let uncapped = routes(&[(CREDITS, ok(&credits)), (KEY_PATH, ok(&no_cap))]);
    assert_eq!(
        reported(check(&uncapped, &openrouter()).await),
        (
            account(
                json!({CREDITS: credits, KEY_PATH: no_cap}),
                None,
                Vec::new()
            ),
            2
        )
    );

    let key_fails = routes(&[(CREDITS, ok(&credits)), (KEY_PATH, status(500))]);
    assert_eq!(
        reported(check(&key_fails, &openrouter()).await),
        (account(credits.clone(), None, Vec::new()), 2)
    );

    let mut one = openrouter();
    one.request_budget = 1;
    let only_credits = routes(&[(CREDITS, ok(&credits)), (KEY_PATH, ok(&key))]);
    assert_eq!(
        reported(check(&only_credits, &one).await),
        (account(credits, None, Vec::new()), 1)
    );
}

#[tokio::test]
async fn openrouter_a_rejected_credits_request_falls_back_to_the_key() {
    let key = json!({"data": {"limit": 5, "limit_remaining": 2.5}});
    let capped = routes(&[(CREDITS, status(401)), (KEY_PATH, ok(&key))]);
    assert_eq!(
        reported(check(&capped, &openrouter()).await),
        (
            sealed(
                Evidence::UpstreamSource,
                key,
                BalanceReport {
                    scope: BalanceScope::Key,
                    source_endpoint: Some(KEY_PATH.to_owned()),
                    unit: BalanceUnit::Usd,
                    key_limit_remaining_usd: Some(2.5),
                    labels: vec![
                        "Key limit".to_owned(),
                        "OpenRouter: the key cannot read account credits".to_owned()
                    ],
                    ..BalanceReport::default()
                }
            ),
            2
        )
    );

    let uncapped = routes(&[(CREDITS, status(403)), (KEY_PATH, ok(&json!({"data": {}})))]);
    let (report, _) = reported(check(&uncapped, &openrouter()).await);
    assert_eq!(
        report.labels,
        [
            "No key limit set",
            "OpenRouter: the key cannot read account credits"
        ]
    );
    assert_eq!(report.key_limit_remaining_usd, None);
}

#[tokio::test]
async fn openrouter_failures_keep_the_original_rejection_or_name_the_endpoint() {
    let rejected = error(
        BalanceErrorKind::Auth,
        "The key was rejected",
        Some(401),
        Some(CREDITS),
    );
    let missing_fallback = routes(&[(CREDITS, status(401)), (KEY_PATH, status(404))]);
    assert_eq!(
        failed(check(&missing_fallback, &openrouter()).await),
        (rejected.clone(), 2)
    );

    let mut one = openrouter();
    one.request_budget = 1;
    let no_room = routes(&[(CREDITS, status(401))]);
    assert_eq!(failed(check(&no_room, &one).await), (rejected, 1));

    let broken_fallback = routes(&[(CREDITS, status(401)), (KEY_PATH, ok(&json!({"foo": 1})))]);
    assert_eq!(
        failed(check(&broken_fallback, &openrouter()).await),
        (
            error(
                BalanceErrorKind::SchemaMismatch,
                "data is missing",
                None,
                Some(KEY_PATH)
            ),
            2
        )
    );

    let failing_fallback = routes(&[(CREDITS, status(401)), (KEY_PATH, status(503))]);
    let (error, _) = failed(check(&failing_fallback, &openrouter()).await);
    assert_eq!(
        (error.kind, error.endpoint.as_deref()),
        (BalanceErrorKind::Transient, Some(KEY_PATH))
    );

    let short = routes(&[(CREDITS, ok(&json!({"data": {"total_credits": 20}})))]);
    assert_eq!(
        failed(check(&short, &openrouter()).await),
        (
            self::error(
                BalanceErrorKind::SchemaMismatch,
                "data.total_credits or data.total_usage is missing",
                None,
                Some(CREDITS)
            ),
            1
        )
    );
}

async fn deepseek(body: &Value) -> BalanceReport {
    let http = routes(&[("/user/balance", ok(body))]);
    let (report, used) =
        reported(check(&http, &vendor("deepseek", "https://api.deepseek.com")).await);
    assert_eq!(used, 1);
    report
}

#[tokio::test]
async fn deepseek_is_exhausted_when_every_currency_is_empty_or_the_vendor_says_so() {
    let empty = json!({"is_available": true, "balance_infos": [
        {"currency": "CNY", "total_balance": "0.00"},
        {"currency": "USD", "total_balance": "0"}
    ]});
    assert_eq!(
        deepseek(&empty).await,
        sealed(
            Evidence::Documented,
            empty.clone(),
            BalanceReport {
                source_endpoint: Some("/user/balance".to_owned()),
                unit: BalanceUnit::Cny,
                remaining: Some(0.0),
                alt: vec![BalanceAlt {
                    unit: BalanceUnit::Usd,
                    remaining: 0.0
                }],
                exhausted: true,
                ..BalanceReport::default()
            }
        )
    );

    let other_currency = json!({"is_available": true, "balance_infos": [
        {"currency": "CNY", "total_balance": "0"},
        {"currency": "USD", "total_balance": "3"}
    ]});
    let report = deepseek(&other_currency).await;
    assert_eq!((report.remaining, report.exhausted), (Some(0.0), false));

    let unavailable = json!({"is_available": false, "balance_infos": [
        {"currency": "USD", "total_balance": "5"}
    ]});
    let report = deepseek(&unavailable).await;
    assert_eq!(
        (
            report.unit,
            report.remaining,
            report.remaining_usd,
            report.exhausted
        ),
        (BalanceUnit::Usd, Some(5.0), Some(5.0), true)
    );
    assert_eq!(report.labels, ["DeepSeek: is_available is false"]);
}

#[tokio::test]
async fn deepseek_labels_unknown_currencies_alt_first() {
    let body = json!({"is_available": true, "balance_infos": [
        {"currency": "X$Y", "total_balance": "1"},
        {"currency": "EUR", "total_balance": "2"},
        {"total_balance": "4"}
    ]});
    let report = deepseek(&body).await;
    assert_eq!(
        (
            report.unit,
            report.remaining,
            report.remaining_usd,
            report.exhausted
        ),
        (BalanceUnit::Unsupported, Some(1.0), None, false)
    );
    assert_eq!(report.labels, ["EUR 2", "? 4", "XY 1"]);
}
