//! Shared fixtures and the cross-adapter suites of the balance module: selection, transport and
//! safety rules, the "no secret in any report" property, the custom query and the code adapters.

use serde_json::{Value, json};

use crate::llm::safe_http::SafeMethod;
use crate::llm::safe_http::scripted::{Reply, ScriptedHttp};
use crate::llm::wire::GatewayFamily;
use pingdeck_base::cancel::CancelToken;

use super::*;

mod adapters_gateway;
mod adapters_quota;
mod adapters_vendor;
mod custom_run;
mod custom_test_query;
mod custom_validation;
mod secrets;
mod selection;
mod transport;

/// The model key of every test target.
pub(crate) const KEY: &str = "sk-test-balance-key-0123456789abcdef";
/// The check time of every test.
pub(crate) const NOW: i64 = 1_794_052_800_000;

/// The stored endpoint origins of a test provider: just `origin` (leaked, tests only).
pub(crate) fn origins(origin: &str) -> &'static [String] {
    Box::leak(vec![origin.to_owned()].into_boxed_slice())
}

/// A target at `relay.acme.dev` with the `/v1` mount and a budget of four requests.
pub(crate) fn target(family: GatewayFamily) -> BalanceTarget<'static> {
    BalanceTarget {
        origin: "https://relay.acme.dev",
        endpoint_origins: origins("https://relay.acme.dev"),
        family,
        preset_id: None,
        api_key: KEY,
        management_token: None,
        api_user_id: None,
        quota_per_unit: None,
        display_in_currency: None,
        allow_private: false,
        custom: None,
        request_budget: 4,
    }
}

/// A target for a vendor preset on `origin`.
pub(crate) fn vendor(preset: &'static str, origin: &'static str) -> BalanceTarget<'static> {
    BalanceTarget {
        origin,
        endpoint_origins: origins(origin),
        preset_id: Some(preset),
        ..target(GatewayFamily::Official)
    }
}

/// Runs a check against `http`.
pub(crate) async fn check(http: &ScriptedHttp, target: &BalanceTarget<'_>) -> BalanceOutcome {
    query_balance(http, target, &CancelToken::new(), NOW).await
}

/// The report of a successful outcome.
pub(crate) fn report_of(outcome: BalanceOutcome) -> BalanceReport {
    reported(outcome).0
}

/// The error of a failed outcome.
pub(crate) fn error_of(outcome: BalanceOutcome) -> BalanceError {
    failed(outcome).0
}

