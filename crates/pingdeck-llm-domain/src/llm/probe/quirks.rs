//! What the Providers compatibility panel shows beside each flag: where its value comes from and
//! what the last probe suggests.
//!
//! Single responsibility: two pure derivations over [`Quirks`] and stored [`ProbeResult`]s, no
//! store access and no I/O. [`quirk_sources`] labels each flag `preset`, `user` or `default`;
//! [`quirk_suggestions`] turns the provider error text of the newest failed (or unmetered) probe
//! into flag values worth trying. A suggestion never changes anything by itself.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ProbeResult, ProbeTest};
use crate::llm::wire::{MaxTokensField, Quirks};

/// Where a flag's value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuirkSource {
    /// The value equals the one the provider's preset sets.
    Preset,
    /// The user set a value the preset does not (or a different one).
    User,
    /// Nothing is set: the codec's own behaviour (Auto) applies.
    Default,
}

/// A flag value the last probe suggests, with the reason shown to the user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuirkSuggestion {
    /// The suggested value as the flag serializes (`true`, `"max_completion_tokens"`).
    pub value: Value,
    /// One sentence naming the evidence.
    pub note: String,
}

/// Source of one flag given its value and the preset's value (`None` is unset, Auto).
fn source<T: PartialEq>(value: Option<T>, preset: Option<T>) -> QuirkSource {
    match value {
        None => QuirkSource::Default,
        Some(value) if preset.as_ref() == Some(&value) => QuirkSource::Preset,
        Some(_) => QuirkSource::User,
    }
}

/// The source of every compatibility flag of `quirks`, keyed by the flag's serialized name.
/// `preset` is the quirks of the provider's preset (`None`: no preset, so a set flag is the
/// user's). `extra_headers` and `list_in_models` are not flags of the panel and have none.
#[must_use]
pub fn quirk_sources(quirks: &Quirks, preset: Option<&Quirks>) -> BTreeMap<String, QuirkSource> {
    // `Quirks` renames no field, so the field name is the serialized flag name.
    macro_rules! sources {
        ($quirks:ident, $preset:ident; $($field:ident),+ $(,)?) => {
            BTreeMap::from([$((
                stringify!($field).to_owned(),
                source($quirks.$field.as_ref(), $preset.$field.as_ref()),
            )),+])
        };
    }
    let none = Quirks::default();
    let preset = preset.unwrap_or(&none);
    sources!(
        quirks,
        preset;
        reasoning_request,
        developer_role_as_system,
        stream_usage,
        max_tokens_field,
        drop_sampling_params,
        echo_reasoning_in_tool_turns,
        no_done_sentinel,
        anthropic_version,
        reasoning_response_field,
        tools_via_responses,
    )
}

/// The suggestion key of the output cap field.
const MAX_TOKENS_FIELD: &str = "max_tokens_field";
/// The suggestion key of sending `developer` instructions as `system`.
const DEVELOPER_ROLE_AS_SYSTEM: &str = "developer_role_as_system";
/// The suggestion key of asking for usage in streams.
const STREAM_USAGE: &str = "stream_usage";

/// Tests whose HTTP 400 can name a request field the provider rejects (the chat family).
const CHAT_TESTS: [ProbeTest; 6] = [
    ProbeTest::Chat,
    ProbeTest::Stream,
    ProbeTest::Tools,
    ProbeTest::JsonSchema,
    ProbeTest::Vision,
    ProbeTest::Reasoning,
];

/// Words of an error that say a field is not accepted at all (as opposed to a bad value).
const REJECTION_WORDS: [&str; 6] = [
    "unrecognized",
    "unknown parameter",
    "unknown field",
    "unexpected",
    "extra inputs",
    "not permitted",
];

/// The newest result of each test (by `checked_at_ms`; the later of equal ones wins).
fn newest_by_test(results: &[ProbeResult]) -> BTreeMap<&str, &ProbeResult> {
    let mut newest: BTreeMap<&str, &ProbeResult> = BTreeMap::new();
    for result in results.iter().filter(|result| result.skip_reason.is_none()) {
        let slot = newest.entry(result.test.as_str()).or_insert(result);
        if result.checked_at_ms >= slot.checked_at_ms {
            *slot = result;
        }
    }
    newest
}

