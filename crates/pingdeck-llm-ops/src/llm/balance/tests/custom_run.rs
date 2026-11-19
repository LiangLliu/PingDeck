//! Running the saved custom query: key injection, units, the host rule, declared headers, echo
//! redaction and the custom-wins rule.

use super::*;

#[test]
fn a_custom_query_wins_and_removing_it_restores_the_adapter() {
    let custom = query(
        "https://api.moonshot.cn/v1/x",
        BalanceAuth::Bearer,
        UnitName::Cny,
    );
    let with = with_custom(vendor("moonshot", "https://api.moonshot.cn"), &custom);
    assert_eq!(select_adapter(&with), Some(AdapterId::Custom));
    let without = BalanceTarget {
        custom: None,
        ..with
    };
    assert_eq!(select_adapter(&without), Some(AdapterId::Moonshot));
    let bare = with_custom(target(GatewayFamily::Ollama), &custom);
    assert_eq!(select_adapter(&bare), Some(AdapterId::Custom));
    assert_eq!(
        select_adapter(&BalanceTarget {
            custom: None,
            ..bare
        }),
        None,
        "removing it restores No balance API"
    );
}

#[tokio::test]
async fn the_key_is_injected_in_the_chosen_auth_style() {
    for (auth, expect) in [
        (
            BalanceAuth::Bearer,
            vec![("Authorization", format!("Bearer {KEY}"))],
        ),
        (BalanceAuth::XApiKey, vec![("x-api-key", KEY.to_owned())]),
        (BalanceAuth::Raw, vec![("Authorization", KEY.to_owned())]),
        (BalanceAuth::None, vec![]),
    ] {
        let http = ScriptedHttp::new().route(SafeMethod::Get, ZHIPU_PATH, ok(&zhipu_body()));
        let custom = query(ZHIPU_URL, auth, UnitName::Cny);
        let outcome = check(&http, &zhipu_with(&custom)).await;
        let report = report_of(outcome);
        assert_eq!(
            (report.unit, report.remaining),
            (BalanceUnit::Cny, Some(128.5)),
            "{auth:?}"
        );
        let sent: Vec<(String, String)> = http.requests()[0]
            .headers
            .iter()
            .filter(|(n, _)| n != "Accept")
            .cloned()
            .collect();
        let expected: Vec<(String, String)> =
            expect.into_iter().map(|(n, v)| (n.to_owned(), v)).collect();
        assert_eq!(sent, expected, "{auth:?}");
    }
}

#[tokio::test]
async fn auth_none_works_without_any_key() {
    let http = ScriptedHttp::new().route(SafeMethod::Get, ZHIPU_PATH, ok(&zhipu_body()));
    let custom = query(ZHIPU_URL, BalanceAuth::None, UnitName::Cny);
    let mut t = zhipu_with(&custom);
    t.api_key = "";
    assert_eq!(report_of(check(&http, &t).await).remaining, Some(128.5));
    let bearer = query(ZHIPU_URL, BalanceAuth::Bearer, UnitName::Cny);
    let mut t = zhipu_with(&bearer);
    t.api_key = "";
    assert_eq!(
        error_of(check(&http, &t).await).kind,
        BalanceErrorKind::Auth
    );
}

