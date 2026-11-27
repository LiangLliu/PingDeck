//! Field classes of the gateway: what becomes of every request field
//! when the target speaks another protocol.
//!
//! Single responsibility: the normative table of the PRD ("Field classes") as data, and the two
//! questions the pipeline asks of it. [`refusal`] says before encoding whether the request uses a
//! feature the table refuses toward other protocols (the router then skips the target), and
//! [`drops`] says after encoding which fields the target did not carry, as translation notes the
//! log shows and the `x-spanbay-dropped` header lists. A same-protocol target gets the client's
//! JSON untouched, so it never reaches this code.
//!
//! A field the table does not list is refused toward other protocols: only a field the table
//! names as safe to drop may be left out. Conversation references (`previous_response_id`,
//! `conversation`, a stored `prompt`, Gemini's `cachedContent`) are refused by name, since a
//! model that answers without that context still answers "successfully".
//!
//! The codecs read a few fields and ignore the rest, so "carried" is decided on the *encoded*
//! body: a field is carried when the target's equivalent key is in what the codec produced (a
//! provider whose quirks have no reasoning style loses `thinking`, and the note says so). Nothing
//! the table or the codecs leave out is dropped silently.

use serde_json::Value;

use crate::llm::capture_record::{TranslationKind, TranslationNote};
use crate::llm::hub::HubRequest;
use crate::llm::wire::Protocol;

/// Note of a dropped `cache_control`.
pub(super) const CACHE_NOTE: &str = "prompt cache not available on target, cost may differ";
/// Note of dropped reasoning controls.
pub(super) const REASONING_NOTE: &str = "reasoning not available on target";
/// Note of a dropped `stream_options`: the gateway asks for usage itself.
const STREAM_OPTIONS_NOTE: &str = "the gateway requests usage itself";

/// The client header that is forwarded to a Messages target and dropped toward every other one.
pub(super) const BETA_HEADER: &str = "anthropic-beta";

/// What the table says about one field of one inbound protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Class {
    /// The codecs map it to the target's equivalent (`model`, `messages`, `tools`...).
    Map,
    /// May be omitted, with a note, when the target has no equivalent.
    SafeDrop,
    /// Refused toward other protocols.
    Refuse,
    /// Reasoning controls: mapped, or dropped with [`REASONING_NOTE`].
    Reasoning,
    /// The prompt-cache marker: dropped with [`CACHE_NOTE`] (no codec maps it).
    Cache,
    /// Not in the table: refused toward other protocols, forwarded to the same one.
    Unlisted,
}

/// One row of the table: `field` of the listed inbound protocols.
struct Row {
    field: &'static str,
    protocols: &'static [Protocol],
    class: Class,
}

const CHAT: Protocol = Protocol::OpenaiChat;
const RESP: Protocol = Protocol::OpenaiResponses;
const MSG: Protocol = Protocol::AnthropicMessages;
const GEM: Protocol = Protocol::Gemini;
const ANY: &[Protocol] = &[CHAT, RESP, MSG, GEM];
const NOT_GEM: &[Protocol] = &[CHAT, RESP, MSG];

