//! Tests of the failover rules: verdict fixtures, `Retry-After`, the attempt plan and timeouts.

use std::time::{Duration, UNIX_EPOCH};

use serde_json::json;

use super::*;
use crate::llm::health::Outcome;
use crate::llm::hub::{HubEffort, HubReasoning};

fn status(status: u16, body: &str) -> FailureObservation {
    FailureObservation {
        status: Some(status),
        body: body.to_owned(),
        ..FailureObservation::default()
    }
}

fn with_retry_after(status_code: u16, seconds: u64) -> FailureObservation {
    FailureObservation {
        retry_after_ms: Some(seconds * 1000),
        ..status(status_code, "")
    }
}

#[test]
fn body_fixtures_decide_model_and_context_failover() {
    use Verdict::{NextTarget as T, Return as R};
    let cases: &[(&str, u16, &str, Verdict)] = &[
        (
            "openai model_not_found",
            404,
            r#"{"error":{"message":"The model `gpt-9` does not exist or you do not have access to it.","type":"invalid_request_error","param":null,"code":"model_not_found"}}"#,
            T,
        ),
        (
            "anthropic not_found_error on model",
            404,
            r#"{"type":"error","error":{"type":"not_found_error","message":"model: claude-9-opus"}}"#,
            T,
        ),
        (
            "gemini models/x is not found",
            404,
            r#"{"error":{"code":404,"message":"models/gemini-9 is not found for API version v1beta, or is not supported for generateContent.","status":"NOT_FOUND"}}"#,
            T,
        ),
        (
            "deepseek model not exist",
            404,
            r#"{"error":{"message":"Model Not Exist","type":"invalid_request_error","param":null,"code":"invalid_request_error"}}"#,
            T,
        ),
        (
            "moonshot not found the model",
            404,
            r#"{"error":{"message":"Not found the model kimi-x or Permission denied","type":"resource_not_found_error"}}"#,
            T,
        ),
        (
            "openrouter no endpoints found",
            404,
            r#"{"error":{"message":"No endpoints found for acme/foo-9b.","code":404}}"#,
            T,
        ),
        (
            "relay unknown model",
            404,
            r#"{"error":{"message":"unknown model: glm-9","type":"new_api_error"}}"#,
            T,
        ),
        (
            "404 about a missing route is not a model error",
            404,
            r#"{"error":{"message":"Invalid URL (POST /v1/chat/completionz)","type":"invalid_request_error"}}"#,
            R,
        ),
        ("404 plain page", 404, "404 page not found", R),
        (
            "404 mentioning only the models path",
            404,
            r#"{"detail":"Not Found: /v1/models/foo"}"#,
            R,
        ),
        ("404 empty body", 404, "", R),
        (
            "openai context_length_exceeded",
            400,
            r#"{"error":{"message":"This model's maximum context length is 8192 tokens. However, your messages resulted in 9001 tokens.","type":"invalid_request_error","param":"messages","code":"context_length_exceeded"}}"#,
            T,
        ),
        (
            "openai string_above_max_length",
            400,
            r#"{"error":{"message":"Invalid 'messages[0].content': string too long.","type":"invalid_request_error","code":"string_above_max_length"}}"#,
            T,
        ),
        (
            "anthropic prompt is too long",
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 250000 tokens > 200000 maximum"}}"#,
            T,
        ),
        (
            "anthropic input and max_tokens exceed context limit",
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"input length and `max_tokens` exceed context limit: 190000 + 21333 > 200000, decrease input length or `max_tokens` and try again"}}"#,
            T,
        ),
        (
            "anthropic request_too_large 413",
            413,
            r#"{"type":"error","error":{"type":"request_too_large","message":"Request exceeds the maximum allowed number of bytes."}}"#,
            T,
        ),
        (
            "gemini exceeds the maximum number of tokens",
            400,
            r#"{"error":{"code":400,"message":"The input token count (1200000) exceeds the maximum number of tokens allowed (1048575).","status":"INVALID_ARGUMENT"}}"#,
            T,
        ),
        (
            "deepseek maximum context length",
            400,
            r#"{"error":{"message":"This model's maximum context length is 65536 tokens. However, you requested 70000 tokens (60000 in the messages, 10000 in the completion).","type":"invalid_request_error"}}"#,
            T,
        ),
        (
            "openrouter endpoint context length",
            400,
            r#"{"error":{"message":"This endpoint's maximum context length is 32768 tokens. However, you requested about 40000 tokens.","code":400}}"#,
            T,
        ),
        (
            "moonshot exceeded model token limit",
            400,
            r#"{"error":{"message":"Invalid request: Your request exceeded model token limit: 8192","type":"invalid_request_error"}}"#,
            T,
        ),
        (
            "generic 413 payload too large with a size limit",
            413,
            r#"{"error":{"message":"Request body exceeds the maximum size of 1048576 bytes"}}"#,
            T,
        ),
        (
            "400 malformed json",
            400,
            r#"{"error":{"message":"We could not parse the JSON body of your request.","type":"invalid_request_error"}}"#,
            R,
        ),
        (
            "400 malformed (not a context error)",
            400,
            r#"{"error":{"message":"malformed request: messages must be an array"}}"#,
            R,
        ),
        (
            "400 max_tokens above this model's output cap: another model may take it",
            400,
            r#"{"error":{"message":"max_tokens is too large: 99999. This model supports at most 4096 completion tokens, whereas you provided 99999.","type":"invalid_request_error","param":"max_tokens"}}"#,
            T,
        ),
        (
            "400 deepseek max_tokens range",
            400,
            r#"{"error":{"message":"Invalid max_tokens value, the valid range of max_tokens is [1, 8192]","type":"invalid_request_error"}}"#,
            T,
        ),
        (
            "400 temperature exceeds the maximum",
            400,
            r#"{"error":{"message":"temperature exceeds the maximum value of 2","type":"invalid_request_error"}}"#,
            R,
        ),
        (
            "400 model not exist is not a 404, so it is returned",
            400,
            r#"{"error":{"message":"Model Not Exist"}}"#,
            R,
        ),
        ("422 unprocessable", 422, r#"{"detail":"bad field"}"#, R),
        ("415 media type", 415, "unsupported media type", R),
        (
            "404 model words on a context status are not enough",
            404,
            r#"{"error":{"message":"maximum context length is 8192 tokens"}}"#,
            R,
        ),
    ];
    assert!(cases.len() >= 25);
    for (name, code, body, expected) in cases {
        assert_eq!(status(*code, body).assess().verdict(), *expected, "{name}");
    }
}

