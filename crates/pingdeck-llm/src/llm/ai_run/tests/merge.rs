//! Folding the events of a stream and the JSON of a plain answer.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Instant;

use serde_json::json;

use super::super::merge::{from_body, from_stream};
use super::super::read::{Ending, Reading};
use crate::llm::adapters::codec;
use crate::llm::gateway::tests::{Canned, Step, canned_json, canned_sse};
use crate::llm::hub::HubFinish;
use crate::llm::sse::SseParser;
use crate::llm::wire::Protocol;

/// A reading holding the decoded events of `steps` as `protocol`.
fn reading(protocol: Protocol, steps: &[Step], finish: bool) -> Reading {
    let mut parser = SseParser::new();
    let mut decoder = codec(protocol).unwrap().stream_decoder();
    let mut events = Vec::new();
    let mut finish_seen = None;
    for step in steps {
        if let Step::Frame(text) = step {
            for sse in parser.feed(text.as_bytes()) {
                for event in decoder.push(&sse).unwrap() {
                    if let crate::llm::hub::HubEvent::Finish { reason } = &event {
                        finish_seen = Some(reason.clone());
                    }
                    events.push(event);
                }
            }
        }
    }
    if finish {
        events.extend(decoder.finish().unwrap());
    }
    Reading {
        streamed: true,
        raw: Vec::new(),
        truncated: false,
        frames: Vec::new(),
        recorded: None,
        events,
        finish: finish_seen,
        first_content: None,
        last_byte: Instant::now(),
        ending: Ending::Complete,
    }
}

const CHAT: [Protocol; 4] = [
    Protocol::OpenaiChat,
    Protocol::OpenaiResponses,
    Protocol::AnthropicMessages,
    Protocol::Gemini,
];

#[test]
fn every_chat_protocol_merges_text_usage_and_finish_from_its_stream() {
    for protocol in CHAT {
        let canned = Canned {
            text: vec!["Hel", "lo"],
            usage: (11, 7),
            ..Canned::default()
        };
        let merged = from_stream(&reading(
            protocol,
            &canned_sse(protocol, "m", &canned),
            true,
        ));
        assert_eq!(merged.reply, "Hello", "{protocol:?}");
        assert_eq!(merged.usage.input_tokens, Some(11), "{protocol:?}");
        assert_eq!(merged.usage.output_tokens, Some(7), "{protocol:?}");
        assert!(merged.finish.is_some(), "{protocol:?}");
        assert!(merged.stream_error.is_none());
    }
}

#[test]
fn every_chat_protocol_merges_a_plain_answer() {
    for protocol in CHAT {
        let canned = Canned {
            text: vec!["Hel", "lo"],
            usage: (11, 7),
            ..Canned::default()
        };
        let body = canned_json(protocol, "m", &canned).to_string();
        let merged = from_body(protocol, body.as_bytes()).unwrap();
        assert_eq!(merged.reply, "Hello", "{protocol:?}");
        assert_eq!(merged.usage.output_tokens, Some(7));
        assert!(merged.finish.is_some());
    }
}

#[test]
fn a_cut_stream_has_no_finish_reason_but_keeps_its_text() {
    let canned = Canned::text(&["par", "tial"]);
    let steps = canned_sse(Protocol::OpenaiChat, "m", &canned);
    let cut: Vec<Step> = steps.into_iter().take(2).collect();
    let mut cut_reading = reading(Protocol::OpenaiChat, &cut, false);
    cut_reading.ending = Ending::Stopped;
    let merged = from_stream(&cut_reading);
    assert!(merged.reply.starts_with("par"));
    assert_eq!(merged.finish, None);
}