/// The classification table. The order of the safe-drop rows is the order of the closed safe-drop list, which
/// is the order of `x-spanbay-dropped`.
const TABLE: &[Row] = &[
    // model, messages, system, stream
    Row {
        field: "model",
        protocols: ANY,
        class: Class::Map,
    },
    Row {
        field: "messages",
        protocols: &[CHAT, MSG],
        class: Class::Map,
    },
    Row {
        field: "input",
        protocols: &[RESP],
        class: Class::Map,
    },
    Row {
        field: "instructions",
        protocols: &[RESP],
        class: Class::Map,
    },
    Row {
        field: "system",
        protocols: &[MSG],
        class: Class::Map,
    },
    Row {
        field: "contents",
        protocols: &[GEM],
        class: Class::Map,
    },
    Row {
        field: "systemInstruction",
        protocols: &[GEM],
        class: Class::Map,
    },
    Row {
        field: "system_instruction",
        protocols: &[GEM],
        class: Class::Map,
    },
    Row {
        field: "stream",
        protocols: NOT_GEM,
        class: Class::Map,
    },
    // sampling and limits
    Row {
        field: "max_tokens",
        protocols: &[CHAT, MSG],
        class: Class::Map,
    },
    Row {
        field: "max_completion_tokens",
        protocols: &[CHAT],
        class: Class::Map,
    },
    Row {
        field: "max_output_tokens",
        protocols: &[RESP],
        class: Class::Map,
    },
    Row {
        field: "temperature",
        protocols: NOT_GEM,
        class: Class::Map,
    },
    Row {
        field: "top_p",
        protocols: NOT_GEM,
        class: Class::Map,
    },
    Row {
        field: "stop",
        protocols: &[CHAT],
        class: Class::Map,
    },
    Row {
        field: "stop_sequences",
        protocols: &[MSG],
        class: Class::Map,
    },
    Row {
        field: "generationConfig",
        protocols: &[GEM],
        class: Class::Map,
    },
    Row {
        field: "generation_config",
        protocols: &[GEM],
        class: Class::Map,
    },
    // tools and structured output
    Row {
        field: "tools",
        protocols: ANY,
        class: Class::Map,
    },
    Row {
        field: "tool_choice",
        protocols: NOT_GEM,
        class: Class::Map,
    },
    Row {
        field: "toolConfig",
        protocols: &[GEM],
        class: Class::Map,
    },
    Row {
        field: "tool_config",
        protocols: &[GEM],
        class: Class::Map,
    },
    Row {
        field: "response_format",
        protocols: &[CHAT],
        class: Class::Map,
    },
    Row {
        field: "text",
        protocols: &[RESP],
        class: Class::Map,
    },
    Row {
        field: "output_config",
        protocols: &[MSG],
        class: Class::Map,
    },
    Row {
        field: "web_search_options",
        protocols: &[CHAT],
        class: Class::Map,
    },
    Row {
        field: "n",
        protocols: &[CHAT],
        class: Class::Map,
    },
    // reasoning controls
    Row {
        field: "thinking",
        protocols: &[MSG, CHAT],
        class: Class::Reasoning,
    },
    Row {
        field: "reasoning_effort",
        protocols: &[CHAT],
        class: Class::Reasoning,
    },
    Row {
        field: "reasoning",
        protocols: &[CHAT, RESP],
        class: Class::Reasoning,
    },
    // the closed safe-drop list, in its order
    Row {
        field: "service_tier",
        protocols: NOT_GEM,
        class: Class::SafeDrop,
    },
    Row {
        field: "metadata",
        protocols: NOT_GEM,
        class: Class::SafeDrop,
    },
    Row {
        field: "user",
        protocols: &[CHAT, RESP],
        class: Class::SafeDrop,
    },
    Row {
        field: "store",
        protocols: &[CHAT, RESP],
        class: Class::SafeDrop,
    },
    Row {
        field: "seed",
        protocols: &[CHAT, GEM],
        class: Class::SafeDrop,
    },
    Row {
        field: "logit_bias",
        protocols: &[CHAT],
        class: Class::SafeDrop,
    },
    Row {
        field: "top_k",
        protocols: &[MSG, GEM],
        class: Class::SafeDrop,
    },
    Row {
        field: "stream_options",
        protocols: &[CHAT],
        class: Class::SafeDrop,
    },
    Row {
        field: "parallel_tool_calls",
        protocols: &[CHAT, RESP],
        class: Class::SafeDrop,
    },
    Row {
        field: "presence_penalty",
        protocols: &[CHAT, GEM],
        class: Class::SafeDrop,
    },
    Row {
        field: "frequency_penalty",
        protocols: &[CHAT, GEM],
        class: Class::SafeDrop,
    },
    Row {
        field: "cache_control",
        protocols: &[MSG],
        class: Class::Cache,
    },
    // safe to drop as well: hints, cache keys and output extras that change no answer
    Row {
        field: "include",
        protocols: &[RESP],
        class: Class::SafeDrop,
    },
    Row {
        field: "prompt_cache_key",
        protocols: &[CHAT, RESP],
        class: Class::SafeDrop,
    },
    Row {
        field: "prompt_cache_retention",
        protocols: &[CHAT, RESP],
        class: Class::SafeDrop,
    },
    Row {
        field: "safety_identifier",
        protocols: &[CHAT, RESP],
        class: Class::SafeDrop,
    },
    Row {
        field: "truncation",
        protocols: &[RESP],
        class: Class::SafeDrop,
    },
    Row {
        field: "max_tool_calls",
        protocols: &[RESP],
        class: Class::SafeDrop,
    },
    Row {
        field: "stream_options",
        protocols: &[RESP],
        class: Class::SafeDrop,
    },
    Row {
        field: "top_logprobs",
        protocols: &[CHAT, RESP],
        class: Class::SafeDrop,
    },
    Row {
        field: "logprobs",
        protocols: &[CHAT],
        class: Class::SafeDrop,
    },
    Row {
        field: "verbosity",
        protocols: &[CHAT],
        class: Class::SafeDrop,
    },
    Row {
        field: "modalities",
        protocols: &[CHAT],
        class: Class::SafeDrop,
    },
    Row {
        field: "prediction",
        protocols: &[CHAT],
        class: Class::SafeDrop,
    },
    Row {
        field: "top_k",
        protocols: &[CHAT],
        class: Class::SafeDrop,
    },
    Row {
        field: "repetition_penalty",
        protocols: &[CHAT],
        class: Class::SafeDrop,
    },
    Row {
        field: "min_p",
        protocols: &[CHAT],
        class: Class::SafeDrop,
    },
    Row {
        field: "enable_thinking",
        protocols: &[CHAT],
        class: Class::SafeDrop,
    },
    Row {
        field: "chat_template_kwargs",
        protocols: &[CHAT],
        class: Class::SafeDrop,
    },
    Row {
        field: "safetySettings",
        protocols: &[GEM],
        class: Class::SafeDrop,
    },
    Row {
        field: "safety_settings",
        protocols: &[GEM],
        class: Class::SafeDrop,
    },
    Row {
        field: "labels",
        protocols: &[GEM],
        class: Class::SafeDrop,
    },
    // refused toward other protocols
    Row {
        field: "context_management",
        protocols: &[MSG],
        class: Class::Refuse,
    },
    Row {
        field: "mcp_servers",
        protocols: &[MSG],
        class: Class::Refuse,
    },
    Row {
        field: "container",
        protocols: &[MSG],
        class: Class::Refuse,
    },
    Row {
        field: "previous_response_id",
        protocols: &[RESP],
        class: Class::Refuse,
    },
    Row {
        field: "conversation",
        protocols: &[RESP],
        class: Class::Refuse,
    },
    Row {
        field: "prompt",
        protocols: &[RESP],
        class: Class::Refuse,
    },
    Row {
        field: "background",
        protocols: &[RESP],
        class: Class::Refuse,
    },
    Row {
        field: "cachedContent",
        protocols: &[GEM],
        class: Class::Refuse,
    },
    Row {
        field: "cached_content",
        protocols: &[GEM],
        class: Class::Refuse,
    },
    Row {
        field: "functions",
        protocols: &[CHAT],
        class: Class::Refuse,
    },
    Row {
        field: "function_call",
        protocols: &[CHAT],
        class: Class::Refuse,
    },
    Row {
        field: "audio",
        protocols: &[CHAT],
        class: Class::Refuse,
    },
];

