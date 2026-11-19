//! Replay eligibility and the API-client draft.

use super::{GATEWAY, SK_KEY, bounded, captured, facts, raw};
use crate::llm::capture::{
    ApiClientTarget, CapturedBody, open_in_api_client, redact_exchange, replay_eligibility,
};
use crate::llm::wire::Protocol;

fn body(text: &str, truncated: bool, elided: bool) -> Option<CapturedBody> {
    Some(CapturedBody {
        text: Some(text.to_owned()),
        truncated,
        elided,
        original_bytes: text.len() as u64,
    })
}

#[test]
fn a_clean_capture_can_be_replayed() {
    assert_eq!(replay_eligibility(Some(&captured()), false), Ok(()));
}

#[test]
fn no_capture_or_no_body_blocks_replay() {
    let block = replay_eligibility(None, false).unwrap_err();
    assert_eq!(block.reason, "No request body was stored");
    assert!(!block.needs_confirmation);
    let mut stored = captured();
    stored.client_req_body = None;
    assert!(replay_eligibility(Some(&stored), true).is_err());
}

#[test]
fn a_truncated_body_is_never_replayable_even_when_confirmed() {
    let mut stored = captured();
    stored.client_req_body = body("{\"a\":", true, false);
    let block = replay_eligibility(Some(&stored), true).unwrap_err();
    assert!(block.reason.contains("256 KiB"));
    assert!(!block.needs_confirmation);
}

#[test]
fn elided_media_names_how_many_images_were_not_stored() {
    let mut stored = captured();
    stored.client_req_body = body(
        r#"{"image":{"data":"<base64 214 KB elided>"}}"#,
        false,
        true,
    );
    let block = replay_eligibility(Some(&stored), true).unwrap_err();
    assert_eq!(block.reason, "This request had 1 image that was not stored");
    stored.client_req_body = body(
        r#"[{"image":{"data":"<base64 1 KB elided>"}},{"image":{"data":"<base64 9 KB elided>"}}]"#,
        false,
        true,
    );
    let block = replay_eligibility(Some(&stored), true).unwrap_err();
    assert_eq!(
        block.reason,
        "This request had 2 images that were not stored"
    );
    stored.client_req_body = body(r#"{"audio":{"data":"<base64 9 KB elided>"}}"#, false, true);
    let block = replay_eligibility(Some(&stored), false).unwrap_err();
    assert_eq!(
        block.reason,
        "This request had 1 media item that was not stored"
    );
}

#[test]
fn redacted_values_block_replay_until_the_user_confirms() {
    let mut exchange = raw();
    exchange.client_req_body = Some(bounded(&format!(
        r#"{{"model":"m","a":"{SK_KEY}","password":"hunter2-long"}}"#
    )));
    let stored = redact_exchange(exchange, &[]).0;
    let block = replay_eligibility(Some(&stored), false).unwrap_err();
    assert_eq!(
        block.reason,
        "The body contains 2 redacted values; replay would send the redacted text"
    );
    assert!(block.needs_confirmation);
    assert_eq!(replay_eligibility(Some(&stored), true), Ok(()));
}

#[test]
fn each_replay_condition_has_its_own_reason() {
    let mut reasons = vec![replay_eligibility(None, false).unwrap_err().reason];
    let mut stored = captured();
    stored.client_req_body = body("{", true, false);
    reasons.push(replay_eligibility(Some(&stored), false).unwrap_err().reason);
    stored.client_req_body = body(r#"{"image":"<base64 9 KB elided>"}"#, false, true);
    reasons.push(replay_eligibility(Some(&stored), false).unwrap_err().reason);
    stored.client_req_body = body(r#"{"password":"••••••"}"#, false, false);
    reasons.push(replay_eligibility(Some(&stored), false).unwrap_err().reason);
    reasons.sort();
    reasons.dedup();
    assert_eq!(reasons.len(), 4);
}

#[test]
fn the_draft_holds_the_post_translation_upstream_request() {
    let draft = open_in_api_client(&captured(), &facts(), GATEWAY);
    assert_eq!(draft.title, "POST /chat/completions · deepseek-v4-pro");
    assert_eq!(
        draft.target,
        ApiClientTarget::Provider {
            provider_id: "p1".to_owned(),
            model: "deepseek-v4-pro".to_owned()
        }
    );
    assert_eq!(draft.protocol, Protocol::OpenaiChat);
    assert!(draft.body.contains(r#""model":"deepseek-v4-pro""#));
    assert!(draft.notes.is_empty());
}

#[test]
fn a_deleted_provider_opens_in_custom_url_mode_with_the_host() {
    let mut row = facts();
    row.provider_exists = false;
    let draft = open_in_api_client(&captured(), &row, GATEWAY);
    assert_eq!(
        draft.target,
        ApiClientTarget::Custom {
            url: "https://api.deepseek.com/chat/completions".to_owned()
        }
    );
}

#[test]
fn without_an_upstream_request_the_draft_uses_the_client_request_through_the_gateway() {
    let mut stored = captured();
    stored.upstream_url.clear();
    stored.upstream_req_body = None;
    let draft = open_in_api_client(&stored, &facts(), GATEWAY);
    assert_eq!(draft.title, "POST /v1/messages · claude-code");
    assert_eq!(
        draft.target,
        ApiClientTarget::Custom {
            url: "http://127.0.0.1:4141/v1/messages".to_owned()
        }
    );
    assert_eq!(draft.protocol, Protocol::AnthropicMessages);
    assert!(draft.body.contains(r#""model":"claude-code""#));
}

#[test]
fn a_missing_body_opens_empty_with_a_note() {
    let mut stored = captured();
    stored.client_req_body = None;
    stored.upstream_req_body = None;
    stored.upstream_url.clear();
    let draft = open_in_api_client(&stored, &facts(), GATEWAY);
    assert_eq!(draft.body, "");
    assert_eq!(draft.notes, ["The body was not captured"]);
}

#[test]
fn cut_elided_and_redacted_bodies_open_with_warning_notes() {
    let mut stored = captured();
    stored.upstream_req_body = Some(CapturedBody {
        text: Some(r#"{"image":{"data":"<base64 9 KB elided>"},"password":"••••••"#.to_owned()),
        truncated: true,
        elided: true,
        original_bytes: 900_000,
    });
    let draft = open_in_api_client(&stored, &facts(), GATEWAY);
    assert_eq!(draft.notes.len(), 3, "{:?}", draft.notes);
    assert_eq!(draft.notes[0], "Body was cut at 256 KiB");
    assert!(draft.body.contains("elided>"));
}

#[test]
fn a_streaming_gemini_upstream_titles_the_stream_method() {
    let mut row = facts();
    row.protocol_out = Some(Protocol::Gemini);
    row.upstream_model = Some("gemini-2.5-pro".to_owned());
    row.stream = true;
    let draft = open_in_api_client(&captured(), &row, GATEWAY);
    assert_eq!(
        draft.title,
        "POST /models/gemini-2.5-pro:streamGenerateContent · gemini-2.5-pro"
    );
}
