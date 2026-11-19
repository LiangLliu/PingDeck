//! The one-shot "Test query" of the custom-query form.

use crate::llm::safe_http::SafeTransportError;

use super::*;

#[tokio::test]
async fn test_query_returns_the_redacted_body_and_a_preview_after_one_request() {
    let http = ScriptedHttp::new().route(SafeMethod::Get, ZHIPU_PATH, ok(&zhipu_body()));
    let custom = query(ZHIPU_URL, BalanceAuth::Raw, UnitName::Cny);
    let t = vendor("zhipu", "https://open.bigmodel.cn");
    let tested = run_test_query(&http, &t, &custom, &CancelToken::new()).await;
    assert_eq!(http.count(), 1);
    assert_eq!(tested.status, 200);
    assert!(!tested.body.contains("a@b.c"), "{}", tested.body);
    let body: Value = serde_json::from_str(&tested.body).unwrap_or_default();
    assert_eq!(body["data"]["availableBalance"], json!(128.5));
    let preview = tested
        .preview
        .unwrap_or_else(|| panic!("no preview: {:?}", tested.preview_error));
    assert_eq!(
        (preview.unit, preview.remaining),
        (BalanceUnit::Cny, Some(128.5))
    );
    assert_eq!(tested.preview_error, None);
}

#[tokio::test]
async fn test_query_explains_why_there_is_no_preview() {
    let t = vendor("zhipu", "https://open.bigmodel.cn");
    let custom = query(ZHIPU_URL, BalanceAuth::Raw, UnitName::Cny);
    let cancel = CancelToken::new();
    let unauthorised = ScriptedHttp::new().route(
        SafeMethod::Get,
        ZHIPU_PATH,
        vec![Reply::json(
            401,
            &json!({"error": format!("bad key {KEY}")}),
        )],
    );
    let tested = run_test_query(&unauthorised, &t, &custom, &cancel).await;
    assert_eq!((tested.status, tested.preview.is_none()), (401, true));
    assert_eq!(
        tested.preview_error.map(|e| e.kind),
        Some(BalanceErrorKind::Auth)
    );
    assert!(!tested.body.contains(KEY) && tested.body.contains("bad key"));

    let html = ScriptedHttp::new().route(
        SafeMethod::Get,
        ZHIPU_PATH,
        vec![Reply::text(200, "text/html", &format!("<p>{KEY}</p>"))],
    );
    let tested = run_test_query(&html, &t, &custom, &cancel).await;
    assert!(!tested.body.contains(KEY) && tested.body.contains("<p>"));
    assert_eq!(
        tested.preview_error.map(|e| e.kind),
        Some(BalanceErrorKind::SchemaMismatch)
    );

    let wrong_path =
        ScriptedHttp::new().route(SafeMethod::Get, ZHIPU_PATH, ok(&json!({"data": {}})));
    let tested = run_test_query(&wrong_path, &t, &custom, &cancel).await;
    assert_eq!(tested.status, 200);
    assert_eq!(
        tested.preview_error.map(|e| e.kind),
        Some(BalanceErrorKind::SchemaMismatch)
    );
}

#[tokio::test]
async fn test_query_caps_the_body_at_256_kib() {
    let big = format!(
        "{{\"data\":{{\"availableBalance\":1,\"pad\":\"{}\"}}}}",
        "é".repeat(200 * 1024)
    );
    let http = ScriptedHttp::new().route(
        SafeMethod::Get,
        ZHIPU_PATH,
        vec![Reply::text(200, "application/json", &big)],
    );
    let custom = query(ZHIPU_URL, BalanceAuth::Raw, UnitName::Cny);
    let t = vendor("zhipu", "https://open.bigmodel.cn");
    let tested = run_test_query(&http, &t, &custom, &CancelToken::new()).await;
    assert!(tested.body.len() <= MAX_BODY_BYTES, "{}", tested.body.len());
    assert!(!tested.body.is_empty());
    assert_eq!(tested.preview, None, "a cut body is not a reading");
}

#[tokio::test]
async fn test_query_sends_nothing_for_an_invalid_or_unconfirmed_query() {
    let http = ScriptedHttp::new();
    let t = vendor("zhipu", "https://open.bigmodel.cn");
    let cancel = CancelToken::new();
    let invalid = query("ftp://x.test/a", BalanceAuth::Bearer, UnitName::Cny);
    let error = test_custom_query(&http, &t, &invalid, &cancel, NOW)
        .await
        .err()
        .unwrap_or_else(|| panic!("expected an error"));
    assert_eq!(error.kind, BalanceErrorKind::Blocked);
    let other_host = query(
        "https://billing.example.net/a",
        BalanceAuth::Bearer,
        UnitName::Cny,
    );
    let error = test_custom_query(&http, &t, &other_host, &cancel, NOW)
        .await
        .err()
        .unwrap_or_else(|| panic!("expected an error"));
    assert_eq!(error.kind, BalanceErrorKind::Blocked);
    assert_eq!(http.count(), 0);
}

#[tokio::test]
async fn test_query_surfaces_transport_failures_as_errors() {
    let http = ScriptedHttp::new().route(
        SafeMethod::Get,
        ZHIPU_PATH,
        vec![Reply::Error(SafeTransportError::Timeout)],
    );
    let custom = query(ZHIPU_URL, BalanceAuth::Raw, UnitName::Cny);
    let t = vendor("zhipu", "https://open.bigmodel.cn");
    let error = test_custom_query(&http, &t, &custom, &CancelToken::new(), NOW)
        .await
        .err()
        .unwrap_or_else(|| panic!("expected an error"));
    assert_eq!(error.kind, BalanceErrorKind::Transient);
}
