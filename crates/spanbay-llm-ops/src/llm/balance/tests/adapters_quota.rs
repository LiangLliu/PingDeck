//! End-to-end readings of the quota-unit gateway adapters (one-api, the new-api family) through
//! `query_balance`: every field of the report, the labels in order, the raw body and the failure
//! classes with their request counts.

use spanbay_base::secrets::redact_secrets;

use super::*;

const SUBSCRIPTION: &str = "/v1/dashboard/billing/subscription";
const USAGE: &str = "/v1/dashboard/billing/usage";
const UPSTREAM: Evidence = Evidence::UpstreamSource;

#[tokio::test]
async fn one_api_an_unlimited_token_is_one_request() {
    let sub = json!({"hard_limit_usd": 100_000_000, "access_until": 1_794_052_800});
    let http = routes(&[(SUBSCRIPTION, ok(&sub))]);
    let (report, used) = reported(check(&http, &target(GatewayFamily::OneApi)).await);
    assert_eq!(
        report,
        sealed(
            UPSTREAM,
            sub,
            BalanceReport {
                scope: BalanceScope::Key,
                source_endpoint: Some(SUBSCRIPTION.to_owned()),
                unit: BalanceUnit::Unlimited,
                expires_at_ms: Some(1_794_052_800_000),
                labels: vec![
                    "Key remaining".to_owned(),
                    "one-api: unlimited token".to_owned()
                ],
                ..BalanceReport::default()
            }
        )
    );
    assert_eq!((used, http.count()), (1, 1));
}

#[tokio::test]
async fn one_api_units_follow_display_in_currency_from_the_target_or_the_status() {
    let sub = json!({"hard_limit_usd": 50, "access_until": 0});
    let usage = json!({"total_usage": 3750});
    let raw = json!({SUBSCRIPTION: sub, USAGE: usage});
    let reading = |unit, quota_per_unit, remaining_usd, label: &str| {
        sealed(
            UPSTREAM,
            raw.clone(),
            BalanceReport {
                scope: BalanceScope::Key,
                source_endpoint: Some(SUBSCRIPTION.to_owned()),
                unit,
                total: Some(50.0),
                used: Some(37.5),
                remaining: Some(12.5),
                quota_per_unit,
                remaining_usd,
                labels: vec!["Key remaining".to_owned(), label.to_owned()],
                ..BalanceReport::default()
            },
        )
    };
    let usd = reading(
        BalanceUnit::Usd,
        None,
        Some(12.5),
        "one-api: hard limit minus usage / 100",
    );
    let quota = reading(
        BalanceUnit::Quota,
        Some(500_000.0),
        Some(12.5 / 500_000.0),
        "one-api: quota units (display_in_currency is false)",
    );
    let unknown = reading(
        BalanceUnit::Unsupported,
        None,
        None,
        "one-api: display_in_currency unknown, unit unknown",
    );

    // The target says currency: no status request.
    let base = routes(&[(SUBSCRIPTION, ok(&sub)), (USAGE, ok(&usage))]);
    let mut t = target(GatewayFamily::OneApi);
    t.display_in_currency = Some(true);
    assert_eq!(reported(check(&base, &t).await), (usd.clone(), 2));
    assert_eq!(base.count(), 2);

    for (status_body, expected) in [
        (
            ok(&json!({"data": {"quota_per_unit": 500_000, "display_in_currency": true}})),
            usd,
        ),
        (
            ok(&json!({"data": {"quota_per_unit": 500_000, "display_in_currency": false}})),
            quota,
        ),
        (not_found(), unknown),
    ] {
        let http = routes(&[
            (SUBSCRIPTION, ok(&sub)),
            (USAGE, ok(&usage)),
            ("/api/status", status_body),
        ]);
        let outcome = check(&http, &target(GatewayFamily::OneApi)).await;
        assert_eq!(reported(outcome), (expected, 3));
        assert_eq!(header_of(&http, 2, "authorization"), None);
    }
}

#[tokio::test]
async fn one_api_an_error_object_in_a_200_is_a_mismatch_with_safe_text() {
    let message = format!("  bad \n key {KEY}   now ");
    let http = routes(&[(SUBSCRIPTION, ok(&json!({"error": {"message": message}})))]);
    let (error, used) = failed(check(&http, &target(GatewayFamily::OneApi)).await);
    let safe = one_line(&redact_secrets(&message, &[KEY.to_owned()]));
    assert!(
        !safe.contains(KEY) && safe.starts_with("bad key "),
        "{safe}"
    );
    assert_eq!(
        error,
        self::error(
            BalanceErrorKind::SchemaMismatch,
            &safe,
            None,
            Some(SUBSCRIPTION)
        )
    );
    assert_eq!(used, 1);
    let silent = routes(&[(SUBSCRIPTION, ok(&json!({"error": {"code": 1}})))]);
    let (error, _) = failed(check(&silent, &target(GatewayFamily::OneApi)).await);
    assert_eq!(error.message, "The service reported an error");
}

fn console_target(family: GatewayFamily) -> BalanceTarget<'static> {
    BalanceTarget {
        management_token: Some(MGMT),
        api_user_id: Some("42"),
        ..target(family)
    }
}

