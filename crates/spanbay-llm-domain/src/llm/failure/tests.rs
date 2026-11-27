//! Tests of the failure rules: the classify and assess tables, key rests and circuit outcomes.

use super::text::SCAN_BYTES;
use super::*;

/// Texts without a failing status: the text alone decides the kind. Region rows pin the region
/// words against the 401/403 rules.
#[test]
fn classify_maps_status_and_message_to_failure_kind() {
    let openai_region = r#"{"error":{"code":"unsupported_country_region_territory","message":"Country, region, or territory not supported"}}"#;
    for (status, text, want) in [
        (Some(200), "", FailureKind::Unknown),
        (None, "", FailureKind::Unknown),
        (Some(403), openai_region, FailureKind::Region),
        (
            Some(401),
            "API key is bound to another region",
            FailureKind::Region,
        ),
        (
            Some(403),
            "Service is not available in your location",
            FailureKind::Region,
        ),
        (None, "too many requests per minute", FailureKind::RateLimit),
        (None, "Insufficient balance", FailureKind::Quota),
        (None, "no content returned", FailureKind::EmptyOutput),
        (None, "Invalid API key", FailureKind::Auth),
        (None, "model not found", FailureKind::ModelUnavailable),
        (None, "operation timed out", FailureKind::Timeout),
        (None, "Connection refused", FailureKind::Network),
        (
            None,
            "endpoint has no upstream path",
            FailureKind::ProtocolMismatch,
        ),
        (None, "upstream failed", FailureKind::ProviderError),
        (None, "overloaded_error", FailureKind::ProviderError),
    ] {
        assert_eq!(
            classify(status, text),
            want,
            "status={status:?} text={text:?}"
        );
    }
}

fn observed(status: Option<u16>, transport: Option<Transport>, body: &str) -> Failure {
    assess(&FailureObservation {
        status,
        transport,
        body: body.to_owned(),
        retry_after_ms: None,
    })
}

/// One row of the assess table: what was observed and the kind and scope it must give.
struct Case {
    status: Option<u16>,
    transport: Option<Transport>,
    body: &'static str,
    kind: FailureKind,
    scope: FailureScope,
}

const fn case(
    status: Option<u16>,
    transport: Option<Transport>,
    body: &'static str,
    kind: FailureKind,
    scope: FailureScope,
) -> Case {
    Case {
        status,
        transport,
        body,
        kind,
        scope,
    }
}

