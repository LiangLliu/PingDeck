//! End-to-end behaviours of the engine against a scripted transport; these are the sniffer's
//! only whole-scan tests: refusals, cancellation, the whole-scan cap, budgets, verification,
//! regions, proxies, redaction and the re-sniff diff.

use std::{sync::Mutex, time::Duration};

use serde_json::{Value, json};

use super::{
    auth::AuthMemory,
    diff::StoredProviderView,
    engine::scan_with_limits,
    exec::Limits,
    models::MAX_PAGES,
    types::{
        BalanceFinding, KeyCheckStatus, RouteVerdict, SniffContext, SniffError, SniffEvent,
        SniffInput, SniffPhase, SniffReport, SniffWarning,
    },
};
use crate::llm::{
    safe_http::{
        SafeMethod::{Get, Post},
        SafeRequest, SafeTransportError,
        scripted::{Reply, ScriptedHttp},
    },
    wire::{GatewayFamily, Protocol},
};
use spanbay_base::cancel::CancelToken;
use spanbay_model::catalog_entry::CatalogEntry;

const KEY: &str = "sk-test-0123456789abcdefABCD";

fn models() -> Value {
    json!({"data": [{"id": "gpt-4o"}, {"id": "gemini-3.2-pro"}, {"id": "text-embedding-3"}]})
}

fn bad_request() -> Reply {
    Reply::json(400, &json!({"error": {"message": "model is required"}}))
}

fn relay() -> ScriptedHttp {
    ScriptedHttp::new()
        .route(Get, "/v1/models", vec![Reply::json(200, &models())])
        .route(Post, "/v1/chat/completions", vec![bad_request()])
        .route(Post, "/v1/embeddings", vec![bad_request()])
}

fn no_catalog(_: &str) -> Option<CatalogEntry> {
    None
}

fn context<'a>(
    memory: &'a AuthMemory,
    uses_proxy: bool,
    stored: Option<&'a StoredProviderView>,
) -> SniffContext<'a> {
    SniffContext {
        now_ms: 1_000,
        app_version: "1.2.3",
        auth_memory: memory,
        uses_proxy,
        catalog: &no_catalog,
        stored,
    }
}

struct Outcome {
    result: Result<SniffReport, SniffError>,
    events: Vec<SniffEvent>,
}

async fn run_limited(
    http: &ScriptedHttp,
    input: SniffInput,
    stored: Option<&StoredProviderView>,
    uses_proxy: bool,
    limits: Limits,
    cancel: &CancelToken,
) -> Outcome {
    let events = Mutex::new(Vec::new());
    let sink = |event: SniffEvent| {
        events.lock().unwrap().push(event);
        true
    };
    let memory = AuthMemory::new();
    let ctx = context(&memory, uses_proxy, stored);
    let result = scan_with_limits(http, input, &ctx, &sink, cancel, limits).await;
    let events = events.into_inner().unwrap();
    Outcome { result, events }
}

async fn run(http: &ScriptedHttp, input: SniffInput) -> Outcome {
    run_limited(
        http,
        input,
        None,
        false,
        Limits::scaled(100),
        &CancelToken::new(),
    )
    .await
}

fn keyed(url: &str) -> SniffInput {
    let mut input = SniffInput::new(url);
    input.api_key = Some(KEY.to_owned());
    input
}

fn chat_only(url: &str) -> SniffInput {
    let mut input = keyed(url);
    input.protocol_override = Some(Protocol::OpenaiChat);
    input
}

fn listing(models: Value) -> ScriptedHttp {
    ScriptedHttp::new()
        .route(Get, "/v1/models", vec![Reply::json(200, &models)])
        .route(Post, "/v1/chat/completions", vec![bad_request()])
}

fn header<'r>(request: &'r SafeRequest, name: &str) -> Option<&'r str> {
    request
        .headers
        .iter()
        .find(|(own, _)| own == name)
        .map(|(_, value)| value.as_str())
}