/// The lower-cased error text of a result: its message and any detail the check kept.
fn error_text(result: &ProbeResult) -> String {
    let mut text = result.message.to_lowercase();
    if !result.detail.is_null() {
        text.push(' ');
        text.push_str(&result.detail.to_string().to_lowercase());
    }
    text
}

/// `max_tokens_field` from the error text of a failed chat-family request.
fn max_tokens_field_suggestion(text: &str) -> Option<QuirkSuggestion> {
    if !text.contains("max_completion_tokens") {
        return None;
    }
    let rejected =
        !text.contains("max_tokens") && REJECTION_WORDS.iter().any(|word| text.contains(word));
    let (field, note) = if rejected {
        (
            MaxTokensField::MaxTokens,
            "The provider rejected `max_completion_tokens`; send `max_tokens`.",
        )
    } else {
        (
            MaxTokensField::MaxCompletionTokens,
            "The provider asks for `max_completion_tokens` instead of `max_tokens`.",
        )
    };
    Some(QuirkSuggestion {
        value: json!(field),
        note: note.to_owned(),
    })
}

/// `developer_role_as_system` from the error text of a failed chat-family request.
fn developer_role_suggestion(text: &str) -> Option<QuirkSuggestion> {
    (text.contains("developer") && text.contains("role")).then(|| QuirkSuggestion {
        value: json!(true),
        note: "The provider rejected the `developer` role; send instructions as `system`."
            .to_owned(),
    })
}

/// `stream_usage` when the newest stream test passed without any token count.
fn stream_usage_suggestion(newest: &BTreeMap<&str, &ProbeResult>) -> Option<QuirkSuggestion> {
    newest
        .get(ProbeTest::Stream.as_str())
        .is_some_and(|result| {
            result.ok && result.input_tokens.is_none() && result.output_tokens.is_none()
        })
        .then(|| QuirkSuggestion {
            value: json!(true),
            note: "The stream test reported no token usage; ask for it in the stream.".to_owned(),
        })
}

/// Flag values the last probe suggests, keyed by flag name. Only the newest result of each test
/// counts, so a later pass clears a suggestion; skipped cells are ignored. The rules:
///
/// - `max_tokens_field`: a chat-family test that failed with HTTP 400 naming
///   `max_completion_tokens` suggests `max_completion_tokens` (also when the text names both
///   fields, which is how OpenAI asks to switch); when the text names only that field and calls
///   it unrecognized or unexpected, the provider rejects it and `max_tokens` is suggested.
/// - `stream_usage`: a passing `stream` test with no input and no output token count suggests
///   `true` (ask for usage in the stream).
/// - `developer_role_as_system`: a chat-family test that failed with HTTP 400 naming the
///   `developer` role suggests `true` (send instructions as `system`).
///
/// The chat-family tests are read in column order and the first one that suggests a flag wins.
#[must_use]
pub fn quirk_suggestions(results: &[ProbeResult]) -> BTreeMap<String, QuirkSuggestion> {
    type Rule = fn(&str) -> Option<QuirkSuggestion>;
    const TEXT_RULES: [(&str, Rule); 2] = [
        (MAX_TOKENS_FIELD, max_tokens_field_suggestion),
        (DEVELOPER_ROLE_AS_SYSTEM, developer_role_suggestion),
    ];
    let newest = newest_by_test(results);
    let mut out = BTreeMap::new();
    for test in CHAT_TESTS {
        let Some(result) = newest
            .get(test.as_str())
            .filter(|result| !result.ok && result.status == Some(400))
        else {
            continue;
        };
        let text = error_text(result);
        for (key, rule) in TEXT_RULES {
            if !out.contains_key(key)
                && let Some(suggestion) = rule(&text)
            {
                out.insert(key.to_owned(), suggestion);
            }
        }
    }
    if let Some(suggestion) = stream_usage_suggestion(&newest) {
        out.insert(STREAM_USAGE.to_owned(), suggestion);
    }
    out
}

#[cfg(test)]
mod tests;
