use super::*;
use crate::llm::probe::test_support::probe_result;
use crate::llm::wire::ReasoningStyle;

fn result(test: &str, ok: bool, status: Option<u16>, message: &str, at: i64) -> ProbeResult {
    ProbeResult {
        test: test.to_owned(),
        ok,
        status,
        message: message.to_owned(),
        checked_at_ms: at,
        ..probe_result(ProbeTest::Chat)
    }
}

fn suggested(results: &[ProbeResult], flag: &str) -> Option<Value> {
    quirk_suggestions(results)
        .remove(flag)
        .map(|suggestion| suggestion.value)
}

#[test]
fn sources_table() {
    let preset = Quirks {
        reasoning_request: Some(ReasoningStyle::ThinkingType),
        drop_sampling_params: Some(true),
        max_tokens_field: Some(MaxTokensField::MaxTokens),
        ..Quirks::default()
    };
    let cases: [(Quirks, Option<&Quirks>, &str, QuirkSource); 12] = [
        (
            Quirks::default(),
            None,
            "stream_usage",
            QuirkSource::Default,
        ),
        (
            Quirks {
                stream_usage: Some(true),
                ..Quirks::default()
            },
            None,
            "stream_usage",
            QuirkSource::User,
        ),
        (
            preset.clone(),
            Some(&preset),
            "reasoning_request",
            QuirkSource::Preset,
        ),
        (
            Quirks {
                reasoning_request: Some(ReasoningStyle::None),
                ..preset.clone()
            },
            Some(&preset),
            "reasoning_request",
            QuirkSource::User,
        ),
        (
            Quirks::default(),
            Some(&preset),
            "reasoning_request",
            QuirkSource::Default,
        ),
        (
            preset.clone(),
            Some(&preset),
            "drop_sampling_params",
            QuirkSource::Preset,
        ),
        // Auto under a preset that sets the flag is Auto, not the user's choice.
        (
            Quirks::default(),
            Some(&preset),
            "drop_sampling_params",
            QuirkSource::Default,
        ),
        (
            Quirks {
                drop_sampling_params: Some(true),
                ..Quirks::default()
            },
            None,
            "drop_sampling_params",
            QuirkSource::User,
        ),
        (
            Quirks::default(),
            None,
            "no_done_sentinel",
            QuirkSource::Default,
        ),
        (
            Quirks {
                max_tokens_field: Some(MaxTokensField::MaxCompletionTokens),
                ..Quirks::default()
            },
            Some(&preset),
            "max_tokens_field",
            QuirkSource::User,
        ),
        (
            Quirks {
                max_tokens_field: Some(MaxTokensField::MaxTokens),
                ..Quirks::default()
            },
            Some(&preset),
            "max_tokens_field",
            QuirkSource::Preset,
        ),
        (
            Quirks {
                developer_role_as_system: Some(false),
                ..Quirks::default()
            },
            Some(&preset),
            "developer_role_as_system",
            QuirkSource::User,
        ),
    ];
    for (quirks, from, flag, expected) in cases {
        assert_eq!(quirk_sources(&quirks, from)[flag], expected, "{flag}");
    }
}

#[test]
fn every_compatibility_flag_has_a_source() {
    let sources = quirk_sources(&Quirks::default(), None);
    let names: Vec<&str> = sources.keys().map(String::as_str).collect();
    // Regression guard (anthropic_version, reasoning_response_field, tools_via_responses).
    assert_eq!(
        names,
        [
            "anthropic_version",
            "developer_role_as_system",
            "drop_sampling_params",
            "echo_reasoning_in_tool_turns",
            "max_tokens_field",
            "no_done_sentinel",
            "reasoning_request",
            "reasoning_response_field",
            "stream_usage",
            "tools_via_responses",
        ]
    );
    assert_eq!(
        serde_json::to_value(QuirkSource::Preset).unwrap(),
        json!("preset")
    );
}

/// The panel's `tools_via_responses`, `reasoning_response_field` and `anthropic_version` follow
/// the same rule as the other flags instead of falling back to the preset label.
#[test]
fn the_responses_reasoning_field_and_version_flags_have_sources() {
    let preset = Quirks {
        anthropic_version: Some("2023-06-01".to_owned()),
        ..Quirks::default()
    };
    let quirks = Quirks {
        anthropic_version: Some("2023-06-01".to_owned()),
        reasoning_response_field: Some("thinking".to_owned()),
        tools_via_responses: Some(true),
        ..Quirks::default()
    };
    let sources = quirk_sources(&quirks, Some(&preset));
    assert_eq!(sources.get("anthropic_version"), Some(&QuirkSource::Preset));
    assert_eq!(
        sources.get("reasoning_response_field"),
        Some(&QuirkSource::User)
    );
    assert_eq!(sources.get("tools_via_responses"), Some(&QuirkSource::User));
    let unset = quirk_sources(&Quirks::default(), Some(&preset));
    assert_eq!(
        unset.get("tools_via_responses"),
        Some(&QuirkSource::Default)
    );
    assert_eq!(unset.get("anthropic_version"), Some(&QuirkSource::Default));
}

