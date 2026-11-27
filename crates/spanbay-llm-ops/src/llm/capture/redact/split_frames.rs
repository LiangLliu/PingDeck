//! Redaction of secrets split across stream frames.
//!
//! A key streamed as `sk-ab` + `c123…` is in no single frame, so per-frame redaction misses it.
//! The text-bearing string fields of the frames (`content`, `text`, `reasoning_content`,
//! `arguments`, `partial_json`, `delta`, …) are joined per field name in stream order, the joined
//! text is redacted, and every change is mapped back onto the frames it spans. Fail closed: when
//! the mapping cannot be verified, the whole field is blanked.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::ops::Range;

use serde_json::Value;

use super::{redact_text, saturating_u32};
use crate::llm::capture::model::CapturedFrame;
use spanbay_base::secrets::{ELIDED, REDACTED};

/// String fields that carry streamed text across the four chat protocols.
const TEXT_KEYS: [&str; 8] = [
    "content",
    "text",
    "reasoning_content",
    "reasoning",
    "thinking",
    "arguments",
    "partial_json",
    "delta",
];

/// The channel of a frame whose whole `data` is the text (not JSON); sorts before every key.
const WHOLE_CHANNEL: &str = "";

/// Where a string sits in its frame.
enum Place {
    /// The frame's whole `data` (not JSON).
    Whole,
    /// A JSON pointer into the frame's parsed `data`.
    Pointer(String),
}

struct Leaf {
    frame: usize,
    place: Place,
    text: String,
}

/// A region of the original text that redaction replaced, and with what.
struct Span {
    range: Range<usize>,
    token: &'static str,
}

/// One step of the path from a frame's root value to a string.
#[derive(Clone, Copy)]
enum Seg<'a> {
    Key(&'a str),
    Index(usize),
}

/// The text-bearing strings of every frame: the parsed frames (`None` when not JSON), the
/// leaves in stream order and, per channel (field name), the indices of its leaves.
struct Leaves {
    parsed: Vec<Option<Value>>,
    leaves: Vec<Leaf>,
    channels: BTreeMap<&'static str, Vec<usize>>,
}

/// Removes every secret that spans two or more frames from the frames' text.
pub(super) fn redact_split_frames(
    frames: &mut [CapturedFrame],
    needles: &[String],
    removed: &mut u32,
) {
    let Leaves {
        mut parsed,
        mut leaves,
        channels,
    } = collect_leaves(frames);
    let mut dirty = vec![false; frames.len()];
    for members in channels.values() {
        // Leaves are pushed in frame order, so the first and last member bound the frames spanned.
        let (Some(&first), Some(&last)) = (members.first(), members.last()) else {
            continue;
        };
        if leaves[first].frame == leaves[last].frame {
            continue;
        }
        let texts: Vec<&str> = members
            .iter()
            .map(|&leaf| leaves[leaf].text.as_str())
            .collect();
        let Some((new_texts, count)) = redact_channel(&texts, needles) else {
            continue;
        };
        *removed = removed.saturating_add(count);
        apply_to_leaves(&mut leaves, members, new_texts, &mut dirty);
    }
    write_back(frames, &mut parsed, &leaves, &dirty);
}

