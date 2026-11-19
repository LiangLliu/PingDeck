//! The bounded copy on the request path: caps, copy margin, UTF-8 cuts, base64 elision and the
//! frame recorder.

use crate::llm::capture::{
    BASE64_ELISION_THRESHOLD, BODY_COPY_MARGIN, FRAME_COPY_MARGIN_BYTES, FRAME_COPY_MARGIN_FRAMES,
    FrameRecorder, MAX_BODY_BYTES, MAX_FRAME_BYTES, MAX_FRAMES, bound_body,
};

fn b64(len: usize) -> String {
    "QUJD".repeat(len / 4)
}

#[test]
fn small_bodies_pass_through_unchanged() {
    let body = bound_body(br#"{"model":"m","messages":[]}"#);
    assert_eq!(body.text, r#"{"model":"m","messages":[]}"#);
    assert!(!body.truncated && !body.elided);
    assert_eq!(body.original_bytes, 27);
}

#[test]
fn a_body_over_the_copy_cap_is_cut_and_keeps_its_size() {
    let copy_cap = MAX_BODY_BYTES + BODY_COPY_MARGIN;
    let bytes = vec![b'a'; copy_cap + 10];
    let body = bound_body(&bytes);
    assert_eq!(body.text.len(), copy_cap);
    assert!(body.truncated);
    assert_eq!(body.original_bytes, (copy_cap + 10) as u64);
}

#[test]
fn a_body_within_the_margin_is_copied_whole_for_the_writer_to_cut() {
    let bytes = vec![b'a'; MAX_BODY_BYTES + 100];
    let body = bound_body(&bytes);
    assert_eq!(body.text.len(), MAX_BODY_BYTES + 100);
    assert!(!body.truncated);
}

#[test]
fn the_cut_never_splits_a_utf8_character() {
    // Euro sign is 3 bytes; start offset 1 makes the cap fall inside a character.
    let mut bytes = vec![b'a'];
    bytes.extend(
        "€"
            .repeat((MAX_BODY_BYTES + BODY_COPY_MARGIN) / 3 + 2)
            .as_bytes(),
    );
    let body = bound_body(&bytes);
    assert!(body.truncated);
    assert!(body.text.len() <= MAX_BODY_BYTES + BODY_COPY_MARGIN);
    assert!(!body.text.contains('\u{FFFD}'), "cut inside a character");
    assert!(body.text.ends_with('€'));
}

#[test]
fn a_body_of_exactly_the_cap_is_not_truncated() {
    let body = bound_body(&vec![b'x'; MAX_BODY_BYTES]);
    assert!(!body.truncated);
}

#[test]
fn a_data_url_payload_is_elided_with_its_size() {
    let text = format!(
        r#"{{"image_url":{{"url":"data:image/png;base64,{}"}}}}"#,
        b64(220 * 1024)
    );
    let body = bound_body(text.as_bytes());
    assert!(body.elided && !body.truncated);
    assert_eq!(
        body.text,
        r#"{"image_url":{"url":"data:image/png;base64,<base64 220 KB elided>"}}"#
    );
    assert_eq!(body.original_bytes, text.len() as u64);
}

#[test]
fn anthropic_and_gemini_payload_keys_are_elided() {
    let anthropic = format!(
        r#"{{"source":{{"type":"base64","media_type":"image/png","data":"{}"}}}}"#,
        b64(8 * 1024)
    );
    let gemini = format!(
        r#"{{"inlineData":{{"mimeType":"image/png","data": "{}"}}}}"#,
        b64(5 * 1024)
    );
    let a = bound_body(anthropic.as_bytes());
    assert!(a.elided);
    assert!(
        a.text.contains(r#""data":"<base64 8 KB elided>""#),
        "{}",
        a.text
    );
    let g = bound_body(gemini.as_bytes());
    assert!(g.elided);
    assert!(
        g.text.contains(r#""data": "<base64 5 KB elided>""#),
        "{}",
        g.text
    );
}

#[test]
fn payloads_at_or_under_the_threshold_are_kept() {
    let text = format!(r#"{{"data":"{}"}}"#, b64(BASE64_ELISION_THRESHOLD));
    let body = bound_body(format!("{text}{}", " ".repeat(10)).as_bytes());
    assert!(!body.elided);
    assert_eq!(body.text.len(), text.len() + 10);
}

#[test]
fn other_long_strings_are_not_mistaken_for_payloads() {
    let text = format!(r#"{{"content":"{}"}}"#, b64(20 * 1024));
    let body = bound_body(text.as_bytes());
    assert!(!body.elided);
    assert_eq!(body.text, text);
}

#[test]
fn a_five_mib_request_with_a_four_mib_image_stores_a_small_body() {
    let text = format!(
        r#"{{"messages":[{{"content":[{{"type":"image","source":{{"data":"{}"}}}},{{"type":"text","text":"{}"}}]}}]}}"#,
        b64(4 * 1024 * 1024),
        "hello ".repeat(170_000)
    );
    assert!(text.len() > 5 * 1024 * 1024 - 100_000);
    let body = bound_body(text.as_bytes());
    assert!(body.elided && body.truncated);
    assert!(body.text.len() <= MAX_BODY_BYTES + BODY_COPY_MARGIN);
    assert!(body.text.contains("<base64 4096 KB elided>"));
    assert_eq!(body.original_bytes, text.len() as u64);
}

#[test]
fn two_payloads_in_one_body_are_both_elided() {
    let text = format!(
        r#"["data:image/png;base64,{}","data:image/png;base64,{}"]"#,
        b64(6000),
        b64(9000)
    );
    let body = bound_body(text.as_bytes());
    assert_eq!(body.text.matches("elided>").count(), 2);
}

#[test]
fn an_unclosed_json_string_is_not_elided() {
    let text = format!(r#"{{"data":"{}"#, b64(8000));
    let body = bound_body(text.as_bytes());
    assert!(!body.elided);
    assert_eq!(body.text, text);
}

#[test]
fn a_cut_inside_invalid_utf8_backs_off_at_most_three_bytes() {
    // Regression guard (the cut backed off over every continuation byte, here to the start).
    let cap = MAX_BODY_BYTES + BODY_COPY_MARGIN;
    let body = bound_body(&vec![0x80; cap + 10]);
    assert_eq!(body.text, "\u{FFFD}".repeat(cap));
    assert!(body.truncated);
    let mut bytes = vec![b'a'; cap - 10];
    bytes.extend([0x80; 20]);
    let body = bound_body(&bytes);
    assert_eq!(
        body.text,
        format!("{}{}", "a".repeat(cap - 10), "\u{FFFD}".repeat(10))
    );
    assert!(body.truncated);
}

#[test]
fn non_utf8_bytes_are_kept_lossily() {
    let body = bound_body(&[b'a', 0xFF, b'b']);
    assert_eq!(body.text, "a\u{FFFD}b");
}

#[test]
fn frames_are_copied_up_to_the_cap_plus_margin_and_count_the_rest() {
    let mut recorder = FrameRecorder::new();
    for i in 0..6_413u64 {
        recorder.push(i, Some("content_block_delta"), "{}");
    }
    let frames = recorder.finish();
    assert_eq!(frames.frames.len(), MAX_FRAMES + FRAME_COPY_MARGIN_FRAMES);
    assert_eq!(frames.total_frames, 6_413);
    assert!(frames.truncated);
    assert_eq!(frames.frames[0].index, 1);
    assert_eq!(frames.frames[1999].index, 2000);
}

#[test]
fn frames_are_copied_up_to_256_kib_plus_margin_as_a_prefix() {
    let mut recorder = FrameRecorder::new();
    let data = "x".repeat(10_000);
    for i in 0..100 {
        recorder.push(i, None, &data);
    }
    // A later small frame must not slip in after a big one was refused.
    let frames = recorder.finish();
    let stored: usize = frames
        .frames
        .iter()
        .map(|f| f.data.len() + f.event.len())
        .sum();
    assert!(stored <= MAX_FRAME_BYTES + FRAME_COPY_MARGIN_BYTES);
    assert_eq!(frames.frames.len(), 27);
    assert_eq!(frames.total_frames, 100);
    assert!(frames.truncated);
}

#[test]
fn frame_offsets_start_at_the_first_frame_and_names_default_to_message() {
    let mut recorder = FrameRecorder::new();
    recorder.push(500, None, "{\"a\":1}");
    recorder.push(733, Some(""), "{\"a\":2}");
    recorder.push(900, Some("ping"), "{}");
    let frames = recorder.finish();
    assert!(!frames.truncated);
    let offsets: Vec<u64> = frames.frames.iter().map(|f| f.t_ms).collect();
    assert_eq!(offsets, [0, 233, 400]);
    let names: Vec<&str> = frames.frames.iter().map(|f| f.event.as_str()).collect();
    assert_eq!(names, ["message", "message", "ping"]);
}

#[test]
fn frame_data_has_its_base64_elided() {
    let mut recorder = FrameRecorder::new();
    recorder.push(
        0,
        None,
        &format!(r#"{{"inlineData":{{"data":"{}"}}}}"#, b64(10 * 1024)),
    );
    let frames = recorder.finish();
    assert!(frames.frames[0].data.contains("<base64 10 KB elided>"));
}