#[test]
fn max_tokens_field_suggestions() {
    let cases: [(&str, Option<Value>); 7] = [
        (
            "Unsupported parameter: 'max_tokens' is not supported with this model. Use 'max_completion_tokens' instead.",
            Some(json!("max_completion_tokens")),
        ),
        (
            "use max_completion_tokens",
            Some(json!("max_completion_tokens")),
        ),
        (
            "Unrecognized request argument supplied: max_completion_tokens",
            Some(json!("max_tokens")),
        ),
        (
            "Extra inputs are not permitted: max_completion_tokens",
            Some(json!("max_tokens")),
        ),
        ("invalid api key", None),
        ("max_tokens is too large", None),
        ("", None),
    ];
    for (message, expected) in cases {
        let results = [result("chat", false, Some(400), message, 1)];
        assert_eq!(
            suggested(&results, "max_tokens_field"),
            expected,
            "{message}"
        );
    }
}

#[test]
fn a_non_400_or_passing_or_non_chat_result_suggests_nothing() {
    let text = "Use 'max_completion_tokens' instead of max_tokens";
    for results in [
        vec![result("chat", false, Some(500), text, 1)],
        vec![result("chat", true, Some(400), text, 1)],
        vec![result("models", false, Some(400), text, 1)],
        vec![result("chat", false, None, text, 1)],
    ] {
        assert!(quirk_suggestions(&results).is_empty(), "{results:?}");
    }
}

#[test]
fn a_later_pass_clears_the_suggestion_and_a_later_failure_restores_it() {
    let text = "use max_completion_tokens";
    let failed = result("chat", false, Some(400), text, 1);
    let passed = result("chat", true, Some(200), "ok", 2);
    assert!(quirk_suggestions(&[failed.clone(), passed.clone()]).is_empty());
    assert!(quirk_suggestions(&[passed.clone(), failed.clone()]).is_empty());
    let again = result("chat", false, Some(400), text, 3);
    assert!(suggested(&[failed, passed, again], "max_tokens_field").is_some());
}

#[test]
fn the_error_text_may_sit_in_the_detail() {
    let mut failed = result("stream", false, Some(400), "bad request", 1);
    failed.detail = json!({"error": {"message": "unknown role: developer"}});
    assert_eq!(
        suggested(&[failed], "developer_role_as_system"),
        Some(json!(true))
    );
}

#[test]
fn developer_role_suggestions() {
    for (message, expected) in [
        (
            "Invalid value: 'developer'. Supported values are: 'system', 'assistant', 'user', and 'tool'. role",
            Some(json!(true)),
        ),
        ("the developer role is not supported", Some(json!(true))),
        ("developer docs moved", None),
        ("role must be user", None),
    ] {
        let results = [result("chat", false, Some(400), message, 1)];
        assert_eq!(
            suggested(&results, "developer_role_as_system"),
            expected,
            "{message}"
        );
    }
}

#[test]
fn stream_usage_suggestions() {
    let mut metered = result("stream", true, Some(200), "ok", 1);
    metered.output_tokens = Some(4);
    let mut input_only = result("stream", true, Some(200), "ok", 1);
    input_only.input_tokens = Some(4);
    let bare = result("stream", true, Some(200), "ok", 1);
    let failed = result("stream", false, Some(500), "boom", 1);
    let skipped = ProbeResult {
        skip_reason: Some("key_rejected".to_owned()),
        ..result("stream", true, None, "", 1)
    };
    let chat_only = result("chat", true, Some(200), "ok", 1);
    let cases = [
        (vec![metered], None),
        (vec![input_only], None),
        (vec![bare], Some(json!(true))),
        (vec![failed], None),
        (vec![skipped], None),
        (vec![chat_only], None),
    ];
    for (results, expected) in cases {
        assert_eq!(suggested(&results, "stream_usage"), expected, "{results:?}");
    }
}

#[test]
fn suggestions_carry_a_note_and_serialize() {
    let results = [result(
        "chat",
        false,
        Some(400),
        "use max_completion_tokens",
        1,
    )];
    let suggestion = &quirk_suggestions(&results)["max_tokens_field"];
    assert!(suggestion.note.contains("max_completion_tokens"));
    assert_eq!(
        serde_json::to_value(suggestion).unwrap(),
        json!({"value": "max_completion_tokens", "note": suggestion.note})
    );
}