/// What the table says about `field` of a `protocol` request.
#[must_use]
pub(super) fn class_of(protocol: Protocol, field: &str) -> Class {
    TABLE
        .iter()
        .find(|row| row.field == field && row.protocols.contains(&protocol))
        .map_or(Class::Unlisted, |row| row.class)
}

/// The name of a protocol in messages for people.
#[must_use]
pub(super) fn label(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::OpenaiChat => "Chat Completions",
        Protocol::OpenaiResponses => "Responses",
        Protocol::AnthropicMessages => "Messages",
        Protocol::Gemini => "Gemini",
        Protocol::Embeddings => "Embeddings",
        Protocol::DecisionSystemone | Protocol::DecisionOpenai => "Decision",
    }
}

/// The 400 text of a refused field.
#[must_use]
pub(super) fn refusal_message(
    feature: &str,
    inbound: Protocol,
    outbound: Protocol,
    provider: &str,
    model: &str,
) -> String {
    let (from, to) = (label(inbound), label(outbound));
    format!(
        "SpanBay cannot translate {feature} from {from} to {to} ({provider}/{model}). \
         Use a target that speaks {from}, or remove {feature} from the request."
    )
}

// ------------------------------------------------------------------------------------ refusals

/// The feature a codec error is about: the first backticked name (`web_search_20250305`), else a
/// keyword (audio, document, image, web search), else the message itself.
fn feature_of(message: &str) -> String {
    let message = message.trim();
    if let Some(name) = message.split('`').nth(1).filter(|name| !name.is_empty()) {
        return name.to_owned();
    }
    let lower = message.to_ascii_lowercase();
    for (needle, feature) in [
        ("audio", "audio"),
        ("document", "document"),
        ("web search", "web_search"),
        ("web_search", "web_search"),
        ("image", "image"),
    ] {
        if lower.contains(needle) {
            return feature.to_owned();
        }
    }
    message.to_owned()
}