#[test]
fn reasoning_and_tool_calls_come_out_with_ids() {
    let merged = from_stream(&reading(
        Protocol::OpenaiChat,
        &canned_sse(Protocol::OpenaiChat, "m", &Canned::reasoning()),
        true,
    ));
    assert_eq!(merged.reasoning, "let me think");
    let merged = from_stream(&reading(
        Protocol::OpenaiChat,
        &canned_sse(Protocol::OpenaiChat, "m", &Canned::tool()),
        true,
    ));
    assert_eq!(merged.tool_calls[0].id, "call_9");
    assert_eq!(merged.tool_calls[0].arguments, "{\"city\":\"Paris\"}");
    assert_eq!(merged.finish.as_deref(), Some("tool_calls"));
    let body = canned_json(Protocol::OpenaiChat, "m", &Canned::tool()).to_string();
    let merged = from_body(Protocol::OpenaiChat, body.as_bytes()).unwrap();
    assert_eq!(merged.tool_calls[0].name, "get_weather");
}

#[test]
fn an_error_event_is_reported_and_the_text_before_it_stays() {
    let steps = vec![
        Step::Frame(format!(
            "data: {}\n\n",
            json!({"id": "c", "model": "m", "choices": [{"index": 0, "delta": {"content": "half"}}]})
        )),
        Step::Frame(format!(
            "data: {}\n\n",
            json!({"error": {"message": "overloaded"}})
        )),
    ];
    let merged = from_stream(&reading(Protocol::OpenaiChat, &steps, false));
    assert_eq!(merged.reply, "half");
    assert_eq!(merged.stream_error.as_deref(), Some("overloaded"));
}

#[test]
fn a_body_that_is_not_json_or_not_an_answer_is_an_error_message() {
    assert!(
        from_body(Protocol::OpenaiChat, b"<html>")
            .unwrap_err()
            .contains("not valid JSON")
    );
    assert!(from_body(Protocol::OpenaiChat, b"{\"nope\":1}").is_err());
    assert!(
        from_body(Protocol::Embeddings, b"{\"data\":[]}")
            .unwrap_err()
            .contains("no embedding")
    );
}

#[test]
fn embedding_vectors_cover_openai_gemini_and_batch_shapes() {
    let openai = json!({"data": [{"embedding": [1.0, 2.0, 3.0]}], "usage": {"prompt_tokens": 3, "total_tokens": 3}});
    let merged = from_body(Protocol::Embeddings, openai.to_string().as_bytes()).unwrap();
    assert_eq!(merged.vectors.len(), 1);
    assert_eq!(merged.vectors[0].dimensions, 3);
    assert_eq!(merged.usage.input_tokens, Some(3));
    let gemini = json!({"embedding": {"values": [0.1, 0.2]}});
    let merged = from_body(Protocol::Embeddings, gemini.to_string().as_bytes()).unwrap();
    assert_eq!(merged.vectors[0].dimensions, 2);
    let batch = json!({"embeddings": [{"values": [1.0]}, {"values": [2.0, 3.0]}]});
    let merged = from_body(Protocol::Embeddings, batch.to_string().as_bytes()).unwrap();
    assert_eq!(
        merged
            .vectors
            .iter()
            .map(|v| v.dimensions)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    let long: Vec<f64> = (0..20).map(f64::from).collect();
    let wide = json!({"data": [{"embedding": long}]});
    let merged = from_body(Protocol::Embeddings, wide.to_string().as_bytes()).unwrap();
    assert_eq!(merged.vectors[0].first_values.len(), 8);
    assert_eq!(merged.vectors[0].dimensions, 20);
}

#[test]
fn finish_words_follow_the_log_drawer() {
    let canned = Canned::text(&["x"]);
    let mut stream = reading(
        Protocol::OpenaiChat,
        &canned_sse(Protocol::OpenaiChat, "m", &canned),
        true,
    );
    assert_eq!(from_stream(&stream).finish.as_deref(), Some("stop"));
    stream.finish = Some(HubFinish::Length);
    assert_eq!(from_stream(&stream).finish.as_deref(), Some("length"));
    stream.finish = Some(HubFinish::Other("weird".to_owned()));
    assert_eq!(from_stream(&stream).finish.as_deref(), Some("weird"));
}