#[tokio::test]
async fn units_labels_and_divisors_follow_the_query() {
    let body = json!({"data": {"availableBalance": "6200000", "used": 1, "total": 7_200_000}});
    let run = |custom: CustomBalanceQuery| {
        let body = body.clone();
        async move {
            let http = ScriptedHttp::new().route(SafeMethod::Get, ZHIPU_PATH, ok(&body));
            report_of(check(&http, &zhipu_with(&custom)).await)
        }
    };
    let mut quota = query(ZHIPU_URL, BalanceAuth::Raw, UnitName::Quota);
    quota.divisor = Some(500_000.0);
    quota.used_path = Some("$.data.used".to_owned());
    quota.total_path = Some("$.data.total".to_owned());
    let report = run(quota).await;
    assert_eq!(
        (report.unit, report.remaining, report.used, report.total),
        (
            BalanceUnit::Quota,
            Some(6_200_000.0),
            Some(1.0),
            Some(7_200_000.0)
        )
    );
    assert_eq!(
        (report.quota_per_unit, report.remaining_usd),
        (Some(500_000.0), Some(12.4))
    );

    let undivided = run(query(ZHIPU_URL, BalanceAuth::Raw, UnitName::Quota)).await;
    assert_eq!(
        (undivided.quota_per_unit, undivided.remaining_usd),
        (None, None)
    );

    let mut custom_unit = query(ZHIPU_URL, BalanceAuth::Raw, UnitName::Custom);
    custom_unit.unit_label = Some(" credits ".to_owned());
    let report = run(custom_unit).await;
    assert_eq!(
        (report.unit, report.unit_label.as_deref()),
        (BalanceUnit::Custom, Some("credits"))
    );

    let usd = run(query(ZHIPU_URL, BalanceAuth::Raw, UnitName::Usd)).await;
    assert_eq!(usd.remaining_usd, Some(6_200_000.0));
    assert_eq!(usd.source_endpoint.as_deref(), Some(ZHIPU_PATH));
}