/// The value of header `name` of the request number `index`.
pub(crate) fn header_of(http: &ScriptedHttp, index: usize, name: &str) -> Option<String> {
    http.requests()[index]
        .headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

/// A single `200` JSON answer.
pub(crate) fn ok(body: &Value) -> Vec<Reply> {
    vec![Reply::json(200, body)]
}

/// The console token of the gateway tests.
pub(super) const MGMT: &str = "mgmt-console-token-ABCDEF123456";

/// A scripted transport answering `GET path` with the replies of each row.
pub(super) fn routes(table: &[(&str, Vec<Reply>)]) -> ScriptedHttp {
    table
        .iter()
        .fold(ScriptedHttp::new(), |http, (path, replies)| {
            http.route(SafeMethod::Get, path, replies.clone())
        })
}

/// A plain-text answer with `code`.
pub(super) fn status(code: u16) -> Vec<Reply> {
    vec![Reply::text(code, "text/plain", "no")]
}

/// A plain-text 404.
pub(super) fn not_found() -> Vec<Reply> {
    status(404)
}

/// `text` with whitespace runs collapsed to single spaces (the shape of `Session::safe_text` for
/// short text).
pub(super) fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The report and the request count of a successful outcome.
pub(super) fn reported(outcome: BalanceOutcome) -> (BalanceReport, u32) {
    match outcome {
        BalanceOutcome::Report {
            report,
            requests_used,
        } => (report, requests_used),
        other => panic!("expected a report, got {other:?}"),
    }
}

/// The error and the request count of a failed outcome.
pub(super) fn failed(outcome: BalanceOutcome) -> (BalanceError, u32) {
    match outcome {
        BalanceOutcome::Failed {
            error,
            requests_used,
        } => (error, requests_used),
        other => panic!("expected a failure, got {other:?}"),
    }
}

/// `draft` as a sealed report of an adapter with `evidence`: supported, checked at [`NOW`], with
/// `raw` as its redacted body.
pub(super) fn sealed(evidence: Evidence, raw: Value, draft: BalanceReport) -> BalanceReport {
    BalanceReport {
        supported: true,
        evidence: Some(evidence),
        raw_json: raw,
        checked_at_ms: NOW,
        ..draft
    }
}

/// A balance error as the adapters build it.
pub(super) fn error(
    kind: BalanceErrorKind,
    message: &str,
    status: Option<u16>,
    endpoint: Option<&str>,
) -> BalanceError {
    BalanceError {
        kind,
        status,
        message: message.to_owned(),
        endpoint: endpoint.map(str::to_owned),
        retry_after_ms: None,
    }
}

/// Whether `text` has the shape `YYYY-MM-DD` (four, two and two ASCII digits).
pub(crate) fn is_iso_date(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    matches!(parts.as_slice(), [y, m, d] if y.len() == 4 && m.len() == 2 && d.len() == 2
        && parts.iter().all(|part| part.bytes().all(|b| b.is_ascii_digit())))
}

/// Plausible answers for every endpoint the gateway adapters may call.
pub(super) fn all_ok_http() -> ScriptedHttp {
    ScriptedHttp::new()
        .route(SafeMethod::Get, "/api/usage/token/", ok(&json!({"code": true, "data": {"total_available": 6200000, "unlimited_quota": false}})))
        .route(SafeMethod::Get, "/api/user/self", ok(&json!({"success": true, "data": {"quota": 6200000, "used_quota": 1000000}})))
        .route(SafeMethod::Get, "/api/status", ok(&json!({"data": {"quota_per_unit": 500000, "display_in_currency": true}})))
        .route(SafeMethod::Get, "/v1/dashboard/billing/subscription", ok(&json!({"hard_limit_usd": 50, "access_until": 0})))
        .route(SafeMethod::Get, "/v1/dashboard/billing/usage", ok(&json!({"total_usage": 3750})))
        .route(SafeMethod::Get, "/v1/usage", ok(&json!({"mode": "unrestricted", "remaining": 3.5, "balance": 3.5, "unit": "USD", "planName": "wallet"})))
        .route(SafeMethod::Get, "/key/info", ok(&json!({"info": {"spend": 1.5, "max_budget": 10}})))
        .route(SafeMethod::Get, "/v2/user/info", ok(&json!({"spend": 2.0, "max_budget": null})))
        .route(SafeMethod::Get, "/api/user/info", ok(&json!({"code": 0, "data": {"basicBalance": "3.00", "bindBalance": "0.50", "currency": "CNY"}})))
        .route(SafeMethod::Get, "/api/v1/credits", ok(&json!({"data": {"total_credits": 20, "total_usage": 7.6}})))
        .route(SafeMethod::Get, "/api/v1/key", ok(&json!({"data": {"limit": 5, "limit_remaining": 2.5}})))
}

/// A header value the custom-query tests resolve from the secret store.
pub(super) const SECRET_VALUE: &str = "super-secret-header-value-42";
/// The Zhipu account endpoint the custom-query tests call.
pub(super) const ZHIPU_URL: &str =
    "https://open.bigmodel.cn/api/biz/account/query-customer-account-report";
/// The path of [`ZHIPU_URL`].
pub(super) const ZHIPU_PATH: &str = "/api/biz/account/query-customer-account-report";

/// A valid custom query of `url` reading `$.data.availableBalance` for the account.
pub(super) fn query(url: &str, auth: BalanceAuth, unit: UnitName) -> CustomBalanceQuery {
    CustomBalanceQuery {
        url: url.to_owned(),
        headers: Vec::new(),
        auth,
        remaining_path: "$.data.availableBalance".to_owned(),
        used_path: None,
        total_path: None,
        unit,
        scope: Some(BalanceScope::Account),
        unit_label: None,
        divisor: None,
        cross_host: false,
        resolved_headers: ResolvedHeaders::default(),
    }
}

/// `base` with `custom` as its saved query.
pub(super) fn with_custom<'a>(
    base: BalanceTarget<'static>,
    custom: &'a CustomBalanceQuery,
) -> BalanceTarget<'a> {
    BalanceTarget {
        custom: Some(custom),
        ..base
    }
}

/// The Zhipu preset target with `custom` as its saved query.
pub(super) fn zhipu_with(custom: &CustomBalanceQuery) -> BalanceTarget<'_> {
    with_custom(vendor("zhipu", "https://open.bigmodel.cn"), custom)
}

/// A Zhipu account answer (with an email the redaction must remove).
pub(super) fn zhipu_body() -> Value {
    json!({"code": 200, "msg": "ok", "success": true,
        "data": {"availableBalance": 128.5, "balance": 128.5, "frozenBalance": 0, "email": "a@b.c"}})
}

/// Runs "Test query" and expects an answer.
pub(super) async fn run_test_query(
    http: &ScriptedHttp,
    target: &BalanceTarget<'_>,
    query: &CustomBalanceQuery,
    cancel: &CancelToken,
) -> CustomQueryTest {
    test_custom_query(http, target, query, cancel, NOW)
        .await
        .unwrap_or_else(|e| panic!("{e:?}"))
}