fn invalid_reason(outcome: Outcome) -> String {
    match outcome.result {
        Err(SniffError::InvalidInput { reason }) => reason,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn link_local_target_is_refused_before_any_request() {
    let http = relay();
    let outcome = run(&http, keyed("http://169.254.169.254/latest")).await;
    assert!(matches!(
        outcome.result,
        Err(SniffError::BlockedAddress { .. })
    ));
    assert_eq!(http.count(), 0);
}

#[tokio::test]
async fn unusable_addresses_are_invalid_input_before_any_request() {
    for url in ["ftp://x.test", ""] {
        let http = relay();
        let outcome = run(&http, keyed(url)).await;
        assert!(
            matches!(outcome.result, Err(SniffError::InvalidInput { .. })),
            "{url}"
        );
        assert_eq!(http.count(), 0, "{url}");
    }
}

#[tokio::test]
async fn too_many_extra_headers_is_invalid_input() {
    let mut input = keyed("https://relay.acme.dev");
    input.extra_headers = (0..9)
        .map(|n| (format!("x-h{n}"), "v".to_owned()))
        .collect();
    assert_eq!(
        invalid_reason(run(&relay(), input).await),
        "too_many_headers"
    );
}

#[tokio::test]
async fn forbidden_header_is_invalid_input() {
    let mut input = keyed("https://relay.acme.dev");
    input.extra_headers = vec![("host".to_owned(), "x".to_owned())];
    assert_eq!(invalid_reason(run(&relay(), input).await), "bad_header");
}

#[tokio::test]
async fn models_url_on_another_origin_is_invalid_input() {
    let mut input = keyed("https://relay.acme.dev");
    input.models_url = Some("https://evil.test/models".to_owned());
    assert_eq!(
        invalid_reason(run(&relay(), input).await),
        "models_url_origin"
    );
}

#[tokio::test]
async fn private_resolution_needs_confirmation() {
    let refused = || {
        vec![Reply::Error(SafeTransportError::Blocked {
            reason: "refused_address".into(),
        })]
    };
    let blocked = ScriptedHttp::new()
        .route(Get, "/api/status", refused())
        .route(Get, "/v1/models", refused())
        .route(Post, "/v1/chat/completions", refused());
    let mut typed = chat_only("https://relay.acme.dev");
    assert!(matches!(
        run(&blocked, typed.clone()).await.result,
        Err(SniffError::PrivateResolution { .. })
    ));
    typed.confirm_private = true;
    assert!(matches!(
        run(&blocked, typed).await.result,
        Err(SniffError::BlockedAddress { .. })
    ));
}

#[tokio::test]
async fn lookalike_host_holds_the_key_until_confirmed() {
    let mut lookalike = keyed("api.deepseek.co");
    let held = run(&relay(), lookalike.clone()).await.result.unwrap();
    assert!(held.needs_key_confirmation && held.warnings.contains(&SniffWarning::LookalikeHost));
    lookalike.confirm_key_send = true;
    assert!(
        !run(&relay(), lookalike)
            .await
            .result
            .unwrap()
            .needs_key_confirmation
    );
}

const PATH_TOKEN: &str = "AbCdEfGhIjKlMnOpQrStUvWx";

/// A plain-HTTP relay behind an opaque path token, with a custom Models URL, a query on the
/// typed URL and a secret extra header.
async fn custom_scan() -> (ScriptedHttp, SniffInput, Outcome) {
    let token = PATH_TOKEN;
    let http = ScriptedHttp::new()
        .route(
            Get,
            &format!("/t/{token}/custom/list"),
            vec![Reply::json(200, &models())],
        )
        .route(
            Post,
            &format!("/t/{token}/v1/chat/completions"),
            vec![bad_request()],
        );
    let mut input = chat_only(&format!("http://relay.acme.dev/t/{token}/v1?key=1"));
    input.extra_headers = vec![("X-Team".to_owned(), "blue-team-secret".to_owned())];
    input.models_url = Some(format!("/t/{token}/custom/list?all=1"));
    let outcome = run(&http, input.clone()).await;
    (http, input, outcome)
}

#[tokio::test]
async fn a_custom_models_url_over_plain_http_lists_models_and_warns() {
    let (_, _, outcome) = custom_scan().await;
    let report = outcome.result.unwrap();
    assert!(report.warnings.contains(&SniffWarning::PlainHttpPublic));
    assert_eq!(report.models.len(), 3);
}

#[tokio::test]
async fn extra_headers_go_on_every_request_but_never_into_the_report() {
    let (http, input, outcome) = custom_scan().await;
    let report = outcome.result.unwrap();
    for req in http.requests() {
        assert!(
            req.headers
                .iter()
                .any(|(n, v)| n == "x-team" && v == "blue-team-secret")
        );
    }
    let dump = format!("{}{:?}", serde_json::to_string(&report).unwrap(), input);
    assert!(!dump.contains("blue-team-secret") && !dump.contains(KEY));
}

#[tokio::test]
async fn path_tokens_and_queries_never_reach_evidence_or_events() {
    let (_, input, outcome) = custom_scan().await;
    let report = outcome.result.unwrap();
    let redacted = format!(
        "{}{}{}{:?}",
        serde_json::to_string(&report.evidence).unwrap(),
        serde_json::to_string(&outcome.events).unwrap(),
        report.models_curl,
        input,
    );
    assert!(!redacted.contains(PATH_TOKEN));
    assert!(
        report
            .endpoints
            .iter()
            .all(|ep| !ep.full_request_url_preview.contains(PATH_TOKEN))
    );
    assert!(report.evidence.iter().all(|item| !item.url.contains('?')));
}

#[tokio::test]
async fn no_answer_from_any_request_is_a_transport_error() {
    let dns = || vec![Reply::Error(SafeTransportError::Dns)];
    let http = ScriptedHttp::new()
        .route(Get, "/api/status", dns())
        .route(Get, "/v1/models", dns())
        .route(Post, "/v1/chat/completions", dns());
    assert!(matches!(
        run(&http, chat_only("https://relay.acme.dev")).await.result,
        Err(SniffError::Transport { error_kind, .. }) if error_kind == "dns"
    ));
}

#[tokio::test]
async fn pre_cancelled_scan_sends_nothing() {
    let cancel = CancelToken::new();
    cancel.cancel();
    let fresh = relay();
    let outcome = run_limited(
        &fresh,
        keyed("https://relay.acme.dev"),
        None,
        false,
        Limits::PRODUCTION,
        &cancel,
    )
    .await;
    assert!(matches!(outcome.result, Err(SniffError::Cancelled)) && fresh.count() == 0);
}

#[tokio::test]
async fn total_cap_returns_a_partial_report() {
    let slow = ScriptedHttp::new()
        .route(
            Get,
            "/api/status",
            vec![Reply::json(200, &json!({"success": true, "data": {"quota_per_unit": 500000, "system_name": "Slow Relay", "usd_exchange_rate": 1}}))],
        )
        .route(Get, "/v1/models", vec![Reply::json(200, &models()).with_delay(Duration::from_secs(60))]);
    let limits = Limits {
        total: Duration::from_millis(120),
        probe: Duration::from_secs(30),
        models: Duration::from_secs(30),
        ..Limits::PRODUCTION
    };
    let report = run_limited(
        &slow,
        keyed("https://relay.acme.dev"),
        None,
        false,
        limits,
        &CancelToken::new(),
    )
    .await
    .result
    .unwrap();
    assert!(report.warnings.contains(&SniffWarning::ScanTimeout));
    assert_eq!(report.family, GatewayFamily::NewApi);
}

#[tokio::test]
async fn a_stop_after_an_answer_returns_the_partial_report_like_the_time_limit() {
    let slow = ScriptedHttp::new()
        .route(
            Get,
            "/api/status",
            vec![Reply::json(200, &json!({"success": true, "data": {"quota_per_unit": 500000, "system_name": "Slow Relay", "usd_exchange_rate": 1}}))],
        )
        .route(Get, "/v1/models", vec![Reply::json(200, &models()).with_delay(Duration::from_secs(60))]);
    let cancel = CancelToken::new();
    let stopper = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(120)).await;
        stopper.cancel();
    });
    let report = run_limited(
        &slow,
        keyed("https://relay.acme.dev"),
        None,
        false,
        Limits::PRODUCTION,
        &cancel,
    )
    .await
    .result
    .unwrap();
    assert!(report.warnings.contains(&SniffWarning::ScanCancelled));
    assert!(!report.warnings.contains(&SniffWarning::ScanTimeout));
    assert_eq!(report.family, GatewayFamily::NewApi);
}