/// The feature behind a codec's [`AppError::Unsupported`](spanbay_base::error::AppError) message.
#[must_use]
pub(super) fn feature_of_error(message: &str) -> String {
    feature_of(message)
}

/// The feature the table refuses toward every other protocol that `raw` uses: a refused field
/// (`context_management`, the MCP connector, a conversation reference), a field the table does
/// not list, `n > 1`. `None` when the request is clean; the codecs' own errors (provider tools,
/// documents and audio the target lacks) are the other refusal source.
#[must_use]
pub(super) fn refusal(inbound: Protocol, raw: &Value) -> Option<String> {
    let object = raw.as_object()?;
    let set = |field: &str| object.get(field).is_some_and(|value| !value.is_null());
    for row in TABLE
        .iter()
        .filter(|row| row.class == Class::Refuse && row.protocols.contains(&inbound))
    {
        if set(row.field) {
            return Some(
                if row.field == "mcp_servers" {
                    "MCP connector"
                } else {
                    row.field
                }
                .to_owned(),
            );
        }
    }
    if let Some(unlisted) = object
        .keys()
        .find(|key| class_of(inbound, key) == Class::Unlisted && set(key))
    {
        return Some(unlisted.clone());
    }
    let many = |value: Option<&Value>| value.and_then(Value::as_u64).is_some_and(|n| n > 1);
    let candidates = if inbound == GEM {
        object
            .get("generationConfig")
            .or_else(|| object.get("generation_config"))
            .and_then(|config| {
                config
                    .get("candidateCount")
                    .or_else(|| config.get("candidate_count"))
            })
    } else {
        object.get("n")
    };
    many(candidates).then(|| "n > 1".to_owned())
}

// --------------------------------------------------------------------------------------- drops

/// JSON pointers into an encoded body of `outbound` where the equivalent of `field` would be.
/// Empty: the protocol has none.
fn carriers(field: &str, outbound: Protocol) -> &'static [&'static str] {
    match (field, outbound) {
        ("top_k", MSG) => &["/top_k"],
        ("top_k", GEM) => &["/generationConfig/topK"],
        ("seed", CHAT) => &["/seed"],
        ("seed", GEM) => &["/generationConfig/seed"],
        ("parallel_tool_calls", CHAT | RESP) => &["/parallel_tool_calls"],
        ("parallel_tool_calls", MSG) => &["/tool_choice/disable_parallel_tool_use"],
        ("user" | "metadata", CHAT | RESP) => &["/user"],
        ("user" | "metadata", MSG) => &["/metadata/user_id"],
        ("presence_penalty", CHAT) => &["/presence_penalty"],
        ("presence_penalty", GEM) => &["/generationConfig/presencePenalty"],
        ("frequency_penalty", CHAT) => &["/frequency_penalty"],
        ("frequency_penalty", GEM) => &["/generationConfig/frequencyPenalty"],
        ("prompt_cache_key", CHAT | RESP) => &["/prompt_cache_key"],
        _ => &[],
    }
}

