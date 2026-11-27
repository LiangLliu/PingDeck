//! Redact, then elide, then cut: a secret at or across a body or frame cap leaves no fragment.

use super::{SK_KEY, bounded, chat_frame, raw, redact_client_frames};
use crate::llm::capture::{
    BODY_COPY_MARGIN, BoundedBody, FRAME_COPY_MARGIN_FRAMES, FrameRecorder, MAX_BODY_BYTES,
    MAX_FRAMES, redact_exchange,
};

#[test]
fn a_key_at_byte_262140_leaves_no_fragment() {
    let key = format!("sk-{}", "AbCdEfGhIj".repeat(4));
    let mut text = " ".repeat(MAX_BODY_BYTES - 4);
    text.push_str(&key);
    text.push_str(&" tail".repeat(400));
    let body = bounded(&text);
    assert!(!body.truncated, "within the copy margin, copied whole");
    let mut exchange = raw();
    exchange.client_req_body = Some(body);
    let stored = redact_exchange(exchange, &[]).0.client_req_body.unwrap();
    let stored_text = stored.text.unwrap();
    assert!(stored_text.len() <= MAX_BODY_BYTES);
    assert!(stored.truncated);
    assert!(
        !stored_text.contains("sk-") && !stored_text.contains("AbCdEf"),
        "fragment kept"
    );
    assert_eq!(stored.original_bytes, text.len() as u64);
}

#[test]
fn a_key_straddling_the_cap_is_removed_before_the_cut_not_cut_in_half() {
    for offset in [0usize, 1, 5, 20, 34] {
        let key = format!("sk-{}", "Zz9Yy8Xx7W".repeat(4));
        let mut text = " ".repeat(MAX_BODY_BYTES - offset);
        text.push_str(&key);
        text.push_str(&"z".repeat(BODY_COPY_MARGIN / 2));
        let mut exchange = raw();
        exchange.client_req_body = Some(bounded(&text));
        let stored = redact_exchange(exchange, &[])
            .0
            .client_req_body
            .unwrap()
            .text
            .unwrap();
        assert!(
            !stored.contains("sk-") && !stored.contains("Zz9Yy"),
            "offset {offset}"
        );
    }
}

#[test]
fn a_secret_after_the_copy_point_of_a_300_kib_body_is_never_stored() {
    let mut text = "p".repeat(290 * 1024);
    text.push_str(SK_KEY);
    text.push_str(&"q".repeat(10 * 1024));
    let mut exchange = raw();
    exchange.client_req_body = Some(bounded(&text));
    let stored = redact_exchange(exchange, &[]).0.client_req_body.unwrap();
    assert!(stored.truncated);
    assert!(!stored.text.unwrap().contains("sk-"));
}

#[test]
fn the_writer_elides_base64_that_the_request_path_did_not() {
    let payload = "QUJD".repeat(2000);
    let mut exchange = raw();
    exchange.client_req_body = Some(BoundedBody {
        text: format!(r#"{{"source":{{"data":"{payload}"}}}}"#),
        truncated: false,
        elided: false,
        original_bytes: 9000,
    });
    let body = redact_exchange(exchange, &[]).0.client_req_body.unwrap();
    assert!(body.elided && !body.truncated);
    assert!(body.text.unwrap().contains("<base64 8 KB elided>"));
}

#[test]
fn frames_are_cut_to_the_caps_after_redaction_so_no_fragment_sits_at_the_cut() {
    let mut recorder = FrameRecorder::new();
    for i in 0..2100u32 {
        let content = match i {
            1998 => "sk-abcdefgh",
            1999 => "ijklmnopqrstuvwxyz0123",
            _ => " ",
        };
        recorder.push(u64::from(i), None, &chat_frame(i, content).data);
    }
    let copied = recorder.finish();
    assert_eq!(copied.frames.len(), MAX_FRAMES + FRAME_COPY_MARGIN_FRAMES);
    let stored = redact_client_frames(copied, &[]);
    assert_eq!(stored.frames.len(), MAX_FRAMES);
    assert_eq!(stored.total_frames, 2100);
    assert!(stored.truncated);
    let all: String = stored.frames.iter().map(|f| f.data.as_str()).collect();
    assert!(!all.contains("sk-") && !all.contains("abcdefgh"));
}

#[test]
fn a_key_straddling_the_frame_cap_leaves_no_fragment() {
    let mut recorder = FrameRecorder::new();
    for i in 0..2100u32 {
        let content = match i {
            1999 => "sk-abcdefgh",
            2000 => "ijklmnopqrstuvwxyz0123",
            _ => " ",
        };
        recorder.push(u64::from(i), None, &chat_frame(i, content).data);
    }
    let stored = redact_client_frames(recorder.finish(), &[]);
    assert_eq!(stored.frames.len(), MAX_FRAMES);
    let last = &stored.frames[MAX_FRAMES - 1].data;
    assert!(!last.contains("sk-"), "{last}");
}
