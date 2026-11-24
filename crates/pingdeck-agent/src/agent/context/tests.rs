//! Tests for token estimation, repair, pruning and compaction.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::json;

use std::collections::HashSet;

use super::*;
use crate::agent::tools::{ToolClass, ToolSource};

fn call(id: &str, name: &str) -> HubPart {
    HubPart::ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: "{}".into(),
        signature: None,
        origin: None,
    }
}

fn result(id: &str, text: &str) -> HubPart {
    HubPart::ToolResult {
        call_id: id.into(),
        name: None,
        content: vec![HubPart::text(text)],
        is_error: false,
    }
}

fn assistant_calls(ids: &[&str]) -> HubMessage {
    HubMessage {
        role: HubRole::Assistant,
        content: ids.iter().map(|id| call(id, "read")).collect(),
    }
}

fn tool(parts: Vec<HubPart>) -> HubMessage {
    HubMessage {
        role: HubRole::Tool,
        content: parts,
    }
}

fn spec(name: &str, description: &str) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema: json!({"type": "object"}),
        read_only: true,
        destructive: false,
        source: ToolSource::Builtin,
        class: ToolClass::Read,
        parallel_safe: true,
    }
}

fn image() -> HubPart {
    HubPart::Image {
        image: HubImage::Base64 {
            media_type: "image/png".into(),
            data: "AAAA".repeat(1000),
        },
    }
}

fn document(source: HubDocumentSource) -> HubPart {
    HubPart::Document {
        document: HubDocument {
            media_type: "text/plain".into(),
            name: Some("notes.txt".into()),
            source,
        },
    }
}

fn user_msg(parts: Vec<HubPart>) -> HubMessage {
    HubMessage {
        role: HubRole::User,
        content: parts,
    }
}

// ---- estimation ---------------------------------------------------------------------------

#[test]
fn estimate_is_deterministic_and_counts_text() {
    let messages = vec![
        HubMessage::user("a".repeat(400)),
        HubMessage::assistant("b".repeat(40)),
    ];
    let first = estimate_context_tokens(Some(&"s".repeat(8)), &messages, &[]);
    assert_eq!(first, 112); // (400 + 40 + 8) chars / 4
    assert_eq!(
        first,
        estimate_context_tokens(Some(&"s".repeat(8)), &messages, &[])
    );
    assert_eq!(estimate_context_tokens(None, &[], &[]), 0);
}

#[test]
fn estimate_charges_media_flat() {
    let image_msg = user_msg(vec![image()]);
    assert_eq!(estimate_context_tokens(None, &[image_msg], &[]), 1500);
    let pdf = user_msg(vec![document(HubDocumentSource::Base64 {
        data: "x".repeat(9000),
    })]);
    assert_eq!(estimate_context_tokens(None, &[pdf], &[]), 1000);
    let text_doc = user_msg(vec![document(HubDocumentSource::Text {
        text: "t".repeat(80),
    })]);
    assert_eq!(estimate_context_tokens(None, &[text_doc], &[]), 20);
    let audio = user_msg(vec![HubPart::Audio {
        media_type: "audio/wav".into(),
        data: "x".into(),
    }]);
    assert_eq!(estimate_context_tokens(None, &[audio], &[]), 500);
}

#[test]
fn estimate_covers_tool_calls_results_and_specs() {
    let calls = HubMessage {
        role: HubRole::Assistant,
        content: vec![HubPart::ToolCall {
            id: "c1".into(),
            name: "read".into(), // 4 chars
            arguments: "x".repeat(36),
            signature: None,
            origin: None,
        }],
    };
    assert_eq!(estimate_context_tokens(None, &[calls], &[]), 10);
    let results = tool(vec![HubPart::ToolResult {
        call_id: "c1".into(),
        name: None,
        content: vec![HubPart::text("y".repeat(40)), image()],
        is_error: false,
    }]);
    assert_eq!(estimate_context_tokens(None, &[results], &[]), 10 + 1500);
    // name 4 + description 8 + `{"type":"object"}` 17 = 29 chars => 8 tokens.
    assert_eq!(
        estimate_context_tokens(None, &[], &[spec("read", "12345678")]),
        8
    );
}

