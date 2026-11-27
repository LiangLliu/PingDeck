//! End-to-end readings of the Sub2API, LiteLLM and VoAPI adapters through `query_balance`: every
//! field of the report, the labels in order, the raw body and the failure classes with their
//! request counts.

use spanbay_base::secrets::redact_secrets;

use super::*;

const UPSTREAM: Evidence = Evidence::UpstreamSource;

async fn sub2api(body: &Value) -> (BalanceReport, u32) {
    let http = routes(&[("/v1/usage", ok(body))]);
    reported(check(&http, &target(GatewayFamily::Sub2Api)).await)
}

fn sub2api_report(body: &Value, draft: BalanceReport) -> BalanceReport {
    sealed(
        UPSTREAM,
        body.clone(),
        BalanceReport {
            source_endpoint: Some("/v1/usage".to_owned()),
            ..draft
        },
    )
}

#[tokio::test]
async fn sub2api_quota_limited_keys_report_quota_windows_and_status() {
    let exhausted = json!({"mode": "quota_limited", "planName": "Pro", "status": "quota_exhausted",
    "expires_at": "2026-11-07T12:00:00Z",
    "quota": {"limit": 10, "used": 10, "remaining": 0, "unit": "USD"},
    "rate_limits": [
        {"window": "5h", "limit": 100, "used": 20, "remaining": 80, "reset_at": "2026-11-07T12:00:00Z"},
        {"limit": 1}
    ]});
    assert_eq!(
        sub2api(&exhausted).await,
        (
            sub2api_report(
                &exhausted,
                BalanceReport {
                    scope: BalanceScope::Key,
                    unit: BalanceUnit::Usd,
                    remaining: Some(0.0),
                    total: Some(10.0),
                    used: Some(10.0),
                    remaining_usd: Some(0.0),
                    plan_name: Some("Pro".to_owned()),
                    expires_at_ms: Some(1_794_052_800_000),
                    rate_windows: vec![BalanceWindow {
                        window: "5h".to_owned(),
                        limit: Some(100.0),
                        used: Some(20.0),
                        remaining: Some(80.0),
                        resets_at_ms: Some(1_794_052_800_000),
                    }],
                    labels: vec![
                        "Key remaining".to_owned(),
                        "Sub2API: key quota exhausted".to_owned()
                    ],
                    exhausted: true,
                    ..BalanceReport::default()
                }
            ),
            1
        )
    );

    let expired = json!({"mode": "quota_limited", "status": "expired", "unit": "usd",
        "quota": {"remaining": 3}});
    let (report, _) = sub2api(&expired).await;
    assert_eq!(
        (
            report.unit,
            report.remaining,
            report.remaining_usd,
            report.exhausted
        ),
        (BalanceUnit::Usd, Some(3.0), Some(3.0), false)
    );
    assert_eq!(report.labels, ["Key remaining", "Sub2API: key expired"]);

    let windows_only = json!({"mode": "quota_limited", "unit": "USD",
        "rate_limits": [{"window": "1d", "limit": 5, "used": 1, "remaining": 4}]});
    let (report, _) = sub2api(&windows_only).await;
    assert_eq!(
        (
            report.scope,
            report.unit,
            report.remaining,
            report.remaining_usd
        ),
        (BalanceScope::Key, BalanceUnit::Unsupported, None, None)
    );
    assert_eq!(report.labels, ["Sub2API: the key has rate limits only"]);
    assert_eq!(report.rate_windows.len(), 1);
}

