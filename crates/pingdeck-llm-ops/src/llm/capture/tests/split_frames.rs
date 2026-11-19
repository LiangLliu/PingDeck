//! Secrets split across stream frames: removed from every frame they span, the text around them
//! kept, and a seeded property test.

use serde_json::json;

use super::{Rng, chat_data, contents, datas, frames_of, random_secret, raw, redact_client_frames};
use crate::llm::capture::{
    CapturedFrame, CapturedFrames, MergedSource, merged_reply, redact_exchange,
};
use crate::llm::wire::Protocol;
use pingdeck_base::secrets::REDACTED;

#[test]
fn a_key_split_across_two_frames_is_gone_from_both_and_from_the_merged_reply() {
    let stored = redact_client_frames(
        frames_of(&["Hello sk-", "abcdefghijklmnopqrstuvwx", "yz0123 bye"]),
        &[],
    );
    let texts = contents(&stored);
    let joined = texts.concat();
    assert!(
        !joined.contains("sk-") && !joined.contains("abcdefgh") && !joined.contains("yz0123"),
        "{texts:?}"
    );
    assert!(texts[0].starts_with("Hello "));
    assert_eq!(
        datas(&stored),
        [
            chat_data(&format!("Hello {REDACTED}")),
            chat_data(REDACTED),
            chat_data(&format!("{REDACTED} bye")),
        ]
    );
    let all: String = stored.frames.iter().map(|f| f.data.as_str()).collect();
    assert!(!all.contains("abcdefgh"));
    let reply = merged_reply(Protocol::OpenaiChat, MergedSource::Frames(&stored));
    assert!(
        !reply.text.contains("sk-") && !reply.text.contains("abcdefgh"),
        "{}",
        reply.text
    );
    assert!(reply.text.starts_with("Hello "));
}

#[test]
fn text_around_a_split_secret_is_kept() {
    let stored = redact_client_frames(
        frames_of(&["Hello sk-", "abcdefghij", "klmnopqrstuv", " bye"]),
        &[],
    );
    let texts = contents(&stored);
    assert_eq!(texts[0], format!("Hello {REDACTED}"));
    assert_eq!(texts[1], REDACTED);
    assert_eq!(texts[2], REDACTED);
    assert_eq!(texts[3], " bye");
    assert_eq!(
        datas(&stored),
        [
            chat_data(&format!("Hello {REDACTED}")),
            chat_data(REDACTED),
            chat_data(REDACTED),
            chat_data(" bye"),
        ]
    );
}

#[test]
fn a_split_exact_value_needle_is_removed_from_every_frame() {
    let needles = vec!["relay-9f8e7d6c5b4a".to_owned()];
    let stored = redact_client_frames(frames_of(&["use relay-9f", "8e7d6c", "5b4a now"]), &needles);
    let texts = contents(&stored);
    let all = texts.concat();
    assert!(
        !all.contains("relay-") && !all.contains("8e7d6c") && !all.contains("5b4a"),
        "{texts:?}"
    );
    assert!(texts[2].ends_with(" now"));
    assert_eq!(
        datas(&stored),
        [
            chat_data(&format!("use {REDACTED}")),
            chat_data(REDACTED),
            chat_data(&format!("{REDACTED} now")),
        ]
    );
}

#[test]
fn a_split_json_credential_in_tool_call_arguments_is_removed() {
    let frame = |index: u32, partial: &str| {
        CapturedFrame {
        index,
        t_ms: 0,
        event: "content_block_delta".to_owned(),
        data: json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": partial}}).to_string(),
    }
    };
    let frames = CapturedFrames {
        frames: vec![
            frame(1, "{\"city\":\"Oslo\",\"password\":\"hun"),
            frame(2, "ter2-long\"}"),
        ],
        total_frames: 2,
        truncated: false,
    };
    let mut exchange = raw();
    exchange.upstream_frames = Some(frames);
    let stored = redact_exchange(exchange, &[]).0.upstream_frames.unwrap();
    let all: String = stored.frames.iter().map(|f| f.data.as_str()).collect();
    assert!(!all.contains("hun") && !all.contains("ter2"), "{all}");
    assert!(all.contains("Oslo"));
    let partial = |text: &str| {
        json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": text}}).to_string()
    };
    assert_eq!(
        datas(&stored),
        [
            partial(&format!("{{\"city\":\"Oslo\",\"password\":\"{REDACTED}")),
            partial(&format!("{REDACTED}\"}}")),
        ]
    );
}