// One table pins kind and scope; quota codes win over the status, 5xx over text.
#[test]
fn assess_judges_kind_and_scope_in_a_fixed_order() {
    use FailureKind as K;
    use FailureScope as S;
    let cases = [
        case(
            Some(400),
            None,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"You have reached your specified API usage limits. You will regain access on 2026-12-01"}}"#,
            K::Quota,
            S::Key,
        ),
        case(
            Some(429),
            None,
            r#"{"error":{"code":"insufficient_quota"}}"#,
            K::Quota,
            S::Key,
        ),
        case(
            Some(429),
            None,
            r#"{"error":{"code":"organization_spend_limit_exceeded"}}"#,
            K::Quota,
            S::Key,
        ),
        case(
            Some(429),
            None,
            r#"{"error":{"code":"project_spend_limit_exceeded"}}"#,
            K::Quota,
            S::Key,
        ),
        case(
            Some(429),
            None,
            r#"{"error":{"code":"organization_usage_limit_exceeded"}}"#,
            K::Quota,
            S::Key,
        ),
        case(
            Some(400),
            None,
            "enforced_spend_limit_reached",
            K::Quota,
            S::Key,
        ),
        case(
            Some(429),
            None,
            "credit_balance_exhausted",
            K::Quota,
            S::Key,
        ),
        case(
            Some(429),
            None,
            r#"{"error":{"code":"slow_down"}}"#,
            K::RateLimit,
            S::Key,
        ),
        case(
            Some(400),
            None,
            r#"{"error":{"message":"`top_p` is deprecated for this model"}}"#,
            K::BadRequest,
            S::Request,
        ),
        case(
            Some(400),
            None,
            "prompt is too long",
            K::ContextLimit,
            S::Model,
        ),
        case(
            Some(502),
            None,
            "connection reset by peer",
            K::ProviderError,
            S::Target,
        ),
        case(Some(503), None, "", K::ProviderError, S::Target),
        case(Some(504), None, "", K::Timeout, S::Target),
        case(
            Some(403),
            None,
            "You do not have access to model gpt-9",
            K::ModelUnavailable,
            S::Model,
        ),
        case(Some(403), None, "", K::Auth, S::Key),
        case(Some(402), None, "", K::Quota, S::Key),
        case(
            Some(404),
            None,
            r#"{"error":{"code":"model_not_found"}}"#,
            K::ModelUnavailable,
            S::Model,
        ),
        case(Some(404), None, "no route", K::ModelUnavailable, S::Request),
        case(Some(422), None, "bad field", K::BadRequest, S::Request),
        case(
            Some(400),
            None,
            "content_policy_violation",
            K::PolicyBlocked,
            S::Request,
        ),
        case(Some(401), None, "bad", K::Auth, S::Key),
        case(Some(402), None, "model not activated", K::Quota, S::Key),
        case(Some(404), None, "", K::ModelUnavailable, S::Request),
        case(Some(408), None, "", K::Timeout, S::Target),
        case(Some(429), None, "slow down", K::RateLimit, S::Key),
        case(Some(500), None, "oops", K::ProviderError, S::Target),
        case(Some(418), None, "", K::BadRequest, S::Request),
        case(Some(401), None, "Incorrect API key", K::Auth, S::Key),
        case(Some(500), None, "region down", K::ProviderError, S::Target),
        case(
            Some(502),
            None,
            "upstream connection reset",
            K::ProviderError,
            S::Target,
        ),
        case(
            Some(403),
            None,
            "blocked by safety system",
            K::PolicyBlocked,
            S::Request,
        ),
        case(
            Some(429),
            None,
            "Safety filter triggered",
            K::PolicyBlocked,
            S::Request,
        ),
        case(
            Some(429),
            None,
            "You exceeded your current quota",
            K::Quota,
            S::Key,
        ),
        case(Some(429), None, "Rate limit reached", K::RateLimit, S::Key),
        case(Some(404), None, "timeout", K::ModelUnavailable, S::Request),
        case(Some(401), None, "rate limit", K::Auth, S::Key),
        case(Some(500), None, "cancelled", K::ProviderError, S::Target),
        // A status that is not a failure: the text decides the kind, the request is the scope.
        case(
            Some(200),
            None,
            "rate limit exceeded",
            K::RateLimit,
            S::Request,
        ),
        case(
            None,
            Some(Transport::Local),
            "error sending request: dns error: failed to lookup address information",
            K::Network,
            S::Local,
        ),
        case(
            None,
            Some(Transport::Connection),
            "connection refused",
            K::Network,
            S::Target,
        ),
        case(None, Some(Transport::Timeout), "", K::Timeout, S::Target),
        case(
            None,
            Some(Transport::Truncated),
            "",
            K::Truncated,
            S::Target,
        ),
        case(
            None,
            Some(Transport::StreamError),
            "overloaded",
            K::ProviderError,
            S::Target,
        ),
        // An in-stream error nobody can read is still the provider's.
        case(
            None,
            Some(Transport::StreamError),
            "something odd happened",
            K::ProviderError,
            S::Target,
        ),
        case(
            None,
            Some(Transport::StreamError),
            "insufficient_quota",
            K::Quota,
            S::Key,
        ),
    ];
    for Case {
        status,
        transport,
        body,
        kind,
        scope,
    } in cases
    {
        let failure = observed(status, transport, body);
        assert_eq!(
            (failure.kind, failure.scope),
            (kind, scope),
            "{status:?} {transport:?} {body}"
        );
    }
}

#[test]
fn the_scope_decides_where_failover_goes() {
    use FailureScope as S;
    for (scope, verdict) in [
        (S::Key, Verdict::NextKey),
        (S::Model, Verdict::NextTarget),
        (S::Target, Verdict::NextTarget),
        (S::Local, Verdict::NextTarget),
        (S::Request, Verdict::Return),
    ] {
        let failure = Failure {
            kind: FailureKind::Unknown,
            scope,
            status: None,
            retry_after_ms: None,
            retried: false,
        };
        assert_eq!(failure.verdict(), verdict, "{scope:?}");
    }
}

#[test]
fn only_the_first_sixteen_kib_are_searched_and_never_cut_inside_a_character() {
    // A keyword past the scanned start is ignored.
    let late = format!("{}insufficient_quota", "x".repeat(SCAN_BYTES));
    assert_eq!(classify(Some(429), &late), FailureKind::RateLimit);
    // A two-byte character straddling the limit is dropped whole, without a panic.
    let straddling = format!("{}é rate limit", "x".repeat(SCAN_BYTES - 1));
    assert_eq!(scan_text(&straddling).len(), SCAN_BYTES - 1);
    assert_eq!(classify(None, &straddling), FailureKind::Unknown);
    // Just inside the limit, the keyword still counts.
    let early = format!("{}quota", "x".repeat(SCAN_BYTES - "quota".len()));
    assert_eq!(classify(Some(429), &early), FailureKind::Quota);
}

#[test]
fn rejected_credentials_rest_the_key_long_and_context_limits_never_charge_the_circuit() {
    for body in ["", "API key is bound to another region"] {
        let failure = observed(Some(401), None, body);
        assert!(matches!(
            failure.kind,
            FailureKind::Auth | FailureKind::Region
        ));
        assert_eq!(failure.key_rest(), Some(KeyRest::Rejected), "{body}");
    }
    let context = observed(Some(400), None, "prompt is too long");
    assert_eq!(context.scope, FailureScope::Model);
    assert_eq!(context.health_outcome(false), Outcome::Success);
    assert_eq!(context.key_rest(), None);
}

