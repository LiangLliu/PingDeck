//! Unit tests for the SSE parser and frame writers.

use super::*;

fn parse_all(chunks: &[&[u8]]) -> Vec<SseEvent> {
    let mut parser = SseParser::new();
    let mut events = Vec::new();
    for chunk in chunks {
        events.extend(parser.feed(chunk));
    }
    events.extend(parser.finish());
    events
}

#[test]
fn parses_named_and_anonymous_events() {
    let events = parse_all(&[b"event: a\ndata: {\"x\":1}\n\ndata: [DONE]\n\n"]);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event.as_deref(), Some("a"));
    assert_eq!(events[0].data, "{\"x\":1}");
    assert_eq!(events[1].event, None);
    assert_eq!(events[1].data, "[DONE]");
}

#[test]
fn handles_crlf_cr_comments_and_multiline_data() {
    let events = parse_all(&[b": ping\r\ndata: one\r\ndata: two\r\n\r\ndata:three\r\r"]);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].data, "one\ntwo");
    assert_eq!(events[1].data, "three");
}

#[test]
fn bom_utf8_and_mixed_line_endings_survive_every_chunk_boundary() {
    let bytes = "\u{feff}id: 7\r\ndata: héllo\rdata: 世界\r\n\r\ndata: tail".as_bytes();
    let expected = parse_all(&[bytes]);
    for cut in 0..=bytes.len() {
        assert_eq!(
            parse_all(&[&bytes[..cut], b"", &bytes[cut..]]),
            expected,
            "cut at {cut}"
        );
    }
    assert_eq!(expected[0].data, "héllo\n世界");
    assert_eq!(expected[0].id.as_deref(), Some("7"));
    assert_eq!(expected[1].data, "tail");
}

#[test]
fn a_line_exactly_at_the_limit_accepts_a_split_crlf_terminator() {
    let mut parser = SseParser::new();
    assert!(parser.feed(b"\xEF").is_empty());
    assert!(parser.feed(b"\xBB\xBF:").is_empty());
    assert!(parser.feed(&vec![b'x'; MAX_LINE_BYTES - 1]).is_empty());
    assert!(parser.feed(b"\r").is_empty());
    let events = parser.feed(b"\ndata: ok\n\n");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "ok");
}

#[test]
fn frame_round_trips_through_the_parser() {
    let bytes = [frame(Some("m"), "a\nb"), frame(None, "[DONE]")].concat();
    let events = parse_all(&[&bytes]);
    assert_eq!(events[0].event.as_deref(), Some("m"));
    assert_eq!(events[0].data, "a\nb");
    assert_eq!(events[1].data, "[DONE]");
}

#[test]
fn json_frames_match_the_text_frame_byte_for_byte() {
    let values = [
        serde_json::json!({"text": "line\nbreak\rcarriage\u{2028}sep", "n": 1}),
        serde_json::json!({"zh": "中文", "emoji": "🦀", "nested": {"a": [1, {"b": null}]}}),
        serde_json::json!({}),
        serde_json::json!("plain"),
        serde_json::json!([1.5, true, "tab\t"]),
    ];
    for value in &values {
        for event in [None, Some("message_delta")] {
            let mut out = b"prefix".to_vec();
            write_json_frame(&mut out, event, value);
            let expected = [b"prefix".to_vec(), frame(event, &value.to_string())].concat();
            assert_eq!(out, expected, "{value} {event:?}");
            assert_eq!(frame_json(event, value), frame(event, &value.to_string()));
        }
    }
}

#[test]
fn an_oversize_line_overflows_and_ends_parsing() {
    let mut parser = SseParser::new();
    let mut events = parser.feed(b"data: ok\n\n");
    let big = vec![b'x'; MAX_LINE_BYTES + 1];
    events.extend(parser.feed(&big));
    events.extend(parser.feed(b"\ndata: late\n\n"));
    events.extend(parser.finish());
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].data, "ok");
    assert_eq!(events[1].event.as_deref(), Some(OVERFLOW_EVENT));
}

#[test]
fn an_oversize_data_field_overflows_across_lines() {
    let line = format!("data: {}\n", "y".repeat(1024 * 1024 - 8));
    let mut parser = SseParser::new();
    let mut events = Vec::new();
    for _ in 0..9 {
        events.extend(parser.feed(line.as_bytes()));
    }
    events.extend(parser.finish());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event.as_deref(), Some(OVERFLOW_EVENT));
}

#[test]
fn many_events_and_chunks_under_the_limits_still_parse() {
    let chunk = format!("data: {}\n\n", "z".repeat(1024 * 1024));
    let (head, tail) = chunk.as_bytes().split_at(chunk.len() / 2);
    let events = parse_all(&[chunk.as_bytes(), head, tail]);
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e.data.len() == 1024 * 1024));
}

#[test]
fn an_event_only_frame_dispatches_with_empty_data() {
    assert_eq!(
        parse_all(&[b"event: ping\n\n"]),
        [SseEvent {
            event: Some("ping".into()),
            data: String::new(),
            id: None,
        }]
    );
}

#[test]
fn an_id_only_frame_is_not_dispatched_and_its_id_is_dropped() {
    assert_eq!(
        parse_all(&[b"id: 1\n\ndata: x\n\n"]),
        [SseEvent {
            event: None,
            data: "x".into(),
            id: None,
        }]
    );
}

#[test]
fn a_field_line_without_a_colon_has_an_empty_value() {
    let events = parse_all(&[b"data\n\n"]);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "");
    assert_eq!(events[0].event, None);
}

#[test]
fn an_unterminated_last_line_can_overflow_the_data_limit_in_finish() {
    let half = format!("data: {}", "d".repeat(MAX_DATA_BYTES / 2));
    let mut parser = SseParser::new();
    assert!(parser.feed(format!("{half}\n").as_bytes()).is_empty());
    assert!(parser.feed(half.as_bytes()).is_empty());
    let events = parser.finish();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event.as_deref(), Some(OVERFLOW_EVENT));
    assert!(parser.finish().is_empty());
}

#[test]
fn a_lone_trailing_cr_ends_the_last_line() {
    let mut parser = SseParser::new();
    assert!(parser.feed(b"data: x\r").is_empty());
    let events = parser.finish();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "x");
}