/// Parses every frame and collects its text-bearing strings.
fn collect_leaves(frames: &[CapturedFrame]) -> Leaves {
    let parsed: Vec<Option<Value>> = frames
        .iter()
        .map(|frame| serde_json::from_str::<Value>(&frame.data).ok())
        .collect();
    let mut leaves: Vec<Leaf> = Vec::new();
    let mut channels: BTreeMap<&'static str, Vec<usize>> = BTreeMap::new();
    for (frame_index, frame) in frames.iter().enumerate() {
        let mut found: Vec<(&'static str, String, String)> = Vec::new();
        match &parsed[frame_index] {
            Some(value) => collect(value, &mut Vec::new(), None, &mut found),
            None => {
                let data = frame.data.trim();
                if !data.is_empty() && data != "[DONE]" {
                    leaves.push(Leaf {
                        frame: frame_index,
                        place: Place::Whole,
                        text: frame.data.clone(),
                    });
                    channels
                        .entry(WHOLE_CHANNEL)
                        .or_default()
                        .push(leaves.len() - 1);
                }
            }
        }
        for (channel, pointer, text) in found {
            leaves.push(Leaf {
                frame: frame_index,
                place: Place::Pointer(pointer),
                text,
            });
            channels.entry(channel).or_default().push(leaves.len() - 1);
        }
    }
    Leaves {
        parsed,
        leaves,
        channels,
    }
}

/// Collects `(channel, pointer, text)` for every text-bearing string of `value`; `path` leads
/// from the frame's root to `value`, `key` is the nearest enclosing object key.
fn collect<'a>(
    value: &'a Value,
    path: &mut Vec<Seg<'a>>,
    key: Option<&'static str>,
    out: &mut Vec<(&'static str, String, String)>,
) {
    match value {
        Value::String(text) => {
            if let Some(key) = key {
                out.push((key, pointer(path), text.clone()));
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                path.push(Seg::Index(index));
                collect(item, path, key, out);
                path.pop();
            }
        }
        Value::Object(members) => {
            for (name, member) in members {
                let text_key = TEXT_KEYS.iter().copied().find(|text_key| text_key == name);
                path.push(Seg::Key(name));
                collect(member, path, text_key, out);
                path.pop();
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// The JSON pointer (RFC 6901: `~` as `~0`, `/` as `~1`) of `path`.
fn pointer(path: &[Seg<'_>]) -> String {
    let mut out = String::new();
    for seg in path {
        out.push('/');
        match seg {
            Seg::Key(name) => out.push_str(&name.replace('~', "~0").replace('/', "~1")),
            Seg::Index(index) => {
                let _ = write!(out, "{index}");
            }
        }
    }
    out
}

/// The redacted texts of one channel and how many values were removed, `None` when redacting the
/// joined text changes nothing. Each replaced region of the joined text counts once, however many
/// texts it spans. When the changes cannot be mapped back onto the texts verifiably, every
/// non-empty text becomes the marker (fail closed) and counts once.
fn redact_channel(texts: &[&str], needles: &[String]) -> Option<(Vec<String>, u32)> {
    let joined: String = texts.concat();
    let (redacted, _) = redact_text(&joined, needles);
    if redacted == joined {
        return None;
    }
    let replaced = changed_spans(&joined, &redacted).and_then(|spans| {
        let new_texts = apply_spans(texts, &spans);
        let rejoined: String = new_texts.concat();
        let verified =
            collapse_markers(&redact_text(&rejoined, needles).0) == collapse_markers(&rejoined);
        verified.then(|| (new_texts, saturating_u32(spans.len())))
    });
    Some(replaced.unwrap_or_else(|| blank(texts)))
}

/// Every non-empty text replaced by the marker, and how many were.
fn blank(texts: &[&str]) -> (Vec<String>, u32) {
    let mut count = 0;
    let blanked = texts
        .iter()
        .map(|text| {
            if text.is_empty() {
                String::new()
            } else {
                count += 1;
                REDACTED.to_owned()
            }
        })
        .collect();
    (blanked, count)
}

/// Stores the new texts of a channel's `members` and marks the frames whose text changed.
fn apply_to_leaves(
    leaves: &mut [Leaf],
    members: &[usize],
    new_texts: Vec<String>,
    dirty: &mut [bool],
) {
    for (&leaf, text) in members.iter().zip(new_texts) {
        if leaves[leaf].text == text {
            continue;
        }
        leaves[leaf].text = text;
        dirty[leaves[leaf].frame] = true;
    }
}

/// Writes the leaves of every changed frame back into its `data`.
fn write_back(
    frames: &mut [CapturedFrame],
    parsed: &mut [Option<Value>],
    leaves: &[Leaf],
    dirty: &[bool],
) {
    for leaf in leaves {
        if !dirty[leaf.frame] {
            continue;
        }
        match &leaf.place {
            Place::Whole => frames[leaf.frame].data.clone_from(&leaf.text),
            Place::Pointer(pointer) => {
                if let Some(Value::String(slot)) = parsed[leaf.frame]
                    .as_mut()
                    .and_then(|value| value.pointer_mut(pointer))
                {
                    slot.clone_from(&leaf.text);
                }
            }
        }
    }
    for (index, frame) in frames.iter_mut().enumerate() {
        if let (true, Some(value)) = (dirty[index], &parsed[index]) {
            frame.data = value.to_string();
        }
    }
}

/// `text` with every run of adjacent redaction markers reduced to one (a secret that spans two
/// frames leaves one marker in each, which redaction would merge when the frames are joined).
fn collapse_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(REDACTED) {
        out.push_str(&rest[..at]);
        out.push_str(REDACTED);
        rest = &rest[at..];
        while let Some(next) = rest.strip_prefix(REDACTED) {
            rest = next;
        }
    }
    out.push_str(rest);
    out
}

fn next_token(text: &str) -> Option<(usize, &'static str)> {
    [REDACTED, ELIDED]
        .into_iter()
        .filter_map(|token| text.find(token).map(|at| (at, token)))
        .min_by_key(|(at, _)| *at)
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars()
        .zip(b.chars())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum()
}

/// The regions of `original` that `redacted` replaced by a marker. `None` when the two cannot be
/// aligned unambiguously, or the alignment does not reproduce `redacted`.
fn changed_spans(original: &str, redacted: &str) -> Option<Vec<Span>> {
    let mut spans: Vec<Span> = Vec::new();
    let (mut i, mut j) = (0, 0);
    loop {
        let common = common_prefix_len(&original[i..], &redacted[j..]);
        i += common;
        j += common;
        if j == redacted.len() {
            if i != original.len() {
                return None;
            }
            break;
        }
        let token = [REDACTED, ELIDED]
            .into_iter()
            .find(|token| redacted[j..].starts_with(token))?;
        let after = j + token.len();
        let anchor_end =
            next_token(&redacted[after..]).map_or(redacted.len(), |(at, _)| after + at);
        let anchor = &redacted[after..anchor_end];
        let end = if anchor.is_empty() {
            if after != redacted.len() {
                return None;
            }
            original.len()
        } else {
            i + original[i..].find(anchor)?
        };
        spans.push(Span {
            range: i..end,
            token,
        });
        i = end;
        j = after;
    }
    reproduces(original, &spans, redacted).then_some(spans)
}

/// Whether replacing `spans` in `original` by their tokens gives exactly `redacted`.
fn reproduces(original: &str, spans: &[Span], redacted: &str) -> bool {
    let mut rebuilt = String::with_capacity(redacted.len());
    let mut at = 0;
    for span in spans {
        rebuilt.push_str(&original[at..span.range.start]);
        rebuilt.push_str(span.token);
        at = span.range.end;
    }
    rebuilt.push_str(&original[at..]);
    rebuilt == redacted
}

/// `texts` (concatenated, they are the original) with every span replaced inside the texts it
/// overlaps: each overlapped text gets its own marker for its part of the span.
fn apply_spans(texts: &[&str], spans: &[Span]) -> Vec<String> {
    let mut offset = 0;
    texts
        .iter()
        .map(|text| {
            let (start, end) = (offset, offset + text.len());
            offset = end;
            let mut out = String::with_capacity(text.len());
            let mut at = start;
            for span in spans
                .iter()
                .filter(|span| span.range.start < end && span.range.end > start)
            {
                let from = span.range.start.max(start);
                let to = span.range.end.min(end);
                out.push_str(&text[at - start..from - start]);
                out.push_str(span.token);
                at = to;
            }
            out.push_str(&text[at - start..]);
            out
        })
        .collect()
}