#[test]
fn failures_only_charge_the_circuit_and_the_key_they_are_about() {
    let local = observed(None, Some(Transport::Local), "dns error");
    assert_eq!(local.health_outcome(false), Outcome::Success);
    let limited = observed(Some(429), None, "");
    assert_eq!(limited.health_outcome(true), Outcome::Success);
    assert_eq!(limited.health_outcome(false), Outcome::Failure);
    assert_eq!(limited.key_rest(), Some(KeyRest::Backoff));
    assert_eq!(
        observed(Some(500), None, "").health_outcome(true),
        Outcome::Failure
    );
    let model = observed(Some(403), None, "model gpt-9 is not available to you");
    assert_eq!(model.key_rest(), None);
    let quota = assess(&FailureObservation {
        status: Some(429),
        body: "insufficient_quota".to_owned(),
        retry_after_ms: Some(5_000),
        ..FailureObservation::default()
    });
    assert_eq!(quota.kind, FailureKind::Quota);
    assert_eq!(quota.key_rest(), Some(KeyRest::RetryAfter(5_000)));
}

#[test]
fn names_round_trip() {
    for kind in FailureKind::ALL {
        assert_eq!(FailureKind::parse(kind.as_str()), Some(kind));
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, format!("\"{}\"", kind.as_str()));
    }
    assert_eq!(FailureKind::parse("nope"), None);
}

#[test]
fn retryable_statuses() {
    for status in [408, 429, 500, 502, 599] {
        assert!(is_retryable_status(status));
    }
    for status in [200, 400, 401, 404, 600] {
        assert!(!is_retryable_status(status));
    }
}

/// Variants of [`FailureKind`]: bump it with the match below when one is added.
const KIND_COUNT: usize = 16;

/// The position of `kind` in [`FailureKind::ALL`]. No wildcard arm: a new variant does not
/// compile until it is placed here; once `KIND_COUNT` is bumped with it, the test below fails if
/// `ALL` misses it.
const fn index(kind: FailureKind) -> usize {
    match kind {
        FailureKind::Auth => 0,
        FailureKind::Region => 1,
        FailureKind::Quota => 2,
        FailureKind::RateLimit => 3,
        FailureKind::ModelUnavailable => 4,
        FailureKind::Timeout => 5,
        FailureKind::EmptyOutput => 6,
        FailureKind::ProtocolMismatch => 7,
        FailureKind::ProviderError => 8,
        FailureKind::Network => 9,
        FailureKind::PolicyBlocked => 10,
        FailureKind::Cancelled => 11,
        FailureKind::Unknown => 12,
        FailureKind::ContextLimit => 13,
        FailureKind::Truncated => 14,
        FailureKind::BadRequest => 15,
    }
}

#[test]
fn all_lists_every_kind_exactly_once() {
    assert_eq!(FailureKind::ALL.len(), KIND_COUNT);
    for (position, kind) in FailureKind::ALL.into_iter().enumerate() {
        assert_eq!(index(kind), position, "{kind:?}");
    }
}

// A 429 worded as a rate limit stays a rate limit even when it
// links the billing page; money words alone still make a 429 a quota failure.
#[test]
fn a_rate_limited_429_that_mentions_billing_backs_off_instead_of_resting_long() {
    let openai_free_tier = r#"{"error":{"message":"Rate limit reached for gpt-4o-mini in organization org-abc on requests per min (RPM): Limit 3, Used 3, Requested 1. Please try again in 20s. Visit https://platform.openai.com/account/rate-limits to learn more. You can increase your rate limit by adding a payment method to your account at https://platform.openai.com/account/billing.","type":"requests","param":null,"code":"rate_limit_exceeded"}}"#;
    let limited = observed(Some(429), None, openai_free_tier);
    assert_eq!(
        (limited.kind, limited.scope, limited.key_rest()),
        (
            FailureKind::RateLimit,
            FailureScope::Key,
            Some(KeyRest::Backoff)
        )
    );
    let billing = observed(
        Some(429),
        None,
        "Please check your plan and billing details",
    );
    assert_eq!(billing.kind, FailureKind::Quota);
    // A structured quota code wins over rate-limit wording.
    let coded = observed(
        Some(429),
        None,
        r#"{"error":{"message":"Too many requests","code":"insufficient_quota"}}"#,
    );
    assert_eq!(coded.kind, FailureKind::Quota);
}

/// The vendors of the provider presets answer in their own codes and in Chinese: every fixture
/// row must get the kind and scope it names.
#[test]
fn vendor_error_fixtures_get_their_kind_and_scope() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../fixtures/provider-errors/vendor-errors.json"
    ))
    .unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert!(cases.len() >= 20);
    for case in cases {
        let status = u16::try_from(case["status"].as_u64().unwrap()).unwrap();
        let body = case["body"].as_str().unwrap();
        let failure = observed(Some(status), None, body);
        let want_kind = FailureKind::parse(case["kind"].as_str().unwrap()).unwrap();
        let want_scope: FailureScope = serde_json::from_value(case["scope"].clone()).unwrap();
        assert_eq!(
            (failure.kind, failure.scope),
            (want_kind, want_scope),
            "{} {status}: {body}",
            case["vendor"]
        );
    }
}