#[tokio::test]
async fn a_missing_path_is_a_schema_mismatch_naming_the_path() {
    let http = ScriptedHttp::new().route(SafeMethod::Get, ZHIPU_PATH, ok(&json!({"data": {}})));
    let custom = query(ZHIPU_URL, BalanceAuth::Raw, UnitName::Cny);
    let error = error_of(check(&http, &zhipu_with(&custom)).await);
    assert_eq!(error.kind, BalanceErrorKind::SchemaMismatch);
    assert!(
        error.message.contains("$.data.availableBalance"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn another_host_needs_the_confirmation_and_then_gets_the_key() {
    let other = "https://billing.example.net/api/me";
    let http = ScriptedHttp::new().route(SafeMethod::Get, "/api/me", ok(&zhipu_body()));
    let unconfirmed = query(other, BalanceAuth::Bearer, UnitName::Cny);
    assert!(unconfirmed.is_cross_host(&["https://open.bigmodel.cn".to_owned()]));
    let outcome = check(&http, &zhipu_with(&unconfirmed)).await;
    assert_eq!(error_of(outcome).kind, BalanceErrorKind::Blocked);
    assert_eq!(http.count(), 0, "nothing is sent to an unconfirmed host");

    let mut confirmed = unconfirmed;
    confirmed.cross_host = true;
    let outcome = check(&http, &zhipu_with(&confirmed)).await;
    let report = report_of(outcome);
    assert_eq!(
        report.source_endpoint.as_deref(),
        Some("billing.example.net/api/me")
    );
    assert_eq!(http.requests()[0].url, other);
    assert_eq!(
        header_of(&http, 0, "authorization").as_deref(),
        Some(&*format!("Bearer {KEY}"))
    );
}

#[tokio::test]
async fn a_confirmed_cross_host_query_is_the_only_way_off_the_stored_origins() {
    let http = ScriptedHttp::new().route(SafeMethod::Get, "/api/me", ok(&zhipu_body()));
    let mut custom = query(
        "https://billing.example.net/api/me",
        BalanceAuth::Bearer,
        UnitName::Cny,
    );
    custom.cross_host = true;
    let t = zhipu_with(&custom);
    assert!(matches!(
        check(&http, &t).await,
        BalanceOutcome::Report { .. }
    ));
    // The same plain adapter run later goes nowhere but the stored origin.
    let plain = vendor("zhipu", "https://open.bigmodel.cn");
    let _ = check(&http, &plain).await;
    assert_eq!(
        http.requests()[1].url,
        format!("https://open.bigmodel.cn{ZHIPU_PATH}")
    );
}

#[tokio::test]
async fn declared_headers_are_sent_only_with_resolved_values() {
    let http = ScriptedHttp::new().route(SafeMethod::Get, ZHIPU_PATH, ok(&zhipu_body()));
    let mut custom = query(ZHIPU_URL, BalanceAuth::Raw, UnitName::Cny);
    custom.headers = vec![CustomHeaderRef {
        name: "X-Org".to_owned(),
        secret_ref: "ref".to_owned(),
    }];
    let t = zhipu_with(&custom);
    let outcome = check(&http, &t).await;
    let error = error_of(outcome);
    assert_eq!(error.kind, BalanceErrorKind::Blocked);
    assert!(error.message.contains("X-Org") && !error.message.contains("ref"));
    assert_eq!(http.count(), 0);

    let mut resolved = custom.clone();
    resolved.resolved_headers = ResolvedHeaders(vec![
        ("x-org".to_owned(), SECRET_VALUE.to_owned()),
        ("X-Undeclared".to_owned(), "never sent".to_owned()),
    ]);
    let t = zhipu_with(&resolved);
    let report = report_of(check(&http, &t).await);
    assert_eq!(header_of(&http, 0, "X-Org").as_deref(), Some(SECRET_VALUE));
    assert_eq!(header_of(&http, 0, "X-Undeclared"), None);
    assert!(
        !serde_json::to_string(&report)
            .unwrap_or_default()
            .contains(SECRET_VALUE)
    );

    let mut injected = custom;
    injected.resolved_headers =
        ResolvedHeaders(vec![("X-Org".to_owned(), "a\r\nX-Evil: 1".to_owned())]);
    let t = zhipu_with(&injected);
    assert_eq!(
        error_of(check(&http, &t).await).kind,
        BalanceErrorKind::Blocked
    );
    assert_eq!(http.count(), 1, "the control character stopped the request");
}

#[tokio::test]
async fn a_header_value_echoed_by_the_service_is_redacted_everywhere() {
    let echo = json!({"data": {"availableBalance": 5, "echo": SECRET_VALUE}, "msg": SECRET_VALUE});
    let http = ScriptedHttp::new().route(SafeMethod::Get, ZHIPU_PATH, ok(&echo));
    let mut custom = query(ZHIPU_URL, BalanceAuth::Raw, UnitName::Cny);
    custom.headers = vec![CustomHeaderRef {
        name: "X-Org".to_owned(),
        secret_ref: "ref".to_owned(),
    }];
    custom.resolved_headers = ResolvedHeaders(vec![("X-Org".to_owned(), SECRET_VALUE.to_owned())]);
    let t = zhipu_with(&custom);
    let report = report_of(check(&http, &t).await);
    assert!(!report.raw_json.to_string().contains(SECRET_VALUE));
    let tested = run_test_query(&http, &t, &custom, &CancelToken::new()).await;
    assert!(!tested.body.contains(SECRET_VALUE), "{}", tested.body);
}

// The saved query's schema error goes through `safe_text` like the
// Test query's, so both report the same bounded, one-line text.
#[tokio::test]
async fn a_saved_query_and_the_test_query_report_the_same_bounded_schema_error() {
    let http = ScriptedHttp::new().route(SafeMethod::Get, ZHIPU_PATH, ok(&json!({"data": {}})));
    let mut custom = query(ZHIPU_URL, BalanceAuth::Raw, UnitName::Cny);
    custom.remaining_path = format!("$.data.{}", "a".repeat(250));
    assert_eq!(custom.validate(), Ok(()));
    let saved = error_of(check(&http, &zhipu_with(&custom)).await);
    let t = vendor("zhipu", "https://open.bigmodel.cn");
    let tested = run_test_query(&http, &t, &custom, &CancelToken::new()).await;
    let previewed = tested
        .preview_error
        .unwrap_or_else(|| panic!("expected a preview error"));
    assert_eq!(saved, previewed);
    assert_eq!(saved.kind, BalanceErrorKind::SchemaMismatch);
    assert_eq!(saved.endpoint.as_deref(), Some(ZHIPU_PATH));
    assert_eq!(saved.message.chars().count(), 200, "{}", saved.message);
    assert!(saved.message.starts_with("$.data.aaa"), "{}", saved.message);
}