#[tokio::test]
async fn sub2api_unrestricted_keys_report_a_subscription_or_a_wallet() {
    let limited = json!({"mode": "unrestricted", "unit": "USD", "remaining": -1,
        "subscription": {"daily_limit_usd": 5, "daily_usage_usd": 6, "weekly_limit_usd": 20,
            "weekly_usage_usd": 4, "monthly_limit_usd": null, "expires_at": "2026-11-07T12:00:00Z"}});
    let window = |name: &str, limit: f64, used: f64, remaining: f64| BalanceWindow {
        window: name.to_owned(),
        limit: Some(limit),
        used: Some(used),
        remaining: Some(remaining),
        resets_at_ms: None,
    };
    assert_eq!(
        sub2api(&limited).await,
        (
            sub2api_report(
                &limited,
                BalanceReport {
                    scope: BalanceScope::Account,
                    unit: BalanceUnit::Usd,
                    remaining: Some(0.0),
                    remaining_usd: Some(0.0),
                    expires_at_ms: Some(1_794_052_800_000),
                    rate_windows: vec![
                        window("daily", 5.0, 6.0, 0.0),
                        window("weekly", 20.0, 4.0, 16.0)
                    ],
                    exhausted: true,
                    ..BalanceReport::default()
                }
            ),
            1
        )
    );

    let open = json!({"mode": "unrestricted", "subscription": {"daily_limit_usd": null}});
    let (report, _) = sub2api(&open).await;
    assert_eq!(
        (report.unit, report.remaining, report.exhausted),
        (BalanceUnit::Unlimited, None, false)
    );
    assert_eq!(report.labels, ["Sub2API: subscription without limits"]);

    let wallet =
        json!({"mode": "unrestricted", "balance": "7.5", "unit": "USD", "planName": "wallet"});
    let (report, _) = sub2api(&wallet).await;
    assert_eq!(
        (report.unit, report.remaining, report.remaining_usd),
        (BalanceUnit::Usd, Some(7.5), Some(7.5))
    );
    assert_eq!(report.plan_name.as_deref(), Some("wallet"));
    assert!(report.labels.is_empty());

    let bare = json!({"mode": "unrestricted"});
    let (report, _) = sub2api(&bare).await;
    assert_eq!(
        (report.unit, report.remaining),
        (BalanceUnit::Unsupported, None)
    );
    assert_eq!(report.labels, ["Sub2API: subscription details unavailable"]);

    let http = routes(&[("/v1/usage", ok(&json!({"foo": 1})))]);
    let (error, used) = failed(check(&http, &target(GatewayFamily::Sub2Api)).await);
    assert_eq!(
        error,
        self::error(
            BalanceErrorKind::SchemaMismatch,
            "The answer has neither mode",
            None,
            Some("/v1/usage")
        )
    );
    assert_eq!(used, 1);
}

#[tokio::test]
async fn litellm_the_key_budget_is_a_key_cap_with_the_user_budget_beside_it() {
    let key = json!({"info": {"spend": 1.5, "max_budget": 10, "budget_duration": "30d",
        "budget_reset_at": "2026-11-07T12:00:00Z"}});
    let user = json!({"spend": 2.0, "max_budget": 50});
    let http = routes(&[("/key/info", ok(&key)), ("/v2/user/info", ok(&user))]);
    let (report, used) = reported(check(&http, &target(GatewayFamily::LiteLlm)).await);
    assert_eq!(
        report,
        sealed(
            UPSTREAM,
            json!({"/key/info": key, "/v2/user/info": user}),
            BalanceReport {
                scope: BalanceScope::Key,
                source_endpoint: Some("/key/info".to_owned()),
                unit: BalanceUnit::Usd,
                used: Some(1.5),
                key_limit_remaining_usd: Some(8.5),
                rate_windows: vec![BalanceWindow {
                    window: "30d".to_owned(),
                    limit: Some(10.0),
                    used: Some(1.5),
                    remaining: Some(8.5),
                    resets_at_ms: Some(1_794_052_800_000),
                }],
                labels: vec!["Budget".to_owned(), "User budget left 48".to_owned()],
                ..BalanceReport::default()
            }
        )
    );
    assert_eq!(used, 2);
}

#[tokio::test]
async fn litellm_falls_back_to_the_user_budget_scoped_to_the_account() {
    // Pins today's fallback: the user budget lands in key_limit_remaining_usd with the Account
    // scope, so remaining stays None and a spent user budget is never marked exhausted (known gap,
    // deliberately unchanged).
    let user = json!({"spend": 3.0, "max_budget": 10});
    let http = routes(&[("/key/info", not_found()), ("/v2/user/info", ok(&user))]);
    let (report, used) = reported(check(&http, &target(GatewayFamily::LiteLlm)).await);
    assert_eq!(
        report,
        sealed(
            UPSTREAM,
            user,
            BalanceReport {
                scope: BalanceScope::Account,
                source_endpoint: Some("/v2/user/info".to_owned()),
                unit: BalanceUnit::Usd,
                used: Some(3.0),
                key_limit_remaining_usd: Some(7.0),
                labels: vec!["User budget".to_owned()],
                ..BalanceReport::default()
            }
        )
    );
    assert_eq!(used, 2);

    let unset = json!({"info": {"spend": 1.5, "max_budget": null}});
    let http = routes(&[("/key/info", ok(&unset)), ("/v2/user/info", not_found())]);
    let (report, _) = reported(check(&http, &target(GatewayFamily::LiteLlm)).await);
    assert_eq!(
        (report.scope, report.used, report.key_limit_remaining_usd),
        (BalanceScope::Key, Some(1.5), None)
    );
    assert_eq!(report.labels, ["No budget set"]);
    assert_eq!(
        report.raw_json, unset,
        "a failed user answer is not part of raw_json"
    );
}

