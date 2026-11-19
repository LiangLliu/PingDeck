//! Chat-family checks that send a model request: chat, tools, JSON schema, vision, reasoning
//! and prompt caching.
//!
//! Single responsibility: one function per check; each fills the [`ProbeResult`] and returns
//! `Ok` for a verdict (pass or content failure) or an [`AttemptError`] for a failed call.

use std::time::Instant;

use serde_json::{Value, json};

use super::{
    Ctx, Outcome, ProbeOptions, chat, elapsed_ms, fail, pass, preview, set_tokens, sum_usage,
    user_request,
};
use crate::llm::failure::FailureKind;
use crate::llm::hub::{
    HubEffort, HubFinish, HubImage, HubMessage, HubPart, HubReasoning, HubRequest,
    HubResponseFormat, HubRole, HubTool, HubToolChoice,
};
use crate::llm::store::ProbeResult;
use crate::llm::upstream::{AttemptError, provider_message};

/// A 16x16 solid red PNG for the vision test.
const RED_PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFklEQVR42mP4z8BAEmIY1TCqYfhqAACQ+f8B8u7oVwAAAABJRU5ErkJggg==";

/// Output tokens of the Chat check.
const CHAT_CAP: u32 = 256;
/// The Chat check's cap for a reasoning model: thinking counts toward `max_completion_tokens`,
/// so 256 tokens are often spent before the first answer token.
const REASONING_CHAT_CAP: u32 = 4096;

/// The Chat check's request: the default prompt and its output cap.
pub(super) fn chat_request(model: &str, options: &ProbeOptions) -> HubRequest {
    let cap = if options.reasoning_model {
        REASONING_CHAT_CAP
    } else {
        CHAT_CAP
    };
    user_request(model, &options.prompt, cap)
}

/// Whether an answer to "what colour is this image" names red as a word ("Red.", "red", "红色"),
/// not as a substring ("bored", "covered", "not red").
pub(super) fn names_red(answer: &str) -> bool {
    let lower = answer.to_lowercase();
    if lower.contains('红') {
        return !lower.contains('不');
    }
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|word| !word.is_empty())
        .collect();
    words.contains(&"red")
        && !words
            .iter()
            .any(|word| matches!(*word, "not" | "no" | "isn't" | "never"))
}

pub(super) async fn test_chat(ctx: &Ctx<'_>, result: &mut ProbeResult) -> Outcome {
    let protocol = ctx.chat_proto()?;
    result.endpoint = protocol.as_str().to_owned();
    let request = chat_request(&ctx.target.model, ctx.options);
    let (response, ms) = chat(ctx, protocol, &request, ctx.timeouts.chat).await?;
    result.latency_ms = Some(ms);
    set_tokens(result, &response.usage);
    let text = response.text();
    result.detail = json!({
        "preview": preview(&text, 200),
        "finish": response.finish,
        "usage": response.usage,
    });
    let other_content = response
        .content
        .iter()
        .any(|part| matches!(part, HubPart::Reasoning { .. } | HubPart::ToolCall { .. }));
    if response.finish == HubFinish::Length {
        // The model answered until the cap: it works, the cap was simply too small (a reasoning
        // model thinks first). Ready, marked truncated, never a failure.
        pass(
            result,
            format!("answered in {ms} ms; the output hit the cap"),
        );
        result.failure_kind = Some(FailureKind::Truncated);
        result.detail["truncated"] = Value::Bool(true);
    } else if text.trim().is_empty() && !other_content {
        fail(
            result,
            FailureKind::EmptyOutput,
            "the model returned no content",
        );
    } else {
        pass(
            result,
            format!("answered in {ms} ms: {}", preview(&text, 60)),
        );
    }
    Ok(())
}

fn weather_tool() -> HubTool {
    HubTool {
        name: "get_weather".to_owned(),
        description: Some("Returns the current weather of a city.".to_owned()),
        parameters: json!({
            "type": "object",
            "properties": {"city": {"type": "string", "description": "City name"}},
            "required": ["city"],
        }),
        strict: None,
    }
}

fn push_step(detail: &mut Value, step: u8, ok: bool, ms: u64) {
    if let Some(steps) = detail["steps"].as_array_mut() {
        steps.push(json!({"step": step, "ok": ok, "ms": ms}));
    }
}

/// The first request of the Tools check.
pub(super) fn tools_request(model: &str) -> HubRequest {
    let mut request = user_request(
        model,
        "What is the weather in Paris? You must call the get_weather tool.",
        512,
    );
    request.tools = vec![weather_tool()];
    request.tool_choice = Some(HubToolChoice::Auto);
    request
}