/// Removes from an encoded body what the table says the target does not take although its codec
/// writes it: Chat Completions has no `top_k`, the PRD's example `top_k,cache_control`
/// from Messages to Chat. Run before [`drops`], so the note and the upstream body agree.
pub(super) fn conform(outbound: Protocol, encoded: &mut Value) {
    if outbound == CHAT
        && let Some(object) = encoded.as_object_mut()
    {
        object.remove("top_k");
    }
}

/// Mapped fields a codec may still leave out: a provider that refuses sampling, a model generation
/// that refuses it, a target without stop sequences.
const MAPPED: [&str; 4] = ["temperature", "top_p", "stop", "stop_sequences"];

/// Where the encoded body of `outbound` carries a [`MAPPED`] field.
fn mapped_carriers(field: &str, outbound: Protocol) -> &'static [&'static str] {
    match (field, outbound) {
        ("temperature", CHAT | RESP | MSG) => &["/temperature"],
        ("temperature", GEM) => &["/generationConfig/temperature"],
        ("top_p", CHAT | RESP | MSG) => &["/top_p"],
        ("top_p", GEM) => &["/generationConfig/topP"],
        ("stop" | "stop_sequences", CHAT) => &["/stop"],
        ("stop" | "stop_sequences", MSG) => &["/stop_sequences"],
        ("stop" | "stop_sequences", GEM) => &["/generationConfig/stopSequences"],
        _ => &[],
    }
}

/// Where the encoded body of `outbound` carries a reasoning request.
fn reasoning_carriers(outbound: Protocol) -> &'static [&'static str] {
    match outbound {
        MSG => &["/thinking", "/output_config/effort"],
        CHAT => &[
            "/reasoning_effort",
            "/reasoning",
            "/thinking",
            "/enable_thinking",
            "/chat_template_kwargs",
        ],
        RESP => &["/reasoning"],
        GEM => &["/generationConfig/thinkingConfig"],
        _ => &[],
    }
}

fn carried(encoded: &Value, pointers: &[&str]) -> bool {
    pointers.iter().any(|pointer| {
        encoded
            .pointer(pointer)
            .is_some_and(|value| !value.is_null())
    })
}

/// Whether `object` (a client request) sets `field`; Gemini keeps its sampling options inside
/// `generationConfig`.
fn present(inbound: Protocol, object: &serde_json::Map<String, Value>, field: &str) -> bool {
    let set = |value: Option<&Value>| value.is_some_and(|value| !value.is_null());
    if set(object.get(field)) {
        return true;
    }
    inbound == GEM
        && matches!(
            field,
            "top_k"
                | "seed"
                | "presence_penalty"
                | "frequency_penalty"
                | "temperature"
                | "top_p"
                | "stop"
        )
        && object
            .get("generationConfig")
            .or_else(|| object.get("generation_config"))
            .is_some_and(|config| {
                let (camel, snake) = match field {
                    "top_k" => ("topK", "top_k"),
                    "seed" => ("seed", "seed"),
                    "temperature" => ("temperature", "temperature"),
                    "top_p" => ("topP", "top_p"),
                    "stop" => ("stopSequences", "stop_sequences"),
                    "presence_penalty" => ("presencePenalty", "presence_penalty"),
                    _ => ("frequencyPenalty", "frequency_penalty"),
                };
                set(config.get(camel)) || set(config.get(snake))
            })
}

