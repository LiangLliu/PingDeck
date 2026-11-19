//! The generation-dependent thinking request, effort and sampling parameters.

use super::*;

#[test]
fn adaptive_quirk_sends_adaptive_thinking_and_low_effort_without_sampling() {
    let mut adaptive = HubRequest::user_prompt("claude", "q");
    adaptive.temperature = Some(0.2);
    adaptive.reasoning = Some(HubReasoning {
        effort: Some(HubEffort::Minimal),
        ..HubReasoning::default()
    });
    let body = encode(
        &adaptive,
        &Quirks {
            reasoning_request: Some(ReasoningStyle::AnthropicAdaptive),
            ..Quirks::default()
        },
    );
    assert_eq!(body["thinking"], json!({"type": "adaptive"}));
    assert_eq!(body["output_config"], json!({"effort": "low"}));
    assert!(body.get("temperature").is_none());
}

#[test]
fn thinking_follows_the_claude_generation() {
    // 4.5 takes only a budget, 4.7 only adaptive plus an effort.
    let request = |model: &str| HubRequest {
        reasoning: Some(HubReasoning {
            effort: Some(HubEffort::High),
            budget_tokens: Some(2_000),
            ..HubReasoning::default()
        }),
        ..HubRequest::user_prompt(model, "q")
    };
    let old = encode(&request("claude-sonnet-4-5"), &Quirks::default());
    assert_eq!(
        old["thinking"],
        json!({"type": "enabled", "budget_tokens": 2_000})
    );
    assert!(old.get("output_config").is_none());
    let new = encode(&request("claude-opus-4-7"), &Quirks::default());
    // 4.7 and later are asked for summarized thinking, or the client sees none.
    assert_eq!(
        new["thinking"],
        json!({"type": "adaptive", "display": "summarized"})
    );
    assert_eq!(new["output_config"]["effort"], "high");
    // `xhigh` stays `xhigh` where the generation has it and is `high`, not `max`, elsewhere.
    let mut xhigh = request("claude-opus-4-7");
    xhigh.reasoning.as_mut().unwrap().effort = Some(HubEffort::XHigh);
    assert_eq!(
        encode(&xhigh, &Quirks::default())["output_config"]["effort"],
        "xhigh"
    );
    xhigh.model = "claude-opus-4-6".into();
    xhigh.reasoning.as_mut().unwrap().budget_tokens = None;
    assert_eq!(
        encode(&xhigh, &Quirks::default())["output_config"]["effort"],
        "high"
    );
}

#[test]
fn claude_5_gets_no_sampling_parameters_whatever_the_flags_say() {
    // The Bedrock preset sets no `drop_sampling_params`; Claude 5 refuses them anyway.
    let mut req = HubRequest::user_prompt("anthropic.claude-sonnet-5-5", "q");
    req.temperature = Some(0.2);
    req.top_p = Some(0.9);
    let body = encode(&req, &Quirks::default());
    assert!(body.get("temperature").is_none() && body.get("top_p").is_none());
    req.model = "claude-sonnet-4-5".into();
    assert_eq!(encode(&req, &Quirks::default())["temperature"], 0.2);
}

#[test]
fn max_tokens_is_raised_above_the_thinking_budget() {
    let request = |max_tokens: Option<u32>| HubRequest {
        max_tokens,
        reasoning: Some(HubReasoning {
            budget_tokens: Some(2_000),
            ..HubReasoning::default()
        }),
        ..HubRequest::user_prompt("claude-sonnet-4-5", "q")
    };
    for (max_tokens, expected) in [
        (Some(1_000), 3_000),
        (Some(2_000), 4_000),
        (Some(0), 2_001),
        (Some(2_001), 2_001),
        (None, 8_192),
    ] {
        let body = encode(&request(max_tokens), &Quirks::default());
        assert_eq!(body["max_tokens"], expected, "{max_tokens:?}");
        assert_eq!(
            body["thinking"],
            json!({"type": "enabled", "budget_tokens": 2_000})
        );
    }
}

#[test]
fn a_budget_only_generation_turns_the_effort_into_a_budget() {
    let request = |enabled: Option<bool>, effort: Option<HubEffort>| HubRequest {
        reasoning: Some(HubReasoning {
            enabled,
            effort,
            budget_tokens: None,
        }),
        ..HubRequest::user_prompt("claude-sonnet-4-5", "q")
    };
    for (enabled, effort, budget, max_tokens) in [
        (None, Some(HubEffort::Minimal), 1_024, 8_192),
        (None, Some(HubEffort::Low), 1_024, 8_192),
        (None, Some(HubEffort::Medium), 4_096, 8_192),
        (None, Some(HubEffort::High), 16_000, 24_192),
        (None, Some(HubEffort::XHigh), 24_000, 32_192),
        (None, Some(HubEffort::Max), 32_000, 40_192),
        (Some(true), None, 4_096, 8_192),
    ] {
        let body = encode(&request(enabled, effort), &Quirks::default());
        assert_eq!(
            body["thinking"],
            json!({"type": "enabled", "budget_tokens": budget}),
            "{effort:?}"
        );
        assert_eq!(body["max_tokens"], max_tokens, "{effort:?}");
        assert!(body.get("output_config").is_none(), "{effort:?}");
    }
    let body = encode(&request(None, None), &Quirks::default());
    assert!(body.get("thinking").is_none());
}

#[test]
fn disabled_thinking_is_written_only_under_the_adaptive_quirk() {
    let mut req = HubRequest::user_prompt("claude-opus-4-7", "q");
    req.reasoning = Some(HubReasoning {
        enabled: Some(false),
        ..HubReasoning::default()
    });
    let adaptive = Quirks {
        reasoning_request: Some(ReasoningStyle::AnthropicAdaptive),
        ..Quirks::default()
    };
    assert_eq!(
        encode(&req, &adaptive)["thinking"],
        json!({"type": "disabled"})
    );
    assert!(encode(&req, &Quirks::default()).get("thinking").is_none());
}