pub(super) async fn test_tools(ctx: &Ctx<'_>, result: &mut ProbeResult) -> Outcome {
    let protocol = ctx.chat_proto()?;
    result.endpoint = protocol.as_str().to_owned();
    let mut request = tools_request(&ctx.target.model);
    let (first, first_ms) = chat(ctx, protocol, &request, ctx.timeouts.tools_request).await?;
    result.latency_ms = Some(first_ms);
    set_tokens(result, &first.usage);
    let calls: Vec<(String, String, String)> = first
        .tool_calls()
        .into_iter()
        .map(|(id, name, arguments)| (id.to_owned(), name.to_owned(), arguments.to_owned()))
        .collect();
    let Some((_, _, arguments)) = calls.first() else {
        result.detail = json!({"steps": [{"step": 1, "ok": false, "ms": first_ms}]});
        fail(
            result,
            FailureKind::Unknown,
            "the model did not call the tool",
        );
        return Ok(());
    };
    let called: Value =
        serde_json::from_str(arguments).unwrap_or_else(|_| Value::String(arguments.clone()));
    result.detail = json!({"steps": [{"step": 1, "ok": true, "ms": first_ms}], "called": called});

    let first_usage = first.usage.clone();
    request.messages.push(first.into_message());
    request.messages.push(HubMessage {
        role: HubRole::Tool,
        content: calls
            .iter()
            .map(|(id, name, _)| HubPart::ToolResult {
                call_id: id.clone(),
                name: Some(name.clone()),
                content: vec![HubPart::text(r#"{"temp_c": 18, "sky": "cloudy"}"#)],
                is_error: false,
            })
            .collect(),
    });
    // The call itself is the evidence: a model that called the tool supports tool calls, so
    // the follow-up only adds detail and never turns that into a failure.
    let second_started = Instant::now();
    let (second, second_ms) = match chat(ctx, protocol, &request, ctx.timeouts.tools_request).await
    {
        Ok(answer) => answer,
        Err(AttemptError::Cancelled) => return Err(AttemptError::Cancelled),
        Err(error) => {
            push_step(&mut result.detail, 2, false, elapsed_ms(second_started));
            result.detail["round_trip"] = Value::Bool(false);
            let status = match &error {
                AttemptError::Status { status, .. } => format!(" (HTTP {status})"),
                _ => String::new(),
            };
            pass(
                result,
                format!("called get_weather; the answer after the tool result failed{status}"),
            );
            return Ok(());
        }
    };
    let usage = sum_usage(&first_usage, &second.usage);
    set_tokens(result, &usage);
    result.latency_ms = Some(first_ms.saturating_add(second_ms));
    let text = second.text();
    let answered = !text.trim().is_empty();
    push_step(&mut result.detail, 2, answered, second_ms);
    result.detail["round_trip"] = Value::Bool(answered);
    if answered {
        pass(
            result,
            format!("called get_weather, then answered: {}", preview(&text, 60)),
        );
    } else {
        pass(
            result,
            "called get_weather; no text came back after the tool result",
        );
    }
    Ok(())
}

/// The JSON-schema check's request.
pub(super) fn json_schema_request(model: &str) -> HubRequest {
    let mut request = user_request(model, "Name the capital of France.", 512);
    request.response_format = Some(HubResponseFormat::JsonSchema {
        name: "capital".to_owned(),
        schema: json!({
            "type": "object",
            "properties": {"country": {"type": "string"}, "capital": {"type": "string"}},
            "required": ["country", "capital"],
            "additionalProperties": false,
        }),
        strict: Some(true),
    });
    request
}

pub(super) async fn test_json_schema(ctx: &Ctx<'_>, result: &mut ProbeResult) -> Outcome {
    let protocol = ctx.chat_proto()?;
    result.endpoint = protocol.as_str().to_owned();
    let request = json_schema_request(&ctx.target.model);
    let (response, ms) = chat(ctx, protocol, &request, ctx.timeouts.chat).await?;
    result.latency_ms = Some(ms);
    set_tokens(result, &response.usage);
    let text = response.text();
    let parsed: Option<Value> = serde_json::from_str(text.trim()).ok();
    let correct = parsed
        .as_ref()
        .and_then(|value| value["capital"].as_str())
        .is_some_and(|capital| capital.to_ascii_lowercase().contains("paris"));
    result.detail = json!({"parsed": parsed});
    if correct {
        pass(result, "the answer follows the schema");
    } else {
        fail(
            result,
            FailureKind::ProtocolMismatch,
            format!(
                "the answer does not follow the schema: {}",
                preview(&text, 200)
            ),
        );
    }
    Ok(())
}

/// The Vision check's request: the bundled red PNG and a one-word question.
pub(super) fn vision_request(model: &str) -> HubRequest {
    let mut request = user_request(model, "", 256);
    request.messages = vec![HubMessage {
        role: HubRole::User,
        content: vec![
            HubPart::Image {
                image: HubImage::Base64 {
                    media_type: "image/png".to_owned(),
                    data: RED_PNG_B64.to_owned(),
                },
            },
            HubPart::text("What is the dominant color of this image? Answer with one word."),
        ],
    }];
    request
}

pub(super) async fn test_vision(ctx: &Ctx<'_>, result: &mut ProbeResult) -> Outcome {
    let protocol = ctx.chat_proto()?;
    result.endpoint = protocol.as_str().to_owned();
    let request = vision_request(&ctx.target.model);
    let (response, ms) = match chat(ctx, protocol, &request, ctx.timeouts.chat).await {
        Ok(answer) => answer,
        Err(AttemptError::Status { status, body, .. }) if matches!(status, 400 | 415 | 422) => {
            result.status = Some(status);
            fail(
                result,
                FailureKind::ProtocolMismatch,
                format!(
                    "the provider rejected the image: {}",
                    provider_message(&body)
                ),
            );
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    result.latency_ms = Some(ms);
    set_tokens(result, &response.usage);
    let text = response.text();
    result.detail = json!({"answer": preview(&text, 200)});
    if names_red(&text) {
        pass(result, "the model read the image");
    } else {
        fail(
            result,
            FailureKind::Unknown,
            format!("the model did not see a red image: {}", preview(&text, 100)),
        );
    }
    Ok(())
}

/// The Reasoning check's request: a small word problem with medium reasoning effort.
pub(super) fn reasoning_request(model: &str) -> HubRequest {
    let mut request = user_request(
        model,
        "A train travels 120 km in 1.5 hours, then 80 km in 1 hour. What is its average speed? \
         Answer with just the number and unit.",
        2048,
    );
    request.reasoning = Some(HubReasoning {
        enabled: Some(true),
        effort: Some(HubEffort::Medium),
        budget_tokens: None,
    });
    request
}

pub(super) async fn test_reasoning(ctx: &Ctx<'_>, result: &mut ProbeResult) -> Outcome {
    let protocol = ctx.chat_proto()?;
    result.endpoint = protocol.as_str().to_owned();
    let request = reasoning_request(&ctx.target.model);
    let (response, ms) = chat(ctx, protocol, &request, ctx.timeouts.reasoning).await?;
    result.latency_ms = Some(ms);
    set_tokens(result, &response.usage);
    let reasoning_chars: usize = response
        .content
        .iter()
        .map(|part| match part {
            HubPart::Reasoning { text, .. } => text.chars().count(),
            _ => 0,
        })
        .sum();
    // A block that carries only a signature (Claude 4.7 without a summary) is reasoning.
    let sealed = response.content.iter().any(|part| {
        matches!(
            part,
            HubPart::Reasoning {
                encrypted: Some(_),
                ..
            } | HubPart::Reasoning {
                signature: Some(_),
                ..
            }
        )
    });
    let reasoning_tokens = response.usage.reasoning_tokens;
    result.detail = json!({
        "reasoning_tokens": reasoning_tokens,
        "reasoning_chars": reasoning_chars,
        "answer": preview(&response.text(), 200),
    });
    if reasoning_chars > 0 || reasoning_tokens.is_some_and(|tokens| tokens > 0) || sealed {
        pass(result, "the model returned reasoning");
    } else {
        fail(
            result,
            FailureKind::Unknown,
            "the model returned no reasoning",
        );
    }
    Ok(())
}

/// A fixed system prompt of about 1,400 words (over 1,200 tokens), so providers that cache
/// implicitly (OpenAI from 1,024 tokens) have something to cache.
pub(super) fn cache_system_prompt() -> String {
    (1..=31)
        .map(|index| {
            format!(
                "Section {index}. The harbor archive keeps tide tables, ledgers of cargo, and notes \
                 from lamp keepers; every record is checked twice by a clerk, copied in ink, bound \
                 with linen thread, and shelved by year so that later readers can find the original \
                 entry without guessing."
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The Cache check's request (sent twice): the fixed long system prompt.
pub(super) fn cache_request(model: &str) -> HubRequest {
    let mut request = user_request(model, "Reply with the single word OK.", 16);
    request.system = Some(cache_system_prompt());
    request
}

pub(super) async fn test_cache(ctx: &Ctx<'_>, result: &mut ProbeResult) -> Outcome {
    let protocol = ctx.chat_proto()?;
    result.endpoint = protocol.as_str().to_owned();
    let request = cache_request(&ctx.target.model);
    let (first, first_ms) = chat(ctx, protocol, &request, ctx.timeouts.cache_request).await?;
    let (second, second_ms) = chat(ctx, protocol, &request, ctx.timeouts.cache_request).await?;
    result.latency_ms = Some(first_ms.saturating_add(second_ms));
    set_tokens(result, &sum_usage(&first.usage, &second.usage));
    let cached = second.usage.cached_input_tokens;
    result.detail = json!({
        "first_cached": first.usage.cached_input_tokens,
        "second_cached": cached,
        "input_tokens": second.usage.input_tokens,
    });
    match cached.filter(|tokens| *tokens > 0) {
        Some(tokens) => {
            let of = second
                .usage
                .input_tokens
                .map_or_else(String::new, |input| format!(" of {input}"));
            pass(result, format!("cache hit: {tokens}{of} tokens"));
        }
        None => fail(
            result,
            FailureKind::Unknown,
            "no cached tokens reported (the provider may not cache implicitly)",
        ),
    }
    Ok(())
}