/// Whether the client's `metadata` is only what the codecs map to `user` (Messages `user_id`).
fn metadata_is_user_only(inbound: Protocol, object: &serde_json::Map<String, Value>) -> bool {
    inbound == MSG
        && object
            .get("metadata")
            .and_then(Value::as_object)
            .is_some_and(|metadata| metadata.keys().all(|key| key == "user_id"))
}

/// Whether the key `needle` occurs anywhere in `value` (`cache_control` sits on content blocks,
/// tools and the request itself).
fn has_key(value: &Value, needle: &str) -> bool {
    match value {
        Value::Object(map) => map.contains_key(needle) || map.values().any(|v| has_key(v, needle)),
        Value::Array(items) => items.iter().any(|item| has_key(item, needle)),
        _ => false,
    }
}

fn dropped(field: &str, reason: impl Into<String>) -> TranslationNote {
    TranslationNote {
        field: field.to_owned(),
        kind: TranslationKind::Dropped,
        reason: reason.into(),
    }
}

/// The fields of the client's request that the `outbound` body did not carry, as notes, in the
/// order of the safe-drop list, then `cache_control`, reasoning, and last the `anthropic-beta`
/// header when the client sent one (unlisted fields never get here: [`refusal`] refuses them).
/// `raw` is the client body, `request` its hub decode and `encoded` what the codec produced for
/// the target.
#[must_use]
pub(super) fn drops(
    inbound: Protocol,
    outbound: Protocol,
    raw: &Value,
    has_beta_header: bool,
    request: &HubRequest,
    encoded: &Value,
) -> Vec<TranslationNote> {
    let mut notes = Vec::new();
    let Some(object) = raw.as_object() else {
        return notes;
    };
    let target = label(outbound);
    let no_equivalent = || format!("no equivalent in {target}");
    for row in TABLE
        .iter()
        .filter(|row| row.class == Class::SafeDrop && row.protocols.contains(&inbound))
    {
        if !present(inbound, object, row.field) {
            continue;
        }
        let mapped = carried(encoded, carriers(row.field, outbound))
            && (row.field != "metadata" || metadata_is_user_only(inbound, object));
        if mapped {
            continue;
        }
        let reason = if row.field == "stream_options" {
            STREAM_OPTIONS_NOTE.to_owned()
        } else {
            no_equivalent()
        };
        notes.push(dropped(row.field, reason));
    }
    // `cache_control` is a field of the request, of tools and of content blocks; no codec maps it.
    if has_key(raw, "cache_control") {
        notes.push(dropped("cache_control", CACHE_NOTE));
    }
    let asked = request.reasoning.as_ref().is_some_and(|reasoning| {
        reasoning.enabled != Some(false)
            || reasoning.effort.is_some()
            || reasoning.budget_tokens.is_some()
    });
    if asked && !carried(encoded, reasoning_carriers(outbound)) {
        let field = TABLE
            .iter()
            .find(|row| {
                row.class == Class::Reasoning
                    && row.protocols.contains(&inbound)
                    && object.contains_key(row.field)
            })
            .map_or("reasoning", |row| row.field);
        notes.push(dropped(field, REASONING_NOTE));
    }
    for field in MAPPED {
        if present(inbound, object, field) && !carried(encoded, mapped_carriers(field, outbound)) {
            notes.push(dropped(field, format!("not sent to {target}")));
        }
    }
    if has_beta_header {
        notes.push(dropped(BETA_HEADER, no_equivalent()));
    }
    notes
}

/// The field names of `notes` for `x-spanbay-dropped`, in order, each once.
#[must_use]
pub(super) fn dropped_header(notes: &[TranslationNote]) -> Option<String> {
    let mut names: Vec<&str> = Vec::new();
    for note in notes
        .iter()
        .filter(|note| note.kind == TranslationKind::Dropped)
    {
        if !names.contains(&note.field.as_str()) {
            names.push(&note.field);
        }
    }
    (!names.is_empty()).then(|| names.join(","))
}

#[cfg(test)]
#[path = "fields_tests.rs"]
mod tests;
