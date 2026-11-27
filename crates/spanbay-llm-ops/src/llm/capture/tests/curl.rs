//! Copy as curl.

use super::{GATEWAY, PD_TOKEN, SK_KEY, bounded, captured, facts, pairs, raw};
use crate::llm::capture::{CapturedBody, CurlKind, build_curl, redact_exchange};
use crate::llm::wire::Protocol;

fn gateway_curl() -> crate::llm::capture::CurlOutput {
    build_curl(&captured(), &facts(), CurlKind::Gateway, GATEWAY).unwrap()
}

#[test]
fn gateway_curl_uses_the_token_placeholder_and_the_stored_body() {
    let out = gateway_curl();
    assert!(
        out.text
            .starts_with("curl -sS 'http://127.0.0.1:4141/v1/messages'"),
        "{}",
        out.text
    );
    assert!(
        out.text
            .contains(r#"-H "Authorization: Bearer $SPANBAY_TOKEN""#)
    );
    assert!(out.text.contains("-H 'content-type: application/json'"));
    assert!(out.text.contains("-H 'anthropic-version: 2023-06-01'"));
    assert!(
        out.text
            .contains("--data-binary @- <<'JSON'\n{\"model\":\"claude-code\"")
    );
    assert!(out.text.trim_end().ends_with("\nJSON"));
    assert!(out.warnings.is_empty());
}

#[test]
fn no_curl_text_holds_a_token_or_key_prefix() {
    for kind in [CurlKind::Gateway, CurlKind::Upstream] {
        let out = build_curl(&captured(), &facts(), kind, GATEWAY).unwrap();
        for forbidden in ["pd_", "sk-", PD_TOKEN, SK_KEY, "…"] {
            assert!(
                !out.text.contains(forbidden),
                "{kind:?} holds {forbidden}: {}",
                out.text
            );
        }
    }
}

#[test]
fn upstream_curl_uses_the_key_placeholder_and_skips_transfer_headers() {
    let out = build_curl(&captured(), &facts(), CurlKind::Upstream, GATEWAY).unwrap();
    assert!(
        out.text
            .starts_with("curl -sS \"https://api.deepseek.com/chat/completions\""),
        "{}",
        out.text
    );
    assert!(
        out.text
            .contains(r#"-H "Authorization: Bearer $PROVIDER_API_KEY""#)
    );
    assert!(out.text.contains("-H 'Content-Type: application/json'"));
    assert!(!out.text.to_lowercase().contains("content-length"));
    assert!(out.text.contains(r#""model":"deepseek-v4-pro""#));
    assert_eq!(
        out.text.matches("content-type").count() + out.text.matches("Content-Type").count(),
        1
    );
}

#[test]
fn x_api_key_and_goog_headers_get_the_placeholder_and_cookies_are_dropped_with_a_warning() {
    let mut exchange = raw();
    exchange.upstream_req_headers = pairs(&[
        ("x-api-key", SK_KEY),
        ("x-goog-api-key", "AIzaSyA-abcdefghijklmnopqrstuvwxyz01234"),
        ("anthropic-version", "2023-06-01"),
        ("Cookie", "session=abcdef123456"),
    ]);
    let stored = redact_exchange(exchange, &[]).0;
    let out = build_curl(&stored, &facts(), CurlKind::Upstream, GATEWAY).unwrap();
    assert!(
        out.text.contains(r#""x-api-key: $PROVIDER_API_KEY""#),
        "{}",
        out.text
    );
    assert!(out.text.contains(r#""x-goog-api-key: $PROVIDER_API_KEY""#));
    assert!(out.text.contains("'anthropic-version: 2023-06-01'"));
    assert!(!out.text.contains("Cookie") && !out.text.contains("abcdef123456"));
    assert_eq!(
        out.warnings,
        ["Header Cookie was removed (its value was redacted)"]
    );
    assert!(out.text.contains("content-type: application/json"));
}

#[test]
fn a_gemini_key_in_the_query_becomes_the_placeholder() {
    let mut exchange = raw();
    exchange.upstream_url = "https://u:p@generativelanguage.googleapis.com/v1beta/models/g:streamGenerateContent?alt=sse&key=AIzaSyA-abcdefghijklmnopqrstuvwxyz01234".to_owned();
    let stored = redact_exchange(exchange, &[]).0;
    let out = build_curl(&stored, &facts(), CurlKind::Upstream, GATEWAY).unwrap();
    assert!(
        out.text.contains("?alt=sse&key=$PROVIDER_API_KEY\""),
        "{}",
        out.text
    );
    assert!(!out.text.contains("AIz") && !out.text.contains("••••••"));
    // Regression guard (the stored URL's masked user info and key used to add "The URL contains
    // redacted text", though the command holds neither).
    assert_eq!(
        out.warnings,
        ["The URL's user info was removed; add credentials if the host needs them"]
    );
}

#[test]
fn the_redacted_url_warning_counts_the_url_the_command_sends() {
    let mut exchange = raw();
    exchange.upstream_url = "https://u:p@h.example/v1?key=0123456789abcdefXYZ&x=1".to_owned();
    let stored = redact_exchange(exchange, &[]).0;
    let out = build_curl(&stored, &facts(), CurlKind::Upstream, GATEWAY).unwrap();
    assert!(
        !out.warnings.iter().any(|w| w.contains("redacted text")),
        "{:?}",
        out.warnings
    );
    let mut exchange = raw();
    exchange.upstream_url = "https://u:p@h.example/v1?x=relay-9f8e7d6c5b4a".to_owned();
    let stored = redact_exchange(exchange, &["relay-9f8e7d6c5b4a".to_owned()]).0;
    let out = build_curl(&stored, &facts(), CurlKind::Upstream, GATEWAY).unwrap();
    assert!(out.text.contains("?x=••••••\""), "{}", out.text);
    assert_eq!(
        out.warnings,
        [
            "The URL contains redacted text",
            "The URL's user info was removed; add credentials if the host needs them",
        ]
    );
}

#[test]
fn every_protocol_has_its_gateway_path() {
    let cases = [
        (Protocol::OpenaiChat, false, "/v1/chat/completions"),
        (Protocol::OpenaiResponses, false, "/v1/responses"),
        (Protocol::AnthropicMessages, false, "/v1/messages"),
        (Protocol::Embeddings, false, "/v1/embeddings"),
        (Protocol::DecisionSystemone, false, "/v1/systemone"),
        (Protocol::DecisionOpenai, false, "/v1/decisions"),
        (
            Protocol::Gemini,
            false,
            "/v1beta/models/claude-code:generateContent",
        ),
        (
            Protocol::Gemini,
            true,
            "/v1beta/models/claude-code:streamGenerateContent?alt=sse",
        ),
    ];
    for (protocol, stream, path) in cases {
        let mut stored = captured();
        stored.client_protocol = protocol;
        let mut row = facts();
        row.stream = stream;
        let out = build_curl(&stored, &row, CurlKind::Gateway, GATEWAY).unwrap();
        assert!(
            out.text
                .starts_with(&format!("curl -sS '{GATEWAY}{path}' \\\n")),
            "{protocol:?}: {}",
            out.text
        );
    }
}

#[test]
fn a_redacted_forwarded_client_header_is_left_out_of_the_gateway_command() {
    let mut exchange = raw();
    exchange.client_req_headers = pairs(&[
        ("anthropic-version", "2023-06-01"),
        ("anthropic-beta", SK_KEY),
    ]);
    let stored = redact_exchange(exchange, &[]).0;
    assert!(stored.client_req_headers[1].redacted);
    let out = build_curl(&stored, &facts(), CurlKind::Gateway, GATEWAY).unwrap();
    assert!(out.text.contains("'anthropic-version: 2023-06-01'"));
    assert!(!out.text.contains("anthropic-beta"), "{}", out.text);
}

#[test]
fn a_gemini_gateway_request_uses_the_model_path_and_the_stream_variant() {
    let mut stored = captured();
    stored.client_protocol = Protocol::Gemini;
    let mut row = facts();
    row.requested_model = "fast".to_owned();
    let plain = build_curl(&stored, &row, CurlKind::Gateway, "http://127.0.0.1:4141/").unwrap();
    assert!(
        plain
            .text
            .starts_with("curl -sS 'http://127.0.0.1:4141/v1beta/models/fast:generateContent'")
    );
    row.stream = true;
    let streamed = build_curl(&stored, &row, CurlKind::Gateway, GATEWAY).unwrap();
    assert!(
        streamed
            .text
            .contains("/v1beta/models/fast:streamGenerateContent?alt=sse'")
    );
}

#[test]
fn without_a_stored_body_curl_is_refused_with_the_spec_reason() {
    let mut stored = captured();
    stored.client_req_body = None;
    let refusal = build_curl(&stored, &facts(), CurlKind::Gateway, GATEWAY).unwrap_err();
    assert_eq!(refusal.reason, "No request body was stored");
    stored.client_req_body = Some(CapturedBody::default());
    assert!(build_curl(&stored, &facts(), CurlKind::Gateway, GATEWAY).is_err());
}

#[test]
fn an_elided_body_is_refused() {
    let mut stored = captured();
    stored.client_req_body = Some(CapturedBody {
        text: Some(r#"{"data":"<base64 214 KB elided>"}"#.to_owned()),
        truncated: false,
        elided: true,
        original_bytes: 300_000,
    });
    let refusal = build_curl(&stored, &facts(), CurlKind::Gateway, GATEWAY).unwrap_err();
    assert!(refusal.reason.contains("elided"));
}

#[test]
fn a_truncated_body_is_copyable_with_a_visible_warning() {
    let mut stored = captured();
    stored.client_req_body = Some(CapturedBody {
        text: Some(r#"{"messages":[{"content":"abc"#.to_owned()),
        truncated: true,
        elided: false,
        original_bytes: 900_000,
    });
    let out = build_curl(&stored, &facts(), CurlKind::Gateway, GATEWAY).unwrap();
    assert_eq!(out.warnings, ["Body was cut at 256 KiB"]);
    assert!(out.text.contains(r#"{"messages":[{"content":"abc"#));
}

#[test]
fn redacted_values_in_the_body_are_announced() {
    let mut exchange = raw();
    exchange.client_req_body = Some(bounded(&format!(r#"{{"model":"m","key":"{SK_KEY}"}}"#)));
    let stored = redact_exchange(exchange, &[]).0;
    let out = build_curl(&stored, &facts(), CurlKind::Gateway, GATEWAY).unwrap();
    assert!(
        out.warnings[0].starts_with("The body contains 1 redacted value;"),
        "{:?}",
        out.warnings
    );
    assert!(!out.text.contains("sk-"));
}

#[test]
fn upstream_curl_needs_an_upstream_request() {
    let mut stored = captured();
    stored.upstream_url.clear();
    let refusal = build_curl(&stored, &facts(), CurlKind::Upstream, GATEWAY).unwrap_err();
    assert_eq!(refusal.reason, "No upstream request was captured");
}

#[test]
fn an_unchanged_upstream_body_is_rebuilt_from_the_client_body_with_the_served_model() {
    let mut exchange = raw();
    exchange.client_protocol = Protocol::OpenaiChat;
    exchange.client_req_body = Some(bounded(
        r#"{"model":"fast","messages":[{"role":"user","content":"hi"}]}"#,
    ));
    exchange.upstream_req_body = None;
    let stored = redact_exchange(exchange, &[]).0;
    let out = build_curl(&stored, &facts(), CurlKind::Upstream, GATEWAY).unwrap();
    assert!(
        out.text.contains(r#""model":"deepseek-v4-pro""#),
        "{}",
        out.text
    );
    assert!(!out.text.contains("fast"));
}

#[test]
fn the_heredoc_delimiter_never_collides_with_a_body_line() {
    let mut exchange = raw();
    exchange.client_req_body = Some(bounded("{\n\"a\":\"x\"\n}\nJSON\nJSON_1\n"));
    let stored = redact_exchange(exchange, &[]).0;
    let out = build_curl(&stored, &facts(), CurlKind::Gateway, GATEWAY).unwrap();
    assert!(out.text.contains("<<'JSON_2'"), "{}", out.text);
    assert!(out.text.trim_end().ends_with("\nJSON_2"));
}

#[test]
fn the_gateway_url_is_shell_quoted() {
    let out = build_curl(&captured(), &facts(), CurlKind::Gateway, "http://h'ost:1").unwrap();
    assert!(
        out.text.contains(r"'http://h'\''ost:1/v1/messages'"),
        "{}",
        out.text
    );
}

#[test]
fn upstream_curl_warns_about_an_elided_url_path_segment() {
    let mut exchange = captured();
    exchange.upstream_url = "https://relay.acme.dev/t/…/v1/chat/completions".to_owned();
    let curl = build_curl(&exchange, &facts(), CurlKind::Upstream, GATEWAY).unwrap();
    assert!(curl.text.contains("/t/…/v1/chat/completions"));
    assert!(
        curl.warnings
            .iter()
            .any(|w| w.contains("token-like segment")),
        "{:?}",
        curl.warnings
    );
}