#[test]
fn status_table_gives_key_target_and_return_verdicts() {
    for code in [401, 402, 403, 429] {
        assert_eq!(
            status(code, "").assess().verdict(),
            Verdict::NextKey,
            "{code}"
        );
    }
    for code in [408, 500, 502, 503, 504, 529, 599] {
        assert_eq!(
            status(code, "").assess().verdict(),
            Verdict::NextTarget,
            "{code}"
        );
    }
    for code in [301, 302, 400, 404, 405, 409, 410, 413, 415, 422, 451] {
        assert_eq!(
            status(code, "").assess().verdict(),
            Verdict::Return,
            "{code}"
        );
    }
}

#[test]
fn transport_failures_move_to_the_next_target() {
    for transport in [
        Transport::Timeout,
        Transport::Connection,
        Transport::UnexpectedBody,
        Transport::StreamError,
    ] {
        let observation = FailureObservation {
            transport: Some(transport),
            ..FailureObservation::default()
        };
        assert_eq!(
            observation.assess().verdict(),
            Verdict::NextTarget,
            "{transport:?}"
        );
    }
}

#[test]
fn attempt_errors_map_to_observations() {
    let error = AttemptError::Status {
        status: 429,
        body: "slow down".to_owned(),
        retry_after_ms: Some(2000),
    };
    let observation = observe(&error).unwrap();
    assert_eq!(
        (observation.status, observation.retry_after_ms),
        (Some(429), Some(2000))
    );
    assert_eq!(
        observe(&AttemptError::Timeout).unwrap().transport,
        Some(Transport::Timeout)
    );
    assert_eq!(
        observe(&AttemptError::Network {
            message: "reset".into(),
            cause: Some(pingdeck_base::error::NetworkCause::Reset),
        })
        .unwrap()
        .transport,
        Some(Transport::Connection)
    );
    assert_eq!(
        observe(&AttemptError::Network {
            message: "tunnel error: proxy authorization required".into(),
            cause: Some(pingdeck_base::error::NetworkCause::ProxyAuth),
        })
        .unwrap()
        .transport,
        Some(Transport::Local)
    );
    assert_eq!(
        observe(&AttemptError::Stream {
            message: "overloaded".into(),
            kind: None
        })
        .unwrap()
        .transport,
        Some(Transport::StreamError)
    );
    let bad_body = observe(&AttemptError::BadBody("not JSON".into())).unwrap();
    assert_eq!(bad_body.transport, Some(Transport::UnexpectedBody));
    assert_eq!(bad_body.assess().verdict(), Verdict::NextTarget);
    assert_eq!(bad_body.health_outcome(false), Outcome::Failure);
    assert!(observe(&AttemptError::Cancelled).is_none());
    assert!(
        observe(&AttemptError::Invalid(
            pingdeck_base::error::AppError::validation("x")
        ))
        .is_none()
    );
}