#[tokio::test]
async fn new_api_the_console_reads_the_account_with_the_family_header() {
    let body = json!({"success": true, "data": {"quota": 6_200_000, "used_quota": 1_000_000}});
    for (family, header, brand) in [
        (GatewayFamily::NewApi, "New-API-User", "new-api"),
        (GatewayFamily::Veloera, "Veloera-User", "Veloera"),
        (GatewayFamily::RixApi, "Rix-Api-User", "RixAPI"),
        (GatewayFamily::DoneHub, "New-API-User", "done-hub"),
    ] {
        let http = routes(&[
            ("/api/user/self", ok(&body)),
            (
                "/api/status",
                ok(&json!({"data": {"quota_per_unit": 500_000}})),
            ),
        ]);
        let (report, used) = reported(check(&http, &console_target(family)).await);
        assert_eq!(
            report,
            sealed(
                UPSTREAM,
                body.clone(),
                BalanceReport {
                    scope: BalanceScope::Account,
                    source_endpoint: Some("/api/user/self".to_owned()),
                    unit: BalanceUnit::Quota,
                    remaining: Some(6_200_000.0),
                    used: Some(1_000_000.0),
                    total: Some(7_200_000.0),
                    quota_per_unit: Some(500_000.0),
                    remaining_usd: Some(6_200_000.0 / 500_000.0),
                    labels: vec![format!("{brand}: quota is remaining")],
                    ..BalanceReport::default()
                }
            ),
            "{family:?}"
        );
        assert_eq!(used, 2, "{family:?}");
        let names: Vec<String> = http.requests()[0]
            .headers
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        assert_eq!(names, ["Accept", "Authorization", header], "{family:?}");
        assert_eq!(
            header_of(&http, 0, "authorization"),
            Some(format!("Bearer {MGMT}"))
        );
        assert_eq!(header_of(&http, 0, header).as_deref(), Some("42"));
    }
}

#[tokio::test]
async fn new_api_a_stored_divisor_saves_the_status_request() {
    let body = json!({"success": true, "data": {"quota": 1_000_000}});
    let http = routes(&[("/api/user/self", ok(&body))]);
    let mut t = console_target(GatewayFamily::NewApi);
    t.quota_per_unit = Some(500_000.0);
    let (report, used) = reported(check(&http, &t).await);
    assert_eq!(
        (report.remaining, report.used, report.total),
        (Some(1_000_000.0), None, None)
    );
    assert_eq!(
        (report.quota_per_unit, report.remaining_usd),
        (Some(500_000.0), Some(2.0))
    );
    assert_eq!((used, http.count()), (1, 1));
}

#[tokio::test]
async fn new_api_a_rejected_console_token_is_console_token_expired() {
    let rejected = routes(&[("/api/user/self", status(401))]);
    let (error, used) = failed(check(&rejected, &console_target(GatewayFamily::NewApi)).await);
    assert_eq!(
        error,
        self::error(
            BalanceErrorKind::ConsoleTokenExpired,
            "The console token was rejected",
            Some(401),
            Some("/api/user/self")
        )
    );
    assert_eq!(used, 1);

    let ended = routes(&[(
        "/api/user/self",
        ok(&json!({"success": false, "message": format!("  session \t ended {MGMT} ")})),
    )]);
    let (error, used) = failed(check(&ended, &console_target(GatewayFamily::NewApi)).await);
    let safe = one_line(&redact_secrets(
        &format!("  session \t ended {MGMT} "),
        &[KEY.to_owned(), MGMT.to_owned()],
    ));
    assert!(
        !safe.contains(MGMT) && safe.starts_with("session ended "),
        "{safe}"
    );
    assert_eq!(
        error,
        self::error(
            BalanceErrorKind::ConsoleTokenExpired,
            &safe,
            None,
            Some("/api/user/self")
        )
    );
    assert_eq!(used, 1);
}

#[tokio::test]
async fn new_api_key_readings_name_their_scope_and_brand() {
    let account = json!({"code": true, "data": {"total_available": 2_000_000, "user_quota": {"total_available": 500_000}}});
    let http1 = routes(&[
        ("/api/usage/token/", ok(&account)),
        (
            "/api/status",
            ok(&json!({"data": {"quota_per_unit": 500_000}})),
        ),
    ]);
    let (report, used) = reported(check(&http1, &target(GatewayFamily::NewApi)).await);
    assert_eq!(
        report,
        sealed(
            UPSTREAM,
            account,
            BalanceReport {
                scope: BalanceScope::Account,
                source_endpoint: Some("/api/usage/token/".to_owned()),
                unit: BalanceUnit::Quota,
                remaining: Some(500_000.0),
                quota_per_unit: Some(500_000.0),
                remaining_usd: Some(1.0),
                key_limit_remaining_usd: Some(4.0),
                labels: vec!["new-api: account balance, key quota as key limit".to_owned()],
                ..BalanceReport::default()
            }
        )
    );
    assert_eq!(used, 2);

    let unlimited = json!({"code": true, "data": {"total_available": 0, "unlimited_quota": true}});
    let http2 = routes(&[("/api/usage/token/", ok(&unlimited))]);
    let (report, used) = reported(check(&http2, &target(GatewayFamily::DoneHub)).await);
    assert_eq!(
        report,
        sealed(
            UPSTREAM,
            unlimited,
            BalanceReport {
                scope: BalanceScope::Key,
                source_endpoint: Some("/api/usage/token/".to_owned()),
                unit: BalanceUnit::Unlimited,
                labels: vec!["done-hub: unlimited key".to_owned()],
                ..BalanceReport::default()
            }
        )
    );
    assert_eq!(used, 1, "an unlimited key needs no divisor");
}
