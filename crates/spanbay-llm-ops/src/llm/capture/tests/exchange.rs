//! Redaction of a whole exchange: every stored text, the counts, `bytes_total`, the body cut and
//! the row's own texts.

use serde_json::json;

use super::{PD_TOKEN, SK_KEY, bounded, captured, raw};
use crate::llm::capture::{
    BoundedBody, CapturedBody, CapturedExchange, CapturedFrame, CapturedFrames, RedactedHeader,
    redact_exchange, redact_row_texts,
};

#[test]
fn a_full_exchange_holds_no_secret_anywhere() {
    let mut exchange = raw();
    exchange.client_req_body = Some(bounded(&format!(
        r#"{{"model":"m","messages":[{{"role":"user","content":"my key is {SK_KEY} and relay-9f8e7d6c5b4a"}}],"api_key":"zzz-private-1"}}"#
    )));
    exchange.upstream_url = format!("https://h.example/v1?key={SK_KEY}");
    exchange.hub_request = Some(json!({"messages": [{"content": format!("token {PD_TOKEN}")}]}));
    exchange.client_frames = Some(CapturedFrames {
        frames: vec![CapturedFrame {
            index: 1,
            t_ms: 0,
            event: "message".to_owned(),
            data: format!(r#"{{"leak":"{SK_KEY}"}}"#),
        }],
        total_frames: 1,
        truncated: false,
    });
    let needles = vec!["relay-9f8e7d6c5b4a".to_owned()];
    let (stored, count) = redact_exchange(exchange, &needles);
    let text = serde_json::to_string(&stored).unwrap();
    for secret in [
        SK_KEY,
        PD_TOKEN,
        "relay-9f8e7d6c5b4a",
        "zzz-private-1",
        "abcdef123456",
    ] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    assert_eq!(count, stored.redacted_count);
    assert_eq!(count, 6);
}

/// Every stored text summed field by field: header names and values, URL, body texts, frame
/// event names and data, and the compact JSON of the hub request.
fn stored_text_bytes(stored: &CapturedExchange) -> u64 {
    let headers = |list: &[RedactedHeader]| -> usize {
        list.iter().map(|h| h.name.len() + h.value.len()).sum()
    };
    let body =
        |body: Option<&CapturedBody>| body.and_then(|b| b.text.as_deref()).map_or(0, str::len);
    let frames = |frames: Option<&CapturedFrames>| -> usize {
        frames.map_or(0, |f| {
            f.frames.iter().map(|f| f.event.len() + f.data.len()).sum()
        })
    };
    let total = headers(&stored.client_req_headers)
        + headers(&stored.upstream_req_headers)
        + headers(&stored.upstream_resp_headers)
        + headers(&stored.client_resp_headers)
        + stored.upstream_url.len()
        + body(stored.client_req_body.as_ref())
        + body(stored.upstream_req_body.as_ref())
        + body(stored.upstream_resp_body.as_ref())
        + body(stored.client_resp_body.as_ref())
        + frames(stored.upstream_frames.as_ref())
        + frames(stored.client_frames.as_ref())
        + stored
            .hub_request
            .as_ref()
            .map_or(0, |v| v.to_string().len());
    total as u64
}

#[test]
fn bytes_total_is_the_exact_sum_of_the_stored_text() {
    let stored = captured();
    assert_eq!(stored.bytes_total, 619);
    assert_eq!(stored.bytes_total, stored_text_bytes(&stored));
    let mut exchange = raw();
    exchange.hub_request = Some(json!({"messages": [{"content": "y".repeat(1000)}], "n": 1}));
    exchange.client_frames = Some(CapturedFrames {
        frames: vec![CapturedFrame {
            index: 1,
            t_ms: 0,
            event: "message".to_owned(),
            data: "{\"a\":1}".to_owned(),
        }],
        total_frames: 1,
        truncated: false,
    });
    let stored = redact_exchange(exchange, &[]).0;
    assert_eq!(stored.bytes_total, stored_text_bytes(&stored));
    assert!(stored.bytes_total > 619 + 1000);
}

#[test]
fn elision_markers_and_flags_survive_redaction() {
    let mut exchange = raw();
    exchange.client_req_body = Some(BoundedBody {
        text: r#"{"data":"<base64 214 KB elided>"}"#.to_owned(),
        truncated: false,
        elided: true,
        original_bytes: 220_000,
    });
    let body = redact_exchange(exchange, &[]).0.client_req_body.unwrap();
    assert!(body.elided);
    assert_eq!(body.original_bytes, 220_000);
    assert!(body.text.unwrap().contains("<base64 214 KB elided>"));
}

#[test]
fn a_body_cut_inside_a_key_does_not_keep_the_half_secret() {
    let mut exchange = raw();
    exchange.client_req_body = Some(BoundedBody {
        text: "hello sk-abcdefgh".to_owned(),
        truncated: true,
        elided: false,
        original_bytes: 900_000,
    });
    let body = redact_exchange(exchange, &[]).0.client_req_body.unwrap();
    assert_eq!(body.text.as_deref(), Some("hello "));
    assert!(body.truncated);
}

#[test]
fn a_cut_body_ending_in_a_long_token_loses_the_whole_token() {
    // Regression guard (a trailing token over 64 characters was kept whole).
    for len in [64, 65, 70, 2_000] {
        let tail: String = "0123456789".chars().cycle().take(len).collect();
        let mut exchange = raw();
        exchange.client_req_body = Some(BoundedBody {
            text: format!("hello {tail}"),
            truncated: true,
            elided: false,
            original_bytes: 900_000,
        });
        let body = redact_exchange(exchange, &[]).0.client_req_body.unwrap();
        assert_eq!(body.text.as_deref(), Some("hello "), "{len}");
        assert!(body.truncated);
    }
}

#[test]
fn frames_data_is_redacted_and_event_names_are_kept() {
    let mut exchange = raw();
    exchange.upstream_frames = Some(CapturedFrames {
        frames: vec![CapturedFrame {
            index: 1,
            t_ms: 5,
            event: "content_block_delta".to_owned(),
            data: format!(r#"{{"text":"{SK_KEY}","token":"abcdefgh12345678"}}"#),
        }],
        total_frames: 9,
        truncated: true,
    });
    let frames = redact_exchange(exchange, &[]).0.upstream_frames.unwrap();
    assert_eq!(frames.total_frames, 9);
    assert!(frames.truncated);
    assert_eq!(frames.frames[0].event, "content_block_delta");
    assert!(
        !frames.frames[0].data.contains("sk-") && !frames.frames[0].data.contains("abcdefgh1234")
    );
}

#[test]
fn the_hub_request_is_redacted_through_the_shared_json_rule_and_stays_valid_json() {
    let mut exchange = raw();
    exchange.hub_request =
        Some(json!({"user": "me", "apiKey": "zzz-private-1", "messages": [{"content": SK_KEY}]}));
    let hub = redact_exchange(exchange, &[]).0.hub_request.unwrap();
    let text = hub.to_string();
    assert!(!text.contains("zzz-private") && !text.contains("sk-"));
    assert_eq!(hub["user"], "me");

    let mut exchange = raw();
    exchange.hub_request =
        Some(json!({"messages": [{"content": format!("key {SK_KEY}")}], "n": 1}));
    let hub = redact_exchange(exchange, &[]).0.hub_request.unwrap();
    assert_eq!(hub["n"], 1);
    assert!(!hub.to_string().contains("sk-"));
}

#[test]
fn translation_notes_are_redacted_too() {
    let mut exchange = raw();
    exchange.translation_notes[0].reason = format!("dropped because of {SK_KEY}");
    let stored = redact_exchange(exchange, &[]).0;
    assert!(!stored.translation_notes[0].reason.contains("sk-"));
}

#[test]
fn row_texts_are_redacted_even_when_no_capture_is_stored() {
    let mut error = format!(
        "upstream 401: bad key {SK_KEY} at https://relay.acme.dev/t/AbCdEfGhIjKlMnOpQrStUvWx/v1"
    );
    let mut attempts = json!([
        {"provider": "p1", "status": 401, "error": "key relay-9f8e7d6c5b4a rejected"},
        {"provider": "p2", "status": 200, "url": "https://relay.acme.dev/t/AbCdEfGhIjKlMnOpQrStUvWx/v1", "token": "abc"}
    ]);
    let needles = vec!["relay-9f8e7d6c5b4a".to_owned()];
    let count = redact_row_texts(Some(&mut error), &mut attempts, &needles);
    let all = format!("{error} {attempts}");
    for secret in [SK_KEY, "relay-9f8e7d6c5b4a", "AbCdEfGhIjKl"] {
        assert!(!all.contains(secret), "{secret} survived: {all}");
    }
    assert_eq!(attempts[0]["status"], 401);
    assert!(count >= 4, "{count}");
    assert_eq!(redact_row_texts(None, &mut json!(null), &needles), 0);
}