#[test]
fn retry_after_header_parses_seconds_and_dates() {
    let now = UNIX_EPOCH + Duration::from_secs(784_111_777);
    assert_eq!(parse_retry_after("2", now), Some(2000));
    assert_eq!(parse_retry_after(" 0 ", now), Some(0));
    assert_eq!(parse_retry_after("1.5", now), Some(1500));
    assert_eq!(parse_retry_after("-3", now), None);
    assert_eq!(parse_retry_after("soon", now), None);
    assert_eq!(parse_retry_after("", now), None);
    assert_eq!(parse_retry_after("inf", now), None);
    // 784111777 is Sun, 06 Nov 1994 08:49:37 GMT.
    assert_eq!(
        parse_retry_after("Sun, 06 Nov 1994 08:49:40 GMT", now),
        Some(3000)
    );
    assert_eq!(
        parse_retry_after("Sun, 06 Nov 1994 08:00:00 GMT", now),
        Some(0)
    );
}

fn context(another_choice: bool) -> WaitContext {
    WaitContext {
        another_choice,
        waited: false,
        content_delivered: false,
        budget_left: true,
    }
}

#[test]
fn retry_after_waits_only_when_nothing_else_remains() {
    let sole = context(false);
    assert_eq!(
        RetryAfterPolicy::wait(&with_retry_after(429, 2), sole),
        Some(Duration::from_secs(2))
    );
    assert_eq!(
        RetryAfterPolicy::wait(&with_retry_after(503, 10), sole),
        Some(Duration::from_secs(10))
    );
    assert_eq!(
        RetryAfterPolicy::wait(&with_retry_after(429, 2), context(true)),
        None
    );
    assert_eq!(
        RetryAfterPolicy::wait(&with_retry_after(429, 11), sole),
        None
    );
    assert_eq!(
        RetryAfterPolicy::wait(&with_retry_after(429, 30), sole),
        None
    );
    for code in [500, 502, 504, 401, 402, 403, 408] {
        assert_eq!(
            RetryAfterPolicy::wait(&with_retry_after(code, 1), sole),
            None,
            "{code}"
        );
    }
    assert_eq!(RetryAfterPolicy::wait(&status(429, ""), sole), None);
    let mut blocked = sole;
    blocked.waited = true;
    assert_eq!(
        RetryAfterPolicy::wait(&with_retry_after(429, 1), blocked),
        None
    );
    blocked = sole;
    blocked.content_delivered = true;
    assert_eq!(
        RetryAfterPolicy::wait(&with_retry_after(429, 1), blocked),
        None
    );
    blocked = sole;
    blocked.budget_left = false;
    assert_eq!(
        RetryAfterPolicy::wait(&with_retry_after(429, 1), blocked),
        None
    );
}

#[test]
fn the_budget_is_one_plus_max_retries() {
    assert_eq!(attempt_budget(0), 1);
    assert_eq!(attempt_budget(2), 3);
    assert_eq!(attempt_budget(10), 11);
    assert_eq!(exhausted_status(Some(429)), 429);
    assert_eq!(exhausted_status(Some(200)), 502);
    assert_eq!(exhausted_status(None), 502);
}