#[tokio::test]
async fn billable_verification_upgrades_existing_routes_to_works() {
    let http = ScriptedHttp::new()
        .route(Get, "/v1/models", vec![Reply::json(200, &models())])
        .route(
            Post,
            "/v1/chat/completions",
            vec![
                bad_request(),
                Reply::json(200, &json!({"choices": [], "usage": {"total_tokens": 9}})),
            ],
        )
        .route(
            Post,
            "/v1/embeddings",
            vec![
                bad_request(),
                Reply::json(200, &json!({"data": [], "usage": {"total_tokens": 1}})),
            ],
        );
    let mut input = keyed("https://relay.acme.dev");
    input.allow_billable_probe = true;
    let report = run(&http, input).await.result.unwrap();
    assert_eq!(report.tokens_spent, 10);
    let works: Vec<Protocol> = report
        .endpoints
        .iter()
        .filter(|e| e.route == RouteVerdict::Works)
        .map(|e| e.protocol)
        .collect();
    assert_eq!(works, [Protocol::OpenaiChat, Protocol::Embeddings]);
}

#[tokio::test]
async fn extended_scan_tries_more_mounts_within_its_budget_of_twenty() {
    let mut ext = keyed("https://relay.acme.dev");
    ext.extended = true;
    let ext_report = run(&relay(), ext).await.result.unwrap();
    assert!(ext_report.requests_used > 8 && ext_report.requests_used <= 20);
}

