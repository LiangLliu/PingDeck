//! The transport rules every adapter shares: bounded `GET`s, the origin and budget limits, cancel,
//! and the mapping of transport and status failures.

use crate::llm::safe_http::SafeTransportError;

use super::*;

#[tokio::test]
async fn every_request_is_a_bounded_get_with_the_adapters_own_headers() {
    let http = ScriptedHttp::new().route(
        SafeMethod::Get,
        "/user/balance",
        ok(&json!({"is_available": true, "balance_infos": [
            {"currency": "CNY", "total_balance": "86.40"}]})),
    );
    let mut t = vendor("deepseek", "https://api.deepseek.com");
    t.allow_private = true;
    let report = report_of(check(&http, &t).await);
    assert_eq!(report.remaining, Some(86.4));
    let request = &http.requests()[0];
    assert_eq!(request.method, SafeMethod::Get);
    assert_eq!(request.url, "https://api.deepseek.com/user/balance");
    assert_eq!(request.timeout, std::time::Duration::from_secs(10));
    assert_eq!(request.max_body, 256 * 1024);
    assert!(request.allow_private);
    assert!(request.body.is_none());
    let names: Vec<&str> = request
        .headers
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(names, ["Accept", "Authorization"]);
    assert_eq!(
        header_of(&http, 0, "authorization").as_deref(),
        Some(&*format!("Bearer {KEY}"))
    );
}

#[tokio::test]
async fn a_body_naming_another_host_causes_no_request_to_it() {
    let evil = json!({
        "is_available": true,
        "balance_infos": [{"currency": "CNY", "total_balance": "1.00"}],
        "next": "https://evil.test/balance",
        "url": "https://evil.test/balance",
        "balance_url": "//evil.test/balance",
    });
    let http = ScriptedHttp::new()
        .route(SafeMethod::Get, "/user/balance", ok(&evil))
        .route(
            SafeMethod::Get,
            "/api/usage/token/",
            ok(&json!({
            "code": true, "data": {"total_available": 5, "unlimited_quota": false},
            "redirect": "https://evil.test/balance"})),
        )
        .route(
            SafeMethod::Get,
            "/api/status",
            ok(&json!({
            "data": {"quota_per_unit": 500000, "server_address": "https://evil.test"}})),
        );
    let _ = check(&http, &vendor("deepseek", "https://api.deepseek.com")).await;
    let _ = check(&http, &target(GatewayFamily::NewApi)).await;
    for request in http.requests() {
        assert!(!request.url.contains("evil.test"), "{}", request.url);
    }
    assert!(http.count() >= 3);
}

#[tokio::test]
async fn the_key_goes_only_to_the_provider_origin() {
    let http = all_ok_http();
    let outcome = check(&http, &target(GatewayFamily::NewApi)).await;
    assert_eq!(http.count(), 2, "{outcome:?}");
    for (index, request) in http.requests().iter().enumerate() {
        assert!(
            request.url.starts_with("https://relay.acme.dev/"),
            "{}",
            request.url
        );
        let credential = header_of(&http, index, "authorization");
        if request.url.ends_with("/api/status") {
            assert_eq!(
                credential, None,
                "the key is not sent to the public status endpoint"
            );
        }
        assert!(header_of(&http, index, "cookie").is_none());
    }
}

#[tokio::test]
async fn the_key_goes_only_to_a_stored_endpoint_origin() {
    let http = all_ok_http();
    // The origin of the check is not among the stored endpoints: nothing is sent.
    let stored = vec!["https://other.acme.dev".to_owned()];
    let mut t = target(GatewayFamily::NewApi);
    t.endpoint_origins = &stored;
    let error = error_of(check(&http, &t).await);
    assert_eq!(error.kind, BalanceErrorKind::Blocked);
    t.endpoint_origins = &[];
    assert_eq!(
        error_of(check(&http, &t).await).kind,
        BalanceErrorKind::Blocked
    );
    assert_eq!(http.count(), 0);

    // A stored origin written with a path or another case still matches.
    let loose = vec![
        "HTTPS://Relay.Acme.dev/v1".to_owned(),
        "https://second.acme.dev".to_owned(),
    ];
    t.endpoint_origins = &loose;
    assert!(matches!(
        check(&http, &t).await,
        BalanceOutcome::Report { .. }
    ));
    assert!(
        http.requests()
            .iter()
            .all(|request| request.url.starts_with("https://relay.acme.dev/"))
    );
}

#[tokio::test]
async fn an_unusable_origin_or_budget_sends_nothing() {
    let http = ScriptedHttp::new();
    let mut t = target(GatewayFamily::NewApi);
    t.origin = "ftp://relay.acme.dev";
    assert_eq!(
        error_of(check(&http, &t).await).kind,
        BalanceErrorKind::Blocked
    );
    let mut t = target(GatewayFamily::NewApi);
    t.request_budget = 0;
    assert_eq!(
        error_of(check(&http, &t).await).kind,
        BalanceErrorKind::Blocked
    );
    let mut t = target(GatewayFamily::NewApi);
    t.api_key = "";
    assert_eq!(
        error_of(check(&http, &t).await).kind,
        BalanceErrorKind::Auth
    );
    assert_eq!(http.count(), 0);
}