#[tokio::test]
async fn litellm_errors_come_from_the_key_endpoint() {
    let both = routes(&[("/key/info", not_found()), ("/v2/user/info", not_found())]);
    let (error, used) = failed(check(&both, &target(GatewayFamily::LiteLlm)).await);
    assert_eq!(
        error,
        self::error(
            BalanceErrorKind::NotFound,
            "Balance unavailable: HTTP 404",
            Some(404),
            Some("/key/info")
        )
    );
    assert_eq!(used, 2);

    let rejected = routes(&[("/key/info", status(401)), ("/v2/user/info", not_found())]);
    let (error, used) = failed(check(&rejected, &target(GatewayFamily::LiteLlm)).await);
    assert_eq!((error.kind, used), (BalanceErrorKind::Auth, 1));

    let key = json!({"info": {"spend": 1.0, "max_budget": 2}});
    let one = routes(&[("/key/info", ok(&key)), ("/v2/user/info", ok(&json!({})))]);
    let mut t = target(GatewayFamily::LiteLlm);
    t.request_budget = 1;
    let (report, used) = reported(check(&one, &t).await);
    assert_eq!((used, report.raw_json), (1, key));
}

fn voapi_target() -> BalanceTarget<'static> {
    BalanceTarget {
        management_token: Some(MGMT),
        ..target(GatewayFamily::VoApi)
    }
}

#[tokio::test]
async fn voapi_sums_basic_and_bind_balances_in_the_account_currency() {
    let body = json!({"code": 0, "data": {"basicBalance": "3.00", "bindBalance": "0.50",
        "usedBasicBalance": "1.25", "usedBindBalance": 0.25, "currency": " cny "}});
    let http = routes(&[("/api/user/info", ok(&body))]);
    let (report, used) = reported(check(&http, &voapi_target()).await);
    assert_eq!(
        report,
        sealed(
            Evidence::ThirdPartySource,
            body,
            BalanceReport {
                source_endpoint: Some("/api/user/info".to_owned()),
                unit: BalanceUnit::Cny,
                remaining: Some(3.5),
                used: Some(1.5),
                labels: vec!["VoAPI: unconfirmed".to_owned()],
                ..BalanceReport::default()
            }
        )
    );
    assert_eq!(used, 1);
    assert_eq!(
        header_of(&http, 0, "authorization"),
        Some(format!("Bearer {MGMT}"))
    );

    let usd = http_voapi(&json!({"code": 0, "data": {"bindBalance": 2, "currency": "usd"}}));
    let (report, _) = reported(check(&usd, &voapi_target()).await);
    assert_eq!(
        (
            report.unit,
            report.remaining,
            report.remaining_usd,
            report.used
        ),
        (BalanceUnit::Usd, Some(2.0), Some(2.0), None)
    );
}

fn http_voapi(body: &Value) -> ScriptedHttp {
    routes(&[("/api/user/info", ok(body))])
}

#[tokio::test]
async fn voapi_console_failures_have_their_own_classes() {
    let none = ScriptedHttp::new();
    let (error, used) = failed(check(&none, &target(GatewayFamily::VoApi)).await);
    assert_eq!(
        error,
        self::error(
            BalanceErrorKind::NeedsConsoleToken,
            "VoAPI balances need a console token",
            None,
            None
        )
    );
    assert_eq!((used, none.count()), (0, 0));

    let expired = http_voapi(&json!({"code": 2, "msg": "expired"}));
    assert_eq!(
        failed(check(&expired, &voapi_target()).await).0,
        self::error(
            BalanceErrorKind::ConsoleTokenExpired,
            "The console session has expired",
            None,
            Some("/api/user/info")
        )
    );

    let message = format!("bad   token {MGMT}");
    let other = http_voapi(&json!({"code": 1, "msg": message}));
    let (error, _) = failed(check(&other, &voapi_target()).await);
    let safe = one_line(&redact_secrets(
        &message,
        &[KEY.to_owned(), MGMT.to_owned()],
    ));
    assert!(
        !safe.contains(MGMT) && safe.starts_with("bad token "),
        "{safe}"
    );
    assert_eq!(
        error,
        self::error(
            BalanceErrorKind::SchemaMismatch,
            &safe,
            None,
            Some("/api/user/info")
        )
    );
    let silent = http_voapi(&json!({"code": 1}));
    assert_eq!(
        failed(check(&silent, &voapi_target()).await).0.message,
        "code is not 0"
    );

    let rejected = routes(&[("/api/user/info", status(401))]);
    assert_eq!(
        failed(check(&rejected, &voapi_target()).await),
        (
            self::error(
                BalanceErrorKind::ConsoleTokenExpired,
                "The console token was rejected",
                Some(401),
                Some("/api/user/info")
            ),
            1
        )
    );

    let empty = http_voapi(&json!({"code": 0, "data": {"currency": "CNY"}}));
    assert_eq!(
        failed(check(&empty, &voapi_target()).await).0,
        self::error(
            BalanceErrorKind::SchemaMismatch,
            "data.basicBalance and data.bindBalance are missing",
            None,
            Some("/api/user/info")
        )
    );
}