#[tokio::test]
async fn keyless_scan_spends_its_budget_of_eight() {
    let keyless = run(&relay(), SniffInput::new("https://relay.acme.dev"))
        .await
        .result
        .unwrap();
    assert_eq!(
        (keyless.requests_used, keyless.key_status),
        (8, KeyCheckStatus::Untested)
    );
}

#[tokio::test]
async fn verification_requests_do_not_crowd_out_the_balance_step() {
    let new_api = || {
        ScriptedHttp::new()
            .route(
                Get,
                "/api/status",
                vec![
                    Reply::json(200, &json!({"success": true, "data": {"version": "0.9.7"}}))
                        .with_header("x-new-api-version", "0.9.7"),
                ],
            )
            .route(Get, "/v1/models", vec![Reply::json(200, &models())])
            .route(
                Post,
                "/v1/chat/completions",
                vec![
                    bad_request(),
                    Reply::json(200, &json!({"choices": [], "usage": {"total_tokens": 9}})),
                ],
            )
            .route(Post, "/v1/embeddings", vec![bad_request()])
            .route(
                Get,
                "/api/usage/token/",
                vec![Reply::json(
                    200,
                    &json!({"code": true, "data": {"total_available": 6_200_000, "unlimited_quota": false}}),
                )],
            )
    };
    let input = || keyed("https://relay.acme.dev");
    // The budget is exactly what the plain scan needs, its balance request included.
    let plain = run(&new_api(), input()).await.result.unwrap();
    assert!(matches!(plain.balance_finding, BalanceFinding::Found));
    let limits = Limits {
        budget_override: Some(plain.requests_used),
        ..Limits::scaled(100)
    };
    let mut billable = input();
    billable.allow_billable_probe = true;
    let report = run_limited(
        &new_api(),
        billable,
        None,
        false,
        limits,
        &CancelToken::new(),
    )
    .await
    .result
    .unwrap();
    assert!(report.verify_requests_used > 0);
    assert_eq!(
        report.requests_used,
        plain.requests_used + report.verify_requests_used
    );
    assert!(
        matches!(report.balance_finding, BalanceFinding::Found),
        "{:?}",
        report.balance_finding
    );
}