fn attempt(group: usize, key: usize) -> Step {
    Step::Attempt {
        group,
        key,
        wait: None,
    }
}

// Acceptance case A: A answers 429 with Retry-After 2, B healthy: B serves, no wait.
#[test]
fn case_a_a_rate_limit_with_a_healthy_target_moves_on_without_waiting() {
    let mut plan = AttemptPlan::new(vec![1, 1], 2);
    assert_eq!(plan.step(), attempt(0, 0));
    assert_eq!(
        plan.failed(&with_retry_after(429, 2), false),
        Verdict::NextKey
    );
    assert_eq!(plan.step(), attempt(1, 0));
}

// Case B: only A: 429 retry-after 2, wait about 2 s, then success on the same key.
#[test]
fn case_b_the_only_target_waits_then_retries_the_same_key() {
    let mut plan = AttemptPlan::new(vec![1], 2);
    assert_eq!(plan.step(), attempt(0, 0));
    plan.failed(&with_retry_after(429, 2), false);
    assert_eq!(
        plan.step(),
        Step::Attempt {
            group: 0,
            key: 0,
            wait: Some(Duration::from_secs(2))
        }
    );
    assert_eq!(plan.attempts(), 1);
}

// Case C: only A: Retry-After 30 is not waited out: the 429 returns.
#[test]
fn case_c_a_long_retry_after_returns_the_429() {
    let mut plan = AttemptPlan::new(vec![1], 2);
    assert_eq!(plan.step(), attempt(0, 0));
    plan.failed(&with_retry_after(429, 30), false);
    assert_eq!(plan.step(), Step::Stop(StopReason::Exhausted));
    assert_eq!(exhausted_status(Some(429)), 429);
}

// Case D: a 404 that says the model was not found goes to B.
#[test]
fn case_d_model_not_found_goes_to_the_next_target() {
    let mut plan = AttemptPlan::new(vec![1, 1], 2);
    plan.step();
    let verdict = plan.failed(
        &status(
            404,
            r#"{"error":{"code":"model_not_found","message":"no"}}"#,
        ),
        false,
    );
    assert_eq!(verdict, Verdict::NextTarget);
    assert_eq!(plan.step(), attempt(1, 0));
}