#[test]
fn split_non_json_frames_are_covered_as_raw_text() {
    let frame = |index: u32, data: &str| CapturedFrame {
        index,
        t_ms: 0,
        event: "message".to_owned(),
        data: data.to_owned(),
    };
    let frames = CapturedFrames {
        frames: vec![
            frame(1, "key is sk-abcdefgh"),
            frame(2, "ijklmnopqrstuvwxyz0123"),
            frame(3, "[DONE]"),
        ],
        total_frames: 3,
        truncated: false,
    };
    let stored = redact_client_frames(frames, &[]);
    let all: String = stored.frames.iter().map(|f| f.data.as_str()).collect();
    assert!(
        !all.contains("abcdefgh") && !all.contains("ijklmnop"),
        "{all}"
    );
    assert_eq!(stored.frames[2].data, "[DONE]");
    assert_eq!(
        datas(&stored),
        [
            format!("key is {REDACTED}"),
            REDACTED.to_owned(),
            "[DONE]".to_owned()
        ]
    );
}

#[test]
fn two_split_secrets_in_one_channel_count_twice() {
    // Regression guard (a channel counted 1 however many secrets it held).
    let mut exchange = raw();
    exchange.client_frames = Some(frames_of(&[
        "a sk-abcdefgh",
        "ijklmnopqrstuvwxyz0123 and sk-zyxwvuts",
        "rqponmlkjihgfedcba98 end",
    ]));
    let (stored, count) = redact_exchange(exchange, &[]);
    assert_eq!(count, 2);
    assert_eq!(stored.redacted_count, 2);
    assert_eq!(
        datas(stored.client_frames.as_ref().unwrap()),
        [
            chat_data(&format!("a {REDACTED}")),
            chat_data(&format!("{REDACTED} and {REDACTED}")),
            chat_data(&format!("{REDACTED} end")),
        ]
    );
}

#[test]
fn an_unverifiable_split_mapping_blanks_every_part_of_the_channel() {
    // The text after the needle ("-x") also occurs inside it, so the alignment fails and the
    // fail-closed fallback blanks each non-empty part, counting one per blanked part.
    // Regression guard (the fallback counted 1).
    let mut exchange = raw();
    exchange.client_frames = Some(frames_of(&["see abc-x-", "defghij-x"]));
    let (stored, count) = redact_exchange(exchange, &["abc-x-defghij".to_owned()]);
    assert_eq!(count, 2);
    assert_eq!(
        datas(stored.client_frames.as_ref().unwrap()),
        [chat_data(REDACTED), chat_data(REDACTED)]
    );
}

#[test]
fn property_secrets_split_across_frames_never_survive() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for round in 0..300 {
        let needle = format!("relay-{}", rng.chars("0123456789abcdef", 12));
        let secret = random_secret(&mut rng, &needle, false);
        // Cut the secret into 2..=4 pieces at random character positions.
        let pieces = 2 + rng.below(3);
        let mut cuts: Vec<usize> = (0..pieces - 1)
            .map(|_| 1 + rng.below(secret.len() - 1))
            .collect();
        cuts.sort_unstable();
        cuts.dedup();
        let mut parts: Vec<String> = Vec::new();
        let mut at = 0;
        for cut in cuts {
            parts.push(secret[at..cut].to_owned());
            at = cut;
        }
        parts.push(secret[at..].to_owned());
        let mut texts = vec!["see ".to_owned()];
        texts.extend(parts.iter().cloned());
        texts.push(" ok".to_owned());
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let stored = redact_client_frames(frames_of(&refs), std::slice::from_ref(&needle));
        let joined = contents(&stored).concat();
        assert!(
            !joined.contains(&secret),
            "round {round}: {secret} survived in {joined}"
        );
        let all: String = stored
            .frames
            .iter()
            .map(|f| f.data.as_str())
            .collect::<Vec<_>>()
            .join("|");
        for part in parts.iter().filter(|part| part.len() >= 6) {
            assert!(
                !all.contains(part.as_str()),
                "round {round}: piece {part} of {secret} survived in {all}"
            );
        }
        assert!(
            joined.starts_with("see ") && joined.ends_with(" ok"),
            "round {round}: {joined}"
        );
    }
}

#[test]
fn a_split_secret_counts_like_the_same_secret_in_one_frame() {
    // Regression for capture B6: the count follows the secrets, not the channels.
    let text = "a sk-abcdefghijklmnopqrstuvwxyz0123 and sk-zyxwvutsrqponmlkjihgfedcba98 end";
    let mut whole = raw();
    whole.client_frames = Some(frames_of(&[text]));
    let mut split = raw();
    split.client_frames = Some(frames_of(&[&text[..13], &text[13..50], &text[50..]]));
    let whole = redact_exchange(whole, &[]).1;
    let split = redact_exchange(split, &[]).1;
    assert_eq!(whole, 2);
    assert_eq!(split, whole);
}