#[tokio::test]
async fn a_key_refused_by_the_other_region_suggests_that_region() {
    let denied = || {
        Reply::json(
            401,
            &json!({"error": {"message": "Invalid Authentication", "type": "invalid_authentication_error"}}),
        )
    };
    let moonshot = ScriptedHttp::new()
        .route(Get, "/v1/models", vec![denied()])
        .route(Post, "/v1/chat/completions", vec![denied()])
        .route(Post, "/anthropic/v1/messages", vec![denied()])
        .route(Post, "/v1/responses", vec![denied()]);
    let report = run(&moonshot, keyed("https://api.moonshot.cn"))
        .await
        .result
        .unwrap();
    assert!(report.warnings.contains(&SniffWarning::RegionMismatch {
        suggested_region_id: "global".to_owned()
    }));
}

/// A chat route that answers 429 twice.
async fn rate_limited_scan() -> Outcome {
    let limited = || Reply::json(429, &json!({"error": {"message": "slow down"}}));
    let http = ScriptedHttp::new()
        .route(Get, "/v1/models", vec![Reply::json(200, &models())])
        .route(Post, "/v1/chat/completions", vec![limited(), limited()]);
    run(&http, chat_only("https://relay.acme.dev")).await
}

#[tokio::test]
async fn simultaneous_429s_cause_one_waiting_event() {
    let outcome = rate_limited_scan().await;
    assert_eq!(
        outcome
            .events
            .iter()
            .filter(|e| matches!(e, SniffEvent::Waiting { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn a_429_that_persists_after_the_retry_is_warned() {
    let report = rate_limited_scan().await.result.unwrap();
    assert!(report.warnings.contains(&SniffWarning::RateLimited));
}

#[tokio::test]
async fn html_route_answer_stops_the_scan() {
    let html = ScriptedHttp::new()
        .route(Get, "/v1/models", vec![Reply::json(200, &models())])
        .route(
            Post,
            "/v1/chat/completions",
            vec![Reply::text(
                200,
                "text/html",
                "<html><body>hi</body></html>",
            )],
        );
    let report = run(&html, keyed("https://relay.acme.dev"))
        .await
        .result
        .unwrap();
    assert!(report.warnings.contains(&SniffWarning::HtmlPage));
}

/// A re-sniff of a stored relay through a proxy.
async fn resniff_scan() -> (ScriptedHttp, Outcome) {
    let stored = StoredProviderView {
        family: GatewayFamily::UnknownCompatible,
        brand_name: Some("relay.acme.dev".into()),
        balance_adapter: None,
        protocols: vec![Protocol::OpenaiChat, Protocol::AnthropicMessages],
        model_ids: vec!["gpt-4o".into(), "retired-model".into()],
    };
    let http = relay();
    let outcome = run_limited(
        &http,
        keyed("https://relay.acme.dev"),
        Some(&stored),
        true,
        Limits::scaled(100),
        &CancelToken::new(),
    )
    .await;
    (http, outcome)
}

#[tokio::test]
async fn a_resniff_reports_the_models_added_and_removed() {
    let (_, outcome) = resniff_scan().await;
    let diff = outcome.result.unwrap().diff.unwrap();
    assert_eq!(diff.models_added, ["gemini-3.2-pro", "text-embedding-3"]);
    assert_eq!(diff.models_removed, ["retired-model"]);
}

#[tokio::test]
async fn proxy_resolution_is_noted_on_one_evidence_row() {
    let (_, outcome) = resniff_scan().await;
    let report = outcome.result.unwrap();
    assert_eq!(
        report
            .evidence
            .iter()
            .filter(|i| i.note.ends_with("proxy_dns"))
            .count(),
        1
    );
}

#[tokio::test]
async fn keyless_identify_finishes_before_the_first_keyed_request() {
    let (http, _) = resniff_scan().await;
    let reqs = http.requests();
    let keyed_at = reqs
        .iter()
        .position(|r| {
            r.headers
                .iter()
                .any(|(n, _)| n == "authorization" || n == "x-api-key")
        })
        .unwrap();
    let id_end = reqs
        .iter()
        .rposition(|r| r.url.ends_with("/api/status") || r.url.ends_with("/api/v1/auth/me"))
        .unwrap();
    assert!(id_end < keyed_at);
}

#[tokio::test]
async fn phases_are_announced_in_order() {
    let (_, outcome) = resniff_scan().await;
    let phases: Vec<_> = outcome
        .events
        .iter()
        .filter_map(|e| match e {
            SniffEvent::Phase { phase } => Some(*phase),
            _ => None,
        })
        .collect();
    assert_eq!(
        phases,
        [
            SniffPhase::Identify,
            SniffPhase::Models,
            SniffPhase::Routes,
            SniffPhase::Key,
            SniffPhase::Balance
        ]
    );
}

#[tokio::test]
async fn model_ids_that_sanitise_alike_are_listed_once() {
    // Regression guard (deduplication used to run on the raw ids).
    let http = listing(json!({"data": [{"id": "a\u{7}b"}, {"id": "ab"}, {"id": "c"}]}));
    let report = run(&http, chat_only("https://relay.acme.dev"))
        .await
        .result
        .unwrap();
    let ids: Vec<&str> = report.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["ab", "c"]);
    assert_eq!(report.models_enabled + report.models_disabled, 2);
}

#[tokio::test]
async fn every_listed_model_string_is_sanitised() {
    // Regression guard (endpoint types, reasoning options and modalities were copied raw).
    let http = listing(json!({"data": [{
        "id": "m",
        "supported_endpoint_types": ["open\u{7}ai"],
        "capabilities": {"reasoning": {"allowed_options": ["lo\u{1b}w"]}},
        "architecture": {"input_modalities": ["te\u{0}xt", KEY]},
    }]}));
    let report = run(&http, chat_only("https://relay.acme.dev"))
        .await
        .result
        .unwrap();
    let model = &report.models[0];
    assert_eq!(model.declared_endpoint_types, ["openai"]);
    assert_eq!(model.reasoning_options, ["low"]);
    assert_eq!(model.modalities[0], "text");
    assert!(!serde_json::to_string(&report.models).unwrap().contains(KEY));
}

#[tokio::test]
async fn the_models_curl_line_hides_typed_secrets() {
    // Regression guard (only pattern-shaped secrets used to be hidden).
    let secret = "hunter2hunter2";
    let http = listing(models());
    let mut input = chat_only("https://relay.acme.dev");
    input.models_url = Some(format!("/v1/models?q={secret}"));
    input.extra_headers = vec![("X-Team".to_owned(), secret.to_owned())];
    let report = run(&http, input).await.result.unwrap();
    assert!(report.models_curl.contains("/v1/models?q="));
    assert!(
        !report.models_curl.contains(secret),
        "{}",
        report.models_curl
    );
}

#[tokio::test]
async fn a_401_on_the_model_list_tries_the_second_credential_variant() {
    let denied = Reply::json(401, &json!({"error": {"message": "bad key"}}));
    let http = ScriptedHttp::new()
        .route(Get, "/v1/models", vec![denied, Reply::json(200, &models())])
        .route(Post, "/v1/chat/completions", vec![bad_request()]);
    let report = run(&http, chat_only("https://relay.acme.dev"))
        .await
        .result
        .unwrap();
    let lists: Vec<_> = http
        .requests()
        .into_iter()
        .filter(|request| request.url.ends_with("/v1/models"))
        .collect();
    assert_eq!(lists.len(), 2);
    assert_eq!(
        header(&lists[0], "authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    assert_eq!(header(&lists[1], "authorization"), None);
    assert_eq!(header(&lists[1], "x-api-key"), Some(KEY));
    let ids: Vec<&str> = report.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["gpt-4o", "gemini-3.2-pro", "text-embedding-3"]);
}

#[tokio::test]
async fn a_paged_list_stops_after_the_page_limit() {
    let page = json!({"data": [{"id": "claude-x"}], "has_more": true, "last_id": "claude-x"});
    let http = ScriptedHttp::new()
        .route(Get, "/v1/models", vec![Reply::json(200, &page)])
        .route(Post, "/v1/messages", vec![bad_request()]);
    let mut input = keyed("https://relay.acme.dev/v1/messages");
    input.protocol_override = Some(Protocol::AnthropicMessages);
    let report = run(&http, input).await.result.unwrap();
    assert!(report.warnings.contains(&SniffWarning::ListTruncated));
    let pages = http
        .requests()
        .iter()
        .filter(|request| request.url.contains("/v1/models"))
        .count();
    assert_eq!(pages, MAX_PAGES);
}

#[tokio::test]
async fn an_unidentified_404_host_is_asked_for_sub2api_then_voapi_at_its_own_origin() {
    let token = "voapi-console-token-1234";
    let http = ScriptedHttp::new()
        .route(Get, "/v1/models", vec![Reply::json(200, &models())])
        .route(Post, "/v1/chat/completions", vec![bad_request()]);
    let mut input = chat_only("https://relay.acme.dev");
    input.management_token = Some(format!("  {token} "));
    run(&http, input).await.result.unwrap();
    let requests = http.requests();
    let paths: Vec<&str> = requests
        .iter()
        .map(|request| request.url.trim_start_matches("https://relay.acme.dev"))
        .collect();
    assert_eq!(
        paths[..3],
        ["/api/status", "/api/v1/auth/me", "/api/user/info"]
    );
    assert!(
        requests
            .iter()
            .all(|request| request.url.starts_with("https://relay.acme.dev/"))
    );
    let bearer = format!("Bearer {token}");
    let carrying: Vec<&str> = requests
        .iter()
        .filter(|request| {
            request
                .headers
                .iter()
                .any(|(_, value)| value.contains(token))
        })
        .map(|request| request.url.as_str())
        .collect();
    assert_eq!(carrying, ["https://relay.acme.dev/api/user/info"]);
    assert_eq!(header(&requests[2], "authorization"), Some(bearer.as_str()));
}

#[tokio::test]
async fn a_timed_out_scan_still_finishes_every_planned_row() {
    // Regression guard (the slow model-list row stayed "waiting" and the route row
    // "queued").
    let slow = ScriptedHttp::new().route(
        Get,
        "/v1/models",
        vec![Reply::json(200, &models()).with_delay(Duration::from_secs(60))],
    );
    let limits = Limits {
        total: Duration::from_millis(120),
        probe: Duration::from_secs(30),
        models: Duration::from_secs(30),
        ..Limits::PRODUCTION
    };
    let outcome = run_limited(
        &slow,
        chat_only("https://relay.acme.dev"),
        None,
        false,
        limits,
        &CancelToken::new(),
    )
    .await;
    let report = outcome.result.unwrap();
    assert!(report.warnings.contains(&SniffWarning::ScanTimeout));
    let mut planned = Vec::new();
    let mut finished = Vec::new();
    let mut timed_out = 0;
    for event in &outcome.events {
        match event {
            SniffEvent::Planned { rows } => planned.extend(rows.iter().map(|row| row.id)),
            SniffEvent::RequestFinished {
                id, status, note, ..
            } => {
                finished.push(*id);
                if note == "skipped (timeout)" {
                    assert_eq!(*status, None);
                    timed_out += 1;
                }
            }
            _ => {}
        }
    }
    planned.sort_unstable();
    finished.sort_unstable();
    assert_eq!(finished, planned, "one end per planned row");
    assert_eq!(
        timed_out, 2,
        "the model list in flight and the queued route"
    );
}

#[tokio::test]
async fn a_second_credential_variant_dropped_by_a_stop_keeps_the_first_401() {
    // Regression guard (the stop discarded the 401, so the key evidence had no
    // model-list status).
    let http = ScriptedHttp::new()
        .route(
            Get,
            "/models",
            vec![
                Reply::json(401, &json!({"error": {"message": "no"}}))
                    .with_delay(Duration::from_millis(30)),
            ],
        )
        .route(
            Post,
            "/anthropic/v1/messages",
            vec![Reply::text(200, "text/html", "<html></html>")],
        )
        .route(Post, "/chat/completions", vec![bad_request()]);
    let report = run(&http, keyed("https://api.deepseek.com"))
        .await
        .result
        .unwrap();
    assert!(report.warnings.contains(&SniffWarning::HtmlPage));
    assert_eq!(report.key_evidence.models_status, Some(401));
    let list_requests = http
        .requests()
        .iter()
        .filter(|request| request.url.ends_with("/models"))
        .count();
    assert_eq!(list_requests, 1, "the second variant was never sent");
}