#[test]
fn threshold_edges() {
    assert!(!should_compact(100, Some(100), 0));
    assert!(!should_compact(u64::MAX, None, 80));
    assert!(!should_compact(80, Some(100), 80), "equal is not above");
    assert!(should_compact(81, Some(100), 80));
    assert!(!should_compact(0, Some(0), 50));
    assert!(should_compact(1, Some(0), 50));
    assert!(should_compact(u64::MAX, Some(u64::MAX), 99));
}

// ---- repair -------------------------------------------------------------------------------

#[test]
fn orphan_call_repair_preserves_results_order_boundaries_and_is_idempotent() {
    let pending = |id: &str| HubPart::ToolResult {
        call_id: id.into(),
        name: Some("read".into()),
        content: vec![HubPart::text("[interrupted before tool finished]")],
        is_error: true,
    };
    let valid = vec![
        HubMessage::user("go"),
        assistant_calls(&["a", "b"]),
        tool(vec![result("a", "1"), result("b", "2")]),
        HubMessage::assistant("done"),
    ];
    let cases = [
        (
            "missing tool message",
            vec![HubMessage::user("go"), assistant_calls(&["a"])],
            vec![
                HubMessage::user("go"),
                assistant_calls(&["a"]),
                tool(vec![pending("a")]),
            ],
        ),
        (
            "insert before following user",
            vec![
                HubMessage::user("go"),
                assistant_calls(&["a"]),
                HubMessage::user("again"),
            ],
            vec![
                HubMessage::user("go"),
                assistant_calls(&["a"]),
                tool(vec![pending("a")]),
                HubMessage::user("again"),
            ],
        ),
        (
            "partial results in call order",
            vec![
                HubMessage::user("go"),
                assistant_calls(&["a", "b"]),
                tool(vec![result("b", "done")]),
            ],
            vec![
                HubMessage::user("go"),
                assistant_calls(&["a", "b"]),
                tool(vec![pending("a"), result("b", "done")]),
            ],
        ),
        (
            "results split over tool messages",
            vec![
                HubMessage::user("go"),
                assistant_calls(&["a", "b", "c"]),
                tool(vec![result("a", "1")]),
                tool(vec![result("b", "2")]),
            ],
            vec![
                HubMessage::user("go"),
                assistant_calls(&["a", "b", "c"]),
                tool(vec![result("a", "1"), pending("c")]),
                tool(vec![result("b", "2")]),
            ],
        ),
        (
            "orphan duplicate and late results",
            vec![
                HubMessage::user("go"),
                tool(vec![result("ghost", "x")]),
                assistant_calls(&["a"]),
                tool(vec![
                    result("a", "ok"),
                    result("ghost", "y"),
                    result("a", "dup"),
                ]),
                HubMessage::user("next"),
                tool(vec![result("a", "late")]),
            ],
            vec![
                HubMessage::user("go"),
                assistant_calls(&["a"]),
                tool(vec![result("a", "ok")]),
                HubMessage::user("next"),
            ],
        ),
        (
            "empty assistant and orphan tool messages",
            vec![
                HubMessage::user("go"),
                HubMessage {
                    role: HubRole::Assistant,
                    content: Vec::new(),
                },
                tool(vec![result("a", "orphan")]),
                HubMessage::assistant("answer"),
            ],
            vec![HubMessage::user("go"), HubMessage::assistant("answer")],
        ),
        (
            "successive calls with a missing tail",
            vec![
                HubMessage::user("go"),
                assistant_calls(&["a", "b"]),
                tool(vec![result("b", "2"), result("z", "?")]),
                assistant_calls(&["c"]),
            ],
            vec![
                HubMessage::user("go"),
                assistant_calls(&["a", "b"]),
                tool(vec![pending("a"), result("b", "2")]),
                assistant_calls(&["c"]),
                tool(vec![pending("c")]),
            ],
        ),
        ("valid history is unchanged", valid.clone(), valid),
    ];
    for (case, mut messages, expected) in cases {
        repair_orphan_tool_calls(&mut messages);
        assert_eq!(messages, expected, "{case}");
        repair_orphan_tool_calls(&mut messages);
        assert_eq!(messages, expected, "{case}: second repair");
    }
}

