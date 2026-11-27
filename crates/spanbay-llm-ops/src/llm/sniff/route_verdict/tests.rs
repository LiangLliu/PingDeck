//! Route and key verdict tests.

use super::*;

fn response(status: u16, content_type: &str, body: &str) -> SafeResponse {
    SafeResponse {
        status,
        headers: vec![("content-type".into(), content_type.into())],
        body: body.as_bytes().to_vec(),
        ..SafeResponse::default()
    }
}

fn json(status: u16, body: &str) -> SafeResponse {
    response(status, "application/json", body)
}

/// [`classify_route`] on a response whose body is parsed here.
fn classify(protocol: Protocol, response: &SafeResponse, list_answered: bool) -> RouteVerdict {
    classify_route(protocol, response, response.json().as_ref(), list_answered)
}

#[test]
fn route_verdict_by_status_and_envelope() {
    use Protocol::{AnthropicMessages as M, DecisionSystemone as D, Gemini as G, OpenaiChat as C};
    use RouteVerdict::{Absent, Exists, Unclear};
    let openai_401 = r#"{"error":{"message":"Incorrect API key","type":"invalid_request_error","code":"invalid_api_key"}}"#;
    let anthropic_401 =
        r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
    let google_403 =
        r#"{"error":{"code":403,"message":"API key not valid","status":"PERMISSION_DENIED"}}"#;
    // The hosts a real scan meets: TypeSafe's own 401 and FastAPI 422, and a relay's.
    let typesafe_401 =
        r#"{"detail":{"error_type":"authentication_error","message":"Cannot authenticate"}}"#;
    let relay_401 = r#"{"code":"INVALID_API_KEY","message":"Invalid API key"}"#;
    let typesafe_422 =
        r#"{"detail":[{"type":"missing","loc":["body","model"],"msg":"Field required"}]}"#;
    let relay_400 = r#"{"error":{"message":"model must be a non-empty string","type":"invalid_request_error"},"type":"error"}"#;
    let rows: Vec<(Protocol, SafeResponse, bool, RouteVerdict)> = vec![
        (C, json(200, r#"{"choices":[]}"#), false, Exists),
        (
            C,
            json(
                400,
                r#"{"error":{"message":"you must provide a model parameter"}}"#,
            ),
            false,
            Exists,
        ),
        (C, json(422, r#"{"detail":[]}"#), false, Exists),
        (
            C,
            json(429, r#"{"error":{"message":"slow down"}}"#),
            false,
            Exists,
        ),
        (C, json(401, openai_401), false, Exists),
        (M, json(401, anthropic_401), false, Exists),
        (G, json(403, google_403), false, Exists),
        (C, json(401, r#"{"message":"nope"}"#), false, Unclear),
        (C, json(401, r#"{"message":"nope"}"#), true, Exists),
        (
            C,
            json(
                404,
                r#"{"error":{"message":"The model `x` does not exist"}}"#,
            ),
            false,
            Exists,
        ),
        (
            C,
            response(404, "text/plain", "404 page not found"),
            false,
            Absent,
        ),
        (C, json(405, "{}"), false, Absent),
        (
            C,
            json(503, r#"{"error":{"message":"overloaded"}}"#),
            false,
            Unclear,
        ),
        (
            C,
            response(200, "text/html", "<html></html>"),
            false,
            Absent,
        ),
        (D, json(401, typesafe_401), false, Exists),
        (D, json(401, relay_401), false, Exists),
        (D, json(422, typesafe_422), false, Exists),
        (D, json(400, relay_400), false, Exists),
    ];
    for (index, (protocol, response, list_answered, expected)) in rows.into_iter().enumerate() {
        assert_eq!(
            classify(protocol, &response, list_answered),
            expected,
            "row {index}"
        );
    }
    assert_eq!(classify(C, &json(501, "{}"), false), Absent);
    assert_eq!(
        classify(C, &response(200, "text/event-stream", "data: x"), false),
        Exists
    );
    assert_eq!(
        classify(C, &response(400, "text/plain", "Bad Request"), false),
        Absent
    );
    assert_eq!(
        classify(C, &response(302, "text/plain", ""), false),
        Unclear
    );
    // The envelope must be the protocol's own.
    assert_eq!(classify(M, &json(401, openai_401), false), Unclear);
    // A host's own structured error proves the route on every protocol.
    assert_eq!(classify(C, &json(401, relay_401), false), Exists);
    assert_eq!(classify(M, &json(401, typesafe_401), false), Exists);
    assert_eq!(classify(G, &json(403, relay_401), false), Exists);
}

fn verdict(
    has_key: bool,
    models: Option<u16>,
    routes: &[(Protocol, u16)],
    extra: &[(u16, &str)],
) -> KeyCheckStatus {
    let mut responses: Vec<(u16, &str)> = models.map(|status| (status, "")).into_iter().collect();
    responses.extend(routes.iter().map(|(_, status)| (*status, "")));
    responses.extend_from_slice(extra);
    key_verdict(
        KeyFacts {
            has_key,
            responses: &responses,
            models_status: models,
            auth_budget_gap: false,
            stopped: false,
            needs_confirmation: false,
        },
        routes.to_vec(),
    )
    .0
}

#[test]
fn key_verdict_table() {
    use KeyCheckStatus::*;
    let chat = Protocol::OpenaiChat;
    assert_eq!(verdict(false, Some(200), &[], &[]), Untested);
    assert_eq!(verdict(true, None, &[], &[]), Untested);
    assert_eq!(verdict(true, Some(200), &[(chat, 400)], &[]), Accepted);
    assert_eq!(
        verdict(true, Some(401), &[(chat, 400)], &[]),
        ModelsBlockedButChatWorks
    );
    assert_eq!(
        verdict(true, Some(403), &[(chat, 429)], &[]),
        ModelsBlockedButChatWorks
    );
    assert_eq!(verdict(true, Some(401), &[(chat, 401)], &[]), Rejected);
    assert_eq!(verdict(true, Some(404), &[(chat, 400)], &[]), Accepted);
    assert_eq!(
        verdict(true, Some(200), &[(chat, 402)], &[]),
        QuotaExhausted
    );
    assert_eq!(verdict(true, Some(401), &[], &[(401, "")]), Rejected);
    assert_eq!(
        verdict(
            true,
            Some(200),
            &[],
            &[(429, r#"{"error":{"message":"insufficient quota"}}"#)]
        ),
        QuotaExhausted
    );
    assert_eq!(verdict(true, Some(404), &[(chat, 404)], &[]), Untested);
}

#[test]
fn untested_reasons_name_why() {
    let reason = |models: Option<u16>, routes: &[(Protocol, u16)]| {
        let responses: Vec<(u16, &str)> = models
            .into_iter()
            .chain(routes.iter().map(|(_, status)| *status))
            .map(|status| (status, ""))
            .collect();
        key_verdict(
            KeyFacts {
                has_key: true,
                responses: &responses,
                models_status: models,
                auth_budget_gap: false,
                stopped: false,
                needs_confirmation: false,
            },
            routes.to_vec(),
        )
        .1
        .untested_reason
    };
    assert_eq!(reason(None, &[]).as_deref(), Some("unreachable"));
    assert_eq!(
        reason(Some(404), &[(Protocol::OpenaiChat, 404)]).as_deref(),
        Some("inconclusive")
    );
}

#[test]
fn scan_level_overrides_apply_in_order() {
    let chat = Protocol::OpenaiChat;
    let responses = [(401, ""), (401, "")];
    let run = |models_status: Option<u16>,
               responses: &[(u16, &str)],
               gap: bool,
               stopped: bool,
               confirm: bool| {
        let (status, evidence) = key_verdict(
            KeyFacts {
                has_key: true,
                responses,
                models_status,
                auth_budget_gap: gap,
                stopped,
                needs_confirmation: confirm,
            },
            vec![(chat, responses[responses.len() - 1].0)],
        );
        (status, evidence.untested_reason)
    };
    let budget = run(Some(401), &responses, true, false, false);
    assert_eq!(
        budget,
        (KeyCheckStatus::Untested, Some("budget".to_owned()))
    );
    // A stopped scan replaces the budget reason.
    let stopped = run(Some(401), &responses, true, true, false);
    assert_eq!(
        stopped,
        (KeyCheckStatus::Untested, Some("stopped".to_owned()))
    );
    // Without the gap a stopped scan keeps the rejection and adds no reason.
    assert_eq!(
        run(Some(401), &responses, false, true, false),
        (KeyCheckStatus::Rejected, None)
    );
    // A held-back key names its reason last, whatever the status.
    let accepted = [(200, ""), (400, "")];
    assert_eq!(
        run(Some(200), &accepted, false, false, true),
        (
            KeyCheckStatus::Accepted,
            Some("needs_confirmation".to_owned())
        )
    );
    assert_eq!(
        run(Some(401), &responses, true, true, true),
        (
            KeyCheckStatus::Untested,
            Some("needs_confirmation".to_owned())
        )
    );
}

#[test]
fn no_key_is_never_overridden_to_budget_or_stopped() {
    let (status, evidence) = key_verdict(
        KeyFacts {
            has_key: false,
            responses: &[],
            models_status: None,
            auth_budget_gap: true,
            stopped: true,
            needs_confirmation: false,
        },
        Vec::new(),
    );
    assert_eq!(status, KeyCheckStatus::Untested);
    assert_eq!(evidence.untested_reason.as_deref(), Some("no_key"));
}