#[tokio::test]
async fn a_cancelled_token_sends_nothing() {
    let http = ScriptedHttp::new();
    let cancel = CancelToken::new();
    cancel.cancel();
    let outcome = query_balance(&http, &target(GatewayFamily::NewApi), &cancel, NOW).await;
    assert_eq!(error_of(outcome).kind, BalanceErrorKind::Cancelled);
    assert_eq!(http.count(), 0);
}

#[tokio::test]
async fn transport_failures_map_to_their_classes() {
    for (error, kind) in [
        (SafeTransportError::Timeout, BalanceErrorKind::Transient),
        (SafeTransportError::Dns, BalanceErrorKind::Network),
        (SafeTransportError::Refused, BalanceErrorKind::Network),
        (SafeTransportError::Tls, BalanceErrorKind::Network),
        (SafeTransportError::Reset, BalanceErrorKind::Network),
        (
            SafeTransportError::Blocked {
                reason: "private".to_owned(),
            },
            BalanceErrorKind::Blocked,
        ),
        (SafeTransportError::Cancelled, BalanceErrorKind::Cancelled),
    ] {
        let http =
            ScriptedHttp::new().route(SafeMethod::Get, "/user/balance", vec![Reply::Error(error)]);
        let outcome = check(&http, &vendor("deepseek", "https://api.deepseek.com")).await;
        let BalanceOutcome::Failed {
            error,
            requests_used,
        } = outcome
        else {
            panic!("expected a failure");
        };
        assert_eq!((error.kind, requests_used), (kind, 1));
        assert_eq!(error.endpoint.as_deref(), Some("/user/balance"));
    }
}

#[tokio::test]
async fn every_adapter_maps_status_classes_the_same_way() {
    let targets: Vec<(BalanceTarget<'static>, &str)> = vec![
        (
            vendor("deepseek", "https://api.deepseek.com"),
            "/user/balance",
        ),
        (
            vendor("openrouter", "https://openrouter.ai"),
            "/api/v1/credits",
        ),
        (
            vendor("moonshot", "https://api.moonshot.cn"),
            "/v1/users/me/balance",
        ),
        (target(GatewayFamily::NewApi), "/api/usage/token/"),
        (
            target(GatewayFamily::OneApi),
            "/v1/dashboard/billing/subscription",
        ),
        (target(GatewayFamily::Sub2Api), "/v1/usage"),
        (target(GatewayFamily::LiteLlm), "/key/info"),
    ];
    for (target, path) in &targets {
        for (status, kind) in [
            (404, BalanceErrorKind::NotFound),
            (429, BalanceErrorKind::RateLimited),
            (503, BalanceErrorKind::Transient),
            (401, BalanceErrorKind::Auth),
            (403, BalanceErrorKind::Auth),
        ] {
            let http = ScriptedHttp::new().route(
                SafeMethod::Get,
                path,
                vec![Reply::text(status, "text/plain", "no")],
            );
            let outcome = check(&http, target).await;
            let BalanceOutcome::Failed { error, .. } = outcome else {
                panic!("{path} {status}: expected a failure");
            };
            // LiteLLM asks /v2/user/info after any non-auth failure; OpenRouter asks /api/v1/key
            // only after 401/403. Both fallbacks are 404 here.
            assert_eq!(error.kind, kind, "{path} {status}");
            assert!(
                error.status.is_some() || error.kind == BalanceErrorKind::NotFound,
                "{path} {status}"
            );
        }
        let html = ScriptedHttp::new().route(
            SafeMethod::Get,
            path,
            vec![Reply::text(200, "text/html", "<html></html>")],
        );
        let error = error_of(check(&html, target).await);
        assert_eq!(error.kind, BalanceErrorKind::SchemaMismatch, "{path}");
        let big = ScriptedHttp::new().route(
            SafeMethod::Get,
            path,
            vec![Reply::text(
                200,
                "application/json",
                &format!("{{\"pad\":\"{}\"}}", "x".repeat(300 * 1024)),
            )],
        );
        assert_eq!(
            error_of(check(&big, target).await).kind,
            BalanceErrorKind::SchemaMismatch,
            "{path} oversize"
        );
    }
}

#[tokio::test]
async fn requests_used_never_exceeds_the_budget_and_matches_the_transport() {
    for family in [
        GatewayFamily::NewApi,
        GatewayFamily::DoneHub,
        GatewayFamily::OneApi,
        GatewayFamily::Sub2Api,
        GatewayFamily::LiteLlm,
        GatewayFamily::VoApi,
        GatewayFamily::OpenRouter,
    ] {
        for budget in 0..=4_u32 {
            let http = all_ok_http();
            let mut t = target(family);
            t.management_token = Some("mgmt-token-value");
            t.api_user_id = Some("5");
            t.request_budget = budget;
            let outcome = check(&http, &t).await;
            let used = match &outcome {
                BalanceOutcome::Report { requests_used, .. }
                | BalanceOutcome::Failed { requests_used, .. } => *requests_used,
                BalanceOutcome::NoAdapter => panic!("no adapter for {family:?}"),
            };
            assert!(used <= budget, "{family:?} budget {budget} used {used}");
            assert_eq!(http.count(), used as usize, "{family:?} budget {budget}");
        }
    }
}