// ---- pruning ------------------------------------------------------------------------------

fn tool_result_text(message: &HubMessage) -> Vec<&HubPart> {
    match &message.content[0] {
        HubPart::ToolResult { content, .. } => content.iter().collect(),
        other => panic!("not a tool result: {other:?}"),
    }
}

/// A history of `turns` user turns, each a `read_file` of a 10 KiB file and its result.
fn reading_turns(turns: usize) -> Vec<HubMessage> {
    let mut history = Vec::new();
    for turn in 0..turns {
        let id = format!("r{turn}");
        history.push(HubMessage::user(format!("turn {turn}")));
        history.push(HubMessage {
            role: HubRole::Assistant,
            content: vec![HubPart::ToolCall {
                id: id.clone(),
                name: if turn == 0 { "todo_write" } else { "read_file" }.into(),
                arguments: format!(r#"{{"path":"{}"}}"#, if turn == 1 { "a" } else { "b" }),
                signature: None,
                origin: None,
            }],
        });
        history.push(tool(vec![result(&id, &"x".repeat(10 * 1024))]));
    }
    history
}

#[test]
fn the_watermark_moves_at_half_the_window_and_skips_exempt_results() {
    let history = reading_turns(5);
    let none = HashSet::new();
    // Below half of the window the watermark stays, whatever pruning would free.
    assert_eq!(
        advance_watermark(&history, None, &none, 49_999, 100_000),
        None
    );
    // At half it moves to before the last two user turns. Turn 0 is `todo_write` (exempt);
    // turn 1 is the latest read of path `a` (exempt); turns 2 and 3 are older reads of `b`;
    // turn 4's read of `b` is the latest but lies in the kept turns anyway.
    let (through, calls) = advance_watermark(&history, None, &none, 6_000, 12_000).unwrap();
    assert_eq!(through, 8);
    assert_eq!(calls, ["r2"]);
    // Freeing less than 10 % of the window (8,240 bytes < 4 x 30,000 x 10 %) does not move it.
    assert_eq!(
        advance_watermark(&history, None, &none, 15_000, 30_000),
        None
    );
    // Nor does a watermark already there.
    assert_eq!(
        advance_watermark(&history, Some(8), &none, 6_000, 12_000),
        None
    );

    let mut state = ContextState::default();
    state.pruned.extend(calls);
    let mut pruned = history.clone();
    prune_calls(&mut pruned, &state);
    let text = text_of_result(&pruned[8]);
    assert!(text.starts_with(&"x".repeat(2_000)));
    assert!(text.ends_with(&format!("…[pruned {} bytes]", 10 * 1024 - 2_000)));
    // A kept call names its handle; everything else is untouched.
    state.kept.insert("r2".into());
    let mut kept = history.clone();
    prune_calls(&mut kept, &state);
    assert!(text_of_result(&kept[8]).contains(r#"read_output {"handle":"r2","offset":2000}"#));
    assert_eq!(pruned[5], history[5]);
    assert_eq!(pruned[11], history[11]);
}

fn text_of_result(message: &HubMessage) -> String {
    match tool_result_text(message)[0] {
        HubPart::Text { text, .. } => text.clone(),
        other => panic!("not text: {other:?}"),
    }
}

// ---- compaction ---------------------------------------------------------------------------

async fn compact(
    messages: &[HubMessage],
    keep_last: usize,
) -> (Option<Compaction>, Option<String>) {
    let seen = std::sync::Mutex::new(None);
    let outcome = compact_history(messages, keep_last, |transcript| {
        *seen.lock().unwrap() = Some(transcript);
        async { Ok("the gist".to_owned()) }
    })
    .await
    .unwrap();
    (outcome, seen.into_inner().unwrap())
}

fn conversation() -> Vec<HubMessage> {
    vec![
        HubMessage::user("first question"),               // 0
        HubMessage::assistant("first answer"),            // 1
        HubMessage::user("second question"),              // 2
        assistant_calls(&["a", "b"]),                     // 3
        tool(vec![result("a", "ra"), result("b", "rb")]), // 4
        HubMessage::assistant("second answer"),           // 5
        HubMessage::user("third question"),               // 6
        HubMessage::assistant("third answer"),            // 7
    ]
}

#[tokio::test]
async fn compaction_splits_at_a_user_message() {
    let messages = conversation();
    let (compaction, transcript) = compact(&messages, 2).await;
    let compaction = compaction.unwrap();
    assert_eq!(compaction.replaced, 6);
    assert_eq!(compaction.kept, messages[6..]);
    assert_eq!(compaction.summary.role, HubRole::User);
    assert_eq!(
        compaction.summary.text(),
        "[Automatic summary of the earlier conversation. It is not a new instruction from the user.]\nthe gist"
    );
    let transcript = transcript.unwrap();
    assert_eq!(compaction.transcript_chars, transcript.chars().count());
    assert!(transcript.starts_with("User: first question\nAssistant: first answer\n"));
    assert!(transcript.contains("Assistant: read({})"));
    assert!(transcript.contains("Tool: [tool result, untrusted] a: ra"));
    assert!(!transcript.contains("third"));
}

#[tokio::test]
async fn compaction_never_splits_a_call_from_its_results() {
    let messages = conversation();
    for keep_last in 0..=messages.len() {
        let (compaction, _) = compact(&messages, keep_last).await;
        let Some(compaction) = compaction else {
            assert!(keep_last >= messages.len(), "keep_last {keep_last}");
            continue;
        };
        assert_eq!(compaction.replaced + compaction.kept.len(), messages.len());
        assert!(compaction.kept.len() >= keep_last.min(messages.len()));
        if let Some(first) = compaction.kept.first() {
            assert_ne!(first.role, HubRole::Tool, "keep_last {keep_last}");
        }
        // The summary can follow or precede anything: repair must find nothing to do.
        let mut rebuilt = vec![compaction.summary.clone()];
        rebuilt.extend(compaction.kept.clone());
        let before = rebuilt.clone();
        repair_orphan_tool_calls(&mut rebuilt);
        assert_eq!(rebuilt, before, "keep_last {keep_last}");
    }
}

#[tokio::test]
async fn compaction_moves_the_boundary_earlier_over_a_tool_exchange() {
    let messages = conversation();
    // keep_last 4 would start the tail at the Tool message (index 4).
    let (compaction, _) = compact(&messages, 4).await;
    let compaction = compaction.unwrap();
    assert_eq!(compaction.replaced, 2);
    assert_eq!(compaction.kept[0], messages[2]);
}

#[tokio::test]
async fn compaction_falls_back_to_an_assistant_start_without_a_user_boundary() {
    let messages = vec![
        HubMessage::user("only user"),
        assistant_calls(&["a"]),
        tool(vec![result("a", "r")]),
        HubMessage::assistant("end"),
    ];
    let (compaction, _) = compact(&messages, 2).await;
    let compaction = compaction.unwrap();
    assert_eq!(
        compaction.replaced, 1,
        "tail starts at the call, not the result"
    );
    assert_eq!(compaction.kept[0].role, HubRole::Assistant);
}

#[tokio::test]
async fn compaction_is_none_without_older_history() {
    let messages = conversation();
    assert!(compact(&messages, 8).await.0.is_none());
    assert!(compact(&messages, 100).await.0.is_none());
    assert!(compact(&[], 0).await.0.is_none());
    // Nothing before the first boundary: the only candidate would be index 0.
    let tool_heavy = vec![
        HubMessage::user("q"),
        assistant_calls(&["a"]),
        tool(vec![result("a", "r")]),
    ];
    assert!(compact(&tool_heavy, 1).await.0.is_some());
    let leading = vec![assistant_calls(&["a"]), tool(vec![result("a", "r")])];
    assert!(compact(&leading, 1).await.0.is_none());
}

#[tokio::test]
async fn compaction_can_keep_nothing() {
    let messages = conversation();
    let compaction = compact(&messages, 0).await.0.unwrap();
    assert_eq!(compaction.replaced, messages.len());
    assert!(compaction.kept.is_empty());
}

#[tokio::test]
async fn transcript_cuts_calls_results_and_media() {
    let messages = vec![
        HubMessage::user("start"),
        HubMessage {
            role: HubRole::Assistant,
            content: vec![
                HubPart::Reasoning {
                    text: "secret thoughts".into(),
                    signature: None,
                    encrypted: None,
                    id: None,
                    origin: None,
                },
                HubPart::ToolCall {
                    id: "a".into(),
                    name: "write".into(),
                    arguments: "p".repeat(1000),
                    signature: None,
                    origin: None,
                },
            ],
        },
        tool(vec![HubPart::ToolResult {
            call_id: "a".into(),
            name: Some("write".into()),
            content: vec![HubPart::text("r".repeat(1000)), image()],
            is_error: true,
        }]),
        user_msg(vec![document(HubDocumentSource::Text { text: "d".into() })]),
        HubMessage::user("tail"),
    ];
    let (_, transcript) = compact(&messages, 1).await;
    let transcript = transcript.unwrap();
    assert!(!transcript.contains("secret"));
    assert!(transcript.contains(&format!("write({}…)", "p".repeat(300))));
    assert!(!transcript.contains(&"p".repeat(301)));
    assert!(transcript.contains(&format!(
        "[tool result, untrusted] write (error): {} … {}\n[image]",
        "r".repeat(250),
        "r".repeat(242)
    )));
    assert!(!transcript.contains(&"r".repeat(251)));
    assert!(transcript.contains("[document attached: notes.txt]"));
    assert!(!transcript.contains("AAAA"));
}

#[tokio::test]
async fn transcript_is_capped_keeping_head_and_tail() {
    let mut messages: Vec<HubMessage> = (0..500)
        .flat_map(|n| {
            [
                HubMessage::user(format!("q{n} {}", "x".repeat(1000))),
                HubMessage::assistant("ok"),
            ]
        })
        .collect();
    messages.push(HubMessage::user("recent"));
    let (compaction, transcript) = compact(&messages, 1).await;
    let transcript = transcript.unwrap();
    assert!(transcript.chars().count() <= TRANSCRIPT_CAP_CHARS + 64);
    assert!(transcript.starts_with("User: q0 "));
    assert!(transcript.contains("characters omitted"));
    assert!(transcript.ends_with("Assistant: ok\n"));
    assert_eq!(
        compaction.unwrap().transcript_chars,
        transcript.chars().count()
    );
}

#[tokio::test]
async fn summarize_errors_propagate_and_blank_summaries_are_rejected() {
    let messages = conversation();
    let failed = compact_history(&messages, 2, |_| async {
        Err(AppError::network("model unreachable"))
    })
    .await;
    assert!(matches!(failed, Err(AppError::Network { .. })));
    let blank = compact_history(&messages, 2, |_| async { Ok("  \n".to_owned()) }).await;
    assert!(matches!(blank, Err(AppError::Validation { .. })));
}

#[test]
fn compaction_request_is_tool_less_and_non_streaming() {
    let request = compaction_request("m", "User: hi\n");
    assert_eq!(request.model, "m");
    assert_eq!(request.system.as_deref(), Some(COMPACTION_PROMPT));
    assert_eq!(request.messages, vec![HubMessage::user("User: hi\n")]);
    assert!(request.tools.is_empty() && request.native_tools.is_empty());
    assert!(!request.stream);
}

#[test]
fn the_last_turns_are_counted_from_the_nth_newest_user_message() {
    let messages = vec![
        HubMessage::user("one"),
        HubMessage::assistant("a1"),
        HubMessage::user("two"),
        assistant_calls(&["c"]),
        tool(vec![result("c", "r")]),
        HubMessage::assistant("a2"),
        HubMessage::user("three"),
        HubMessage::assistant("a3"),
    ];
    assert_eq!(messages_in_last_turns(&messages, 0), 0);
    assert_eq!(messages_in_last_turns(&messages, 1), 2);
    assert_eq!(messages_in_last_turns(&messages, 2), 6);
    assert_eq!(messages_in_last_turns(&messages, 3), 8);
    assert_eq!(messages_in_last_turns(&messages, 9), 8);
    assert_eq!(messages_in_last_turns(&[], 2), 0);
}

/// One user message driving `steps` tool steps, after an earlier summary.
fn one_long_turn(steps: usize) -> Vec<HubMessage> {
    let mut messages = vec![
        HubMessage::user(format!("{SUMMARY_HEADER}\nearlier work")),
        HubMessage::user("refactor the parser"),
    ];
    for step in 0..steps {
        let id = format!("c{step}");
        messages.push(assistant_calls(&[&id]));
        messages.push(tool(vec![result(&id, &"r".repeat(10 * 1024))]));
    }
    messages
}

#[test]
fn a_long_tool_chain_in_one_turn_is_cut_between_its_steps() {
    let messages = one_long_turn(10);
    let roles: Vec<HubRole> = messages.iter().map(|m| m.role).collect();
    let summary = |index: usize| index == 0;
    // The kept turn is everything after the summary: the cut moves inside it and keeps the last
    // four steps, starting at a call, never at a result.
    let cut = compaction_cut(&roles, summary, 2).unwrap();
    assert!(cut.mid_turn);
    assert_eq!(cut.split, messages.len() - 2 * COMPACT_KEEP_STEPS);
    assert_eq!(roles[cut.split], HubRole::Assistant);
    // Between turns the cut stays at a user message.
    let mut two_turns = conversation();
    two_turns.extend(one_long_turn(1).into_iter().skip(1));
    let roles: Vec<HubRole> = two_turns.iter().map(|m| m.role).collect();
    let cut = compaction_cut(&roles, |_| false, 1).unwrap();
    assert!(!cut.mid_turn);
    assert_eq!(roles[cut.split], HubRole::User);
    // A turn of too few steps holds nothing to cut.
    let short = one_long_turn(3);
    let roles: Vec<HubRole> = short.iter().map(|m| m.role).collect();
    assert_eq!(compaction_cut(&roles, summary, 2), None);
}

#[tokio::test]
async fn a_mid_turn_summary_repeats_the_users_request() {
    let messages = one_long_turn(10);
    let roles: Vec<HubRole> = messages.iter().map(|m| m.role).collect();
    let cut = compaction_cut(&roles, |index| index == 0, 2).unwrap();
    let compaction = compact_history_at(&messages, cut, |_| async { Ok("gist".to_owned()) })
        .await
        .unwrap();
    let text = compaction.summary.text();
    assert!(text.starts_with(SUMMARY_HEADER));
    assert!(text.ends_with("verbatim:]\nrefactor the parser"), "{text}");
    assert!(
        !text.contains("earlier work"),
        "an earlier summary is not the request"
    );
    assert_eq!(compaction.kept, messages[cut.split..]);
    // A second cut inside the same turn carries the request over from the first summary.
    let mut again = vec![compaction.summary.clone()];
    again.extend(compaction.kept.clone());
    again.extend(one_long_turn(6).into_iter().skip(2));
    let roles: Vec<HubRole> = again.iter().map(|m| m.role).collect();
    let cut = compaction_cut(&roles, |index| index == 0, 2).unwrap();
    let second = compact_history_at(&again, cut, |_| async { Ok("more".to_owned()) })
        .await
        .unwrap();
    assert!(
        second
            .summary
            .text()
            .ends_with("verbatim:]\nrefactor the parser")
    );
}

#[test]
fn the_watermark_prunes_inside_one_long_turn() {
    let messages = one_long_turn(10);
    let (through, calls) =
        advance_watermark(&messages, None, &HashSet::new(), 6_000, 12_000).unwrap();
    // Everything before the last four steps.
    assert_eq!(through, messages.len() - 2 * PRUNE_KEEP_STEPS - 1);
    assert_eq!(calls.len(), 10 - PRUNE_KEEP_STEPS);
}

#[tokio::test]
async fn compaction_keeps_the_last_two_turns_verbatim() {
    let messages = vec![
        HubMessage::user("one"),
        HubMessage::assistant("a1"),
        HubMessage::user("two"),
        HubMessage::assistant("a2"),
        HubMessage::user("three"),
        HubMessage::assistant("a3"),
    ];
    let keep = messages_in_last_turns(&messages, 2);
    let compaction = compact_history(&messages, keep, |_| async { Ok("summary".to_owned()) })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(compaction.kept, messages[2..]);
    assert_eq!(compaction.replaced, 2);
    assert_eq!(compaction.summary.role, HubRole::User);
}

#[test]
fn context_length_errors_are_recognised_across_providers() {
    for message in [
        "This model's maximum context length is 128000 tokens. However, your messages resulted in 130000 tokens.",
        r#"{"error":{"code":"context_length_exceeded","message":"too long"}}"#,
        "prompt is too long: 210000 tokens > 200000 maximum",
        "The input token count (1100000) exceeds the maximum number of tokens allowed (1048576).",
        "Invalid request: Your request exceeded model token limit: 8192",
        "Please reduce the length of the messages or completion.",
        "Input is too long for requested model.",
        "Your input exceeds the context window of this model",
        "input length and `max_tokens` exceed context limit: 195000 + 8192 > 200000",
        "This model's maximum context length is 128000 tokens. However, you requested 130000 tokens (125000 in the messages, 5000 in max_tokens).",
    ] {
        for error in [AppError::network(message), AppError::validation(message)] {
            assert!(is_context_length_error(&error), "{message}");
        }
    }
}

#[test]
fn other_failures_are_not_context_length_errors() {
    for message in [
        "rate limit exceeded, retry in 20s",
        "invalid api key",
        "max_tokens: 100000 > 8192, which is the maximum allowed number of output tokens",
        "connection reset by peer",
        "",
    ] {
        assert!(
            !is_context_length_error(&AppError::network(message)),
            "{message}"
        );
    }
}

// ------------------------------------------------------------------------- result caps

fn tool_message(results: Vec<HubPart>) -> HubMessage {
    HubMessage {
        role: HubRole::Tool,
        content: results,
    }
}

fn result_len(message: &HubMessage) -> usize {
    turn_result_bytes(std::slice::from_ref(message))
}

#[test]
fn the_caps_follow_the_models_window() {
    // 8 % of a 32k window at 4 bytes a token; a quarter of it for one turn.
    assert_eq!(result_cap_bytes(Some(32_768)), 10_485);
    assert_eq!(turn_cap_bytes(Some(32_768)), 32_768);
    assert_eq!(result_cap_bytes(Some(1_000_000)), MAX_RESULT_BYTES);
    // An unknown window is budgeted like the 128,000-token fallback, not given the maximum.
    let fallback = crate::agent::limits::UNKNOWN_WINDOW_TOKENS;
    assert_eq!(result_cap_bytes(None), result_cap_bytes(Some(fallback)));
    assert!(result_cap_bytes(None) < MAX_RESULT_BYTES);
    assert_eq!(turn_cap_bytes(None), 128_000);
    assert_eq!(result_cap_bytes(Some(10)), 1024, "never below the floor");
}

#[test]
fn the_emergency_budget_covers_every_result_of_the_user_turn() {
    let step = |id: &str| {
        vec![
            assistant_calls(&[id]),
            tool(vec![result(id, &"x".repeat(20_000))]),
        ]
    };
    let mut messages = vec![
        HubMessage::user("old"),
        tool(vec![result("old", &"o".repeat(20_000))]),
    ];
    messages.push(HubMessage::user("go"));
    for id in ["a", "b", "c"] {
        messages.extend(step(id));
    }
    assert_eq!(turn_result_bytes(&messages), 20_000, "the last step only");
    assert_eq!(
        scoped_result_bytes(&messages, ResultScope::UserTurn),
        60_000
    );
    let saved = shorten_scoped_results(&mut messages, 30_000, ResultScope::UserTurn);
    assert!(saved >= 30_000, "{saved}");
    assert!(scoped_result_bytes(&messages, ResultScope::UserTurn) <= 30_100);
    // The earlier turn's result is outside the scope and stays whole.
    assert_eq!(text_of_result(&messages[1]).len(), 20_000);
}

#[test]
fn shortened_text_keeps_head_and_tail_with_a_marker() {
    let text = format!(
        "{}{}{}",
        "a".repeat(5_000),
        "m".repeat(5_000),
        "z".repeat(5_000)
    );
    let short = shorten_text(&text, 2_000);
    assert!(short.len() <= 2_000, "{}", short.len());
    assert!(short.starts_with("aaa") && short.ends_with("zzz"));
    assert!(short.contains("bytes omitted"));
    assert_eq!(shorten_text("small", 100), "small");
    // Multi-byte characters are never split.
    let wide = "é".repeat(3_000);
    assert!(
        shorten_text(&wide, 1_001)
            .chars()
            .all(|c| c == 'é' || !c.is_alphabetic() || c.is_ascii())
    );
}

#[test]
fn three_big_results_on_a_small_model_fit_the_turn() {
    let big = "x".repeat(256 * 1024);
    let each = result_cap_bytes(Some(32_768));
    let capped = |text: &str| cap_text_parts(vec![HubPart::text(text)], each);
    let parts: Vec<HubPart> = ["a", "b", "c"]
        .into_iter()
        .map(|id| HubPart::ToolResult {
            call_id: id.into(),
            name: None,
            content: capped(&big),
            is_error: false,
        })
        .collect();
    let mut messages = vec![tool_message(parts)];
    shorten_turn_results(&mut messages, turn_cap_bytes(Some(32_768)));
    assert!(result_len(&messages[0]) <= 32_768);
    assert!(result_len(&messages[0]) > 0);
}

#[test]
fn the_largest_result_is_shortened_first() {
    let results = vec![
        result("a", &"s".repeat(1_500)),
        result("b", &"l".repeat(20_000)),
        result("c", &"m".repeat(6_000)),
    ];
    let mut messages = vec![tool_message(results)];
    let saved = shorten_turn_results(&mut messages, 10_000);
    assert!(saved > 0);
    let sizes: Vec<usize> = messages[0]
        .content
        .iter()
        .map(|part| match part {
            HubPart::ToolResult { content, .. } => pingdeck_llm::llm::hub::text_of(content).len(),
            _ => 0,
        })
        .collect();
    assert_eq!(sizes[0], 1_500, "the small one is untouched");
    assert!(sizes[1] < 20_000 && sizes[2] <= 6_000);
    assert!(sizes.iter().sum::<usize>() <= 10_100);
    // Within budget: nothing changes.
    assert_eq!(shorten_turn_results(&mut messages, 1_000_000), 0);
}