// Case E: a 400 naming a context-length limit goes to B.
#[test]
fn case_e_context_length_goes_to_the_next_target() {
    let mut plan = AttemptPlan::new(vec![1, 1], 2);
    plan.step();
    plan.failed(
        &status(400, r#"{"error":{"code":"context_length_exceeded"}}"#),
        false,
    );
    assert_eq!(plan.step(), attempt(1, 0));
}

// Case F: any other 400 is returned after one attempt.
#[test]
fn case_f_another_client_error_returns_after_one_attempt() {
    let mut plan = AttemptPlan::new(vec![1, 1], 2);
    plan.step();
    assert_eq!(
        plan.failed(&status(400, "malformed"), false),
        Verdict::Return
    );
    assert_eq!(plan.step(), Step::Stop(StopReason::Returned));
    assert_eq!(plan.attempts(), 1);
}

// Case G: k1 503 goes to B without trying k2.
#[test]
fn case_g_a_server_error_skips_the_remaining_keys_of_the_provider() {
    let mut plan = AttemptPlan::new(vec![2, 1], 2);
    assert_eq!(plan.step(), attempt(0, 0));
    plan.failed(&status(503, ""), false);
    assert_eq!(plan.step(), attempt(1, 0));
}

#[test]
fn a_rejected_key_rotates_inside_the_provider_first() {
    let mut plan = AttemptPlan::new(vec![2, 1], 5);
    assert_eq!(plan.step(), attempt(0, 0));
    plan.failed(&status(429, ""), false);
    assert_eq!(plan.step(), attempt(0, 1));
    plan.failed(&status(402, ""), false);
    assert_eq!(plan.step(), attempt(1, 0));
}

#[test]
fn the_last_key_rejected_with_no_other_target_stops_exhausted() {
    let mut plan = AttemptPlan::new(vec![1], 5);
    plan.step();
    plan.failed(&status(401, ""), false);
    assert_eq!(plan.step(), Step::Stop(StopReason::Exhausted));
}

#[test]
fn a_rejected_last_key_falls_through_to_the_next_target() {
    let mut plan = AttemptPlan::new(vec![1, 1], 5);
    plan.step();
    plan.failed(&status(403, ""), false);
    assert_eq!(plan.step(), attempt(1, 0));
}

#[test]
fn the_budget_stops_after_one_plus_max_retries_failures() {
    let mut plan = AttemptPlan::new(vec![1, 1, 1, 1, 1], 2);
    for group in 0..3 {
        assert_eq!(plan.step(), attempt(group, 0));
        plan.failed(&status(500, ""), false);
    }
    assert_eq!(plan.step(), Step::Stop(StopReason::BudgetSpent));
    assert_eq!(plan.attempts(), 3);
}

#[test]
fn zero_retries_means_one_attempt() {
    let mut plan = AttemptPlan::new(vec![1, 1], 0);
    plan.step();
    plan.failed(&status(500, ""), false);
    assert_eq!(plan.step(), Step::Stop(StopReason::BudgetSpent));
}

#[test]
fn skipped_circuits_use_no_attempt() {
    let mut plan = AttemptPlan::new(vec![2, 1], 0);
    assert_eq!(plan.step(), attempt(0, 0));
    plan.skipped();
    assert_eq!(plan.step(), attempt(0, 1));
    plan.skipped();
    assert_eq!(plan.step(), attempt(1, 0));
    assert_eq!(plan.attempts(), 0);
    plan.failed(&status(500, ""), false);
    assert_eq!(plan.step(), Step::Stop(StopReason::BudgetSpent));
}

#[test]
fn everything_skipped_stops_exhausted_with_no_attempt() {
    let mut plan = AttemptPlan::new(vec![1, 1], 2);
    plan.step();
    plan.skipped();
    plan.step();
    plan.skipped();
    assert_eq!(plan.step(), Step::Stop(StopReason::Exhausted));
    assert_eq!(plan.attempts(), 0);
}

#[test]
fn the_wait_retry_uses_an_attempt_and_waits_only_once() {
    let mut plan = AttemptPlan::new(vec![1], 5);
    plan.step();
    plan.failed(&with_retry_after(503, 1), false);
    assert!(matches!(plan.step(), Step::Attempt { wait: Some(_), .. }));
    plan.failed(&with_retry_after(503, 1), false);
    assert_eq!(plan.step(), Step::Stop(StopReason::Exhausted));
    assert_eq!(plan.attempts(), 2);
}

#[test]
fn the_wait_never_goes_past_the_budget() {
    let mut plan = AttemptPlan::new(vec![1], 0);
    plan.step();
    plan.failed(&with_retry_after(429, 1), false);
    assert_eq!(plan.step(), Step::Stop(StopReason::BudgetSpent));
}

#[test]
fn a_wait_with_a_later_key_is_not_taken() {
    let mut plan = AttemptPlan::new(vec![2], 5);
    plan.step();
    plan.failed(&with_retry_after(429, 1), false);
    assert_eq!(plan.step(), attempt(0, 1));
}

#[test]
fn a_server_error_with_only_sibling_keys_left_waits_on_the_same_key() {
    // Other keys of the provider are not an alternative for a 503.
    let mut plan = AttemptPlan::new(vec![2], 5);
    plan.step();
    plan.failed(&with_retry_after(503, 2), false);
    assert_eq!(
        plan.step(),
        Step::Attempt {
            group: 0,
            key: 0,
            wait: Some(Duration::from_secs(2))
        }
    );
}

#[test]
fn content_already_delivered_ends_failover() {
    let mut plan = AttemptPlan::new(vec![1, 1], 5);
    plan.step();
    assert_eq!(plan.failed(&status(503, ""), true), Verdict::Return);
    assert_eq!(plan.step(), Step::Stop(StopReason::Returned));
}

#[test]
fn a_skipped_wait_retry_ends_the_request() {
    let mut plan = AttemptPlan::new(vec![1], 5);
    plan.step();
    plan.failed(&with_retry_after(429, 1), false);
    assert!(matches!(plan.step(), Step::Attempt { wait: Some(_), .. }));
    plan.skipped();
    assert_eq!(plan.step(), Step::Stop(StopReason::Exhausted));
}

// Key rotation leaves one attempt for every later group, so five rate-limited keys
// cannot use up the budget before the fallback.
#[test]
fn key_rotation_keeps_an_attempt_for_each_later_group() {
    let mut plan = AttemptPlan::new(vec![5, 1], 3);
    for key in 0..3 {
        assert_eq!(plan.step(), attempt(0, key));
        assert_eq!(plan.failed(&status(429, ""), false), Verdict::NextKey);
    }
    assert_eq!(plan.step(), attempt(1, 0));
    assert_eq!(plan.attempts(), 3);
}

// One jittered in-place retry of the last candidate for a transient error, never for a
// 504, never with another candidate left, never past the budget.
#[test]
fn the_last_candidate_retries_a_transient_error_once_in_place() {
    let wait = Duration::from_millis(400);
    let mut plan = AttemptPlan::new(vec![1], 2).with_retry_wait(wait);
    plan.step();
    assert!(!plan.retrying());
    plan.failed(&status(503, ""), false);
    assert_eq!(
        plan.step(),
        Step::Attempt {
            group: 0,
            key: 0,
            wait: Some(wait)
        }
    );
    assert!(plan.retrying());
    plan.failed(&status(502, ""), false);
    assert_eq!(plan.step(), Step::Stop(StopReason::Exhausted));

    let mut timeout = AttemptPlan::new(vec![1], 2);
    timeout.step();
    timeout.failed(&status(504, ""), false);
    assert_eq!(timeout.step(), Step::Stop(StopReason::Exhausted));

    let mut two = AttemptPlan::new(vec![1, 1], 2);
    two.step();
    two.failed(&status(503, ""), false);
    assert_eq!(two.step(), attempt(1, 0));
    assert!(!two.retrying());

    let mut last = AttemptPlan::new(vec![1], 0);
    last.step();
    last.failed(&status(503, ""), false);
    assert_eq!(last.step(), Step::Stop(StopReason::BudgetSpent));
}

#[test]
fn an_empty_plan_has_nothing_to_attempt() {
    let mut plan = AttemptPlan::new(Vec::new(), 2);
    assert_eq!(plan.step(), Step::Stop(StopReason::Exhausted));
    let mut plan = AttemptPlan::new(vec![0, 1], 2);
    assert_eq!(plan.step(), attempt(1, 0));
}

#[test]
fn reasoning_requests_get_at_least_120_seconds() {
    let setting = Duration::from_secs(30);
    let cases = [
        (
            Protocol::AnthropicMessages,
            json!({"thinking":{"type":"enabled","budget_tokens":2048}}),
            true,
        ),
        (
            Protocol::AnthropicMessages,
            json!({"thinking":{"type":"adaptive"}}),
            true,
        ),
        (
            Protocol::AnthropicMessages,
            json!({"thinking":{"type":"disabled"}}),
            false,
        ),
        (Protocol::AnthropicMessages, json!({"max_tokens":5}), false),
        (
            Protocol::OpenaiChat,
            json!({"reasoning_effort":"high"}),
            true,
        ),
        (
            Protocol::OpenaiChat,
            json!({"reasoning_effort":"none"}),
            false,
        ),
        (
            Protocol::OpenaiChat,
            json!({"reasoning_effort":null}),
            false,
        ),
        (Protocol::OpenaiChat, json!({"messages":[]}), false),
        (
            Protocol::OpenaiResponses,
            json!({"reasoning":{"effort":"medium"}}),
            true,
        ),
        (
            Protocol::OpenaiResponses,
            json!({"reasoning":{"summary":"auto"}}),
            true,
        ),
        (
            Protocol::OpenaiResponses,
            json!({"reasoning":{"effort":"none"}}),
            false,
        ),
        (Protocol::OpenaiResponses, json!({"reasoning":{}}), false),
        (
            Protocol::Gemini,
            json!({"generationConfig":{"thinkingConfig":{"thinkingBudget":1024}}}),
            true,
        ),
        (
            Protocol::Gemini,
            json!({"generationConfig":{"thinkingConfig":{"includeThoughts":true}}}),
            true,
        ),
        (
            Protocol::Gemini,
            json!({"generationConfig":{"thinkingConfig":{"thinkingBudget":0}}}),
            false,
        ),
        (
            Protocol::Gemini,
            json!({"generationConfig":{"temperature":1}}),
            false,
        ),
    ];
    for (protocol, body, reasoning) in cases {
        let expected = if reasoning {
            Duration::from_secs(120)
        } else {
            setting
        };
        assert_eq!(
            first_byte_timeout_raw(protocol, &body, setting),
            expected,
            "{protocol:?} {body}"
        );
    }
    // A longer setting is kept.
    assert_eq!(
        first_byte_timeout_raw(
            Protocol::OpenaiChat,
            &json!({"reasoning_effort":"low"}),
            Duration::from_secs(300)
        ),
        Duration::from_secs(300)
    );
}

#[test]
fn hub_requests_with_reasoning_get_at_least_120_seconds() {
    let setting = Duration::from_secs(30);
    let mut request = HubRequest::default();
    assert_eq!(first_byte_timeout(setting, &request), setting);
    for (reasoning, expected) in [
        (
            HubReasoning {
                effort: Some(HubEffort::Low),
                ..HubReasoning::default()
            },
            120,
        ),
        (
            HubReasoning {
                enabled: Some(true),
                ..HubReasoning::default()
            },
            120,
        ),
        (
            HubReasoning {
                budget_tokens: Some(1024),
                ..HubReasoning::default()
            },
            120,
        ),
        (
            HubReasoning {
                enabled: Some(false),
                effort: Some(HubEffort::High),
                ..HubReasoning::default()
            },
            30,
        ),
        (
            HubReasoning {
                budget_tokens: Some(0),
                ..HubReasoning::default()
            },
            30,
        ),
        (HubReasoning::default(), 30),
    ] {
        request.reasoning = Some(reasoning.clone());
        assert_eq!(
            first_byte_timeout(setting, &request),
            Duration::from_secs(expected),
            "{reasoning:?}"
        );
    }
}

// A failure of the key rests the key instead of opening the circuit; only with the key
// cooldown off does it count against the circuit.
#[test]
fn health_counts_target_failures_and_leaves_key_failures_to_the_cooldown() {
    let health = |observation: &FailureObservation, cooldown| observation.health_outcome(cooldown);
    for code in [408, 500, 503, 504, 529] {
        assert_eq!(health(&status(code, ""), true), Outcome::Failure, "{code}");
    }
    for code in [401, 402, 403, 429] {
        assert_eq!(health(&status(code, ""), true), Outcome::Success, "{code}");
        assert_eq!(health(&status(code, ""), false), Outcome::Failure, "{code}");
    }
    for code in [400, 404, 413, 422] {
        assert_eq!(
            health(&status(code, "context_length_exceeded"), true),
            Outcome::Success
        );
    }
    let transport = FailureObservation {
        transport: Some(Transport::Timeout),
        ..FailureObservation::default()
    };
    assert_eq!(health(&transport, false), Outcome::Failure);
}

#[test]
fn attempt_list_reasons_read_like_the_log_drawer() {
    let status_error = |status, retry_after_ms| AttemptError::Status {
        status,
        body: "{\"error\":{\"message\":\"slow\"}}".to_owned(),
        retry_after_ms,
    };
    assert_eq!(
        describe(&status_error(429, Some(2000))),
        ("failure", "429 retry-after 2".to_owned())
    );
    assert_eq!(
        describe(&status_error(503, Some(1500))),
        ("failure", "503 retry-after 2".to_owned())
    );
    assert_eq!(
        describe(&status_error(401, None)),
        ("rejected", "rejected 401".to_owned())
    );
    assert_eq!(
        describe(&status_error(403, Some(5000))),
        ("rejected", "rejected 403".to_owned())
    );
    assert_eq!(
        describe(&status_error(500, Some(5000))).1,
        "upstream returned HTTP 500: slow"
    );
    assert_eq!(
        describe(&AttemptError::Timeout),
        ("failure", "the upstream timed out".to_owned())
    );
}
