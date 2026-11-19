//! Request encoding (upstream) and response encoding (client) of the Anthropic Messages codec.

use serde_json::{Map, Value, json};

use super::encode_blocks::{
    client_reasoning_block, document_block, encode_native_tool, encode_tool, image_block, no_audio,
    part_kind, reasoning_block, server_tool_result_block, server_tool_use_block, tool_result_block,
    tool_use_block,
};
use super::thinking::encode_thinking;
use super::{JSON_OBJECT_NOTE, OWN, encode_stop_reason, text_block, usage_json};
use crate::llm::adapters::generation::{self, claude_prefill_generation};
use crate::llm::adapters::{EncodeCtx, insert_some};
use crate::llm::hub::{
    CacheBreakpoint, CacheHint, HubMessage, HubPart, HubRequest, HubResponse, HubResponseFormat,
    HubRole, HubToolChoice, signature_usable,
};
use pingdeck_base::error::AppError;

pub(super) fn request(req: &HubRequest, ctx: &EncodeCtx<'_>) -> Result<Value, AppError> {
    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    let mut output_config = Map::new();
    let thinking = encode_thinking(req, ctx, &mut body, &mut output_config);
    body.insert("max_tokens".into(), json!(thinking.max_tokens));

    let system = system_text(req);
    // Error order is part of the contract: messages, then prefill, then native tools.
    let mut messages = encode_messages(&req.messages)?;
    prepare_prefill(&req.model, thinking.on, &mut messages)?;
    let mut tools = encode_tools(req)?;
    let mut system = match (system, &req.cache) {
        (Some(system), Some(_)) => Some(json!([text_block(&system)])),
        (system, _) => system.map(Value::String),
    };
    if let Some(cache) = &req.cache {
        mark_cache(cache, system.as_mut(), &mut tools, &mut messages);
    }
    insert_some(&mut body, "system", system);
    body.insert("messages".into(), Value::Array(messages));
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
    }
    if !req.tools.is_empty()
        && let Some(choice) = encode_tool_choice(req)
    {
        body.insert("tool_choice".into(), Value::Object(choice));
    }
    insert_sampling(req, ctx, thinking.on, &mut body);
    if !req.stop.is_empty() {
        body.insert("stop_sequences".into(), json!(req.stop));
    }
    if let Some(HubResponseFormat::JsonSchema { schema, .. }) = &req.response_format {
        output_config.insert(
            "format".into(),
            json!({"type": "json_schema", "schema": schema}),
        );
    }
    if !output_config.is_empty() {
        body.insert("output_config".into(), Value::Object(output_config));
    }
    if let Some(user) = &req.user {
        body.insert("metadata".into(), json!({"user_id": user}));
    }
    if req.stream {
        body.insert("stream".into(), json!(true));
    }
    Ok(Value::Object(body))
}

pub(super) fn response(response: &HubResponse) -> Value {
    let content: Vec<Value> = response
        .content
        .iter()
        .filter_map(|part| match part {
            HubPart::Text { text, .. } => Some(text_block(text)),
            HubPart::Reasoning {
                text,
                signature,
                encrypted,
                id,
                origin,
            } => client_reasoning_block(
                text,
                signature.as_deref(),
                encrypted.as_deref(),
                id.as_deref(),
                *origin,
            ),
            HubPart::ToolCall {
                id,
                name,
                arguments,
                ..
            } => Some(tool_use_block(id, name, arguments)),
            HubPart::ServerToolUse {
                id,
                name,
                input,
                origin,
            } if signature_usable(*origin, OWN) => Some(server_tool_use_block(id, name, input)),
            HubPart::ServerToolResult {
                call_id,
                name,
                payload,
                origin,
                ..
            } if signature_usable(*origin, OWN) => {
                Some(server_tool_result_block(call_id, name, payload))
            }
            _ => None,
        })
        .collect();
    json!({
        "id": response.id,
        "type": "message",
        "role": "assistant",
        "model": response.model,
        "content": content,
        "stop_reason": encode_stop_reason(&response.finish),
        "stop_sequence": null,
        "usage": usage_json(&response.usage),
    })
}

/// The system prompt, with the JSON-object note appended when that format is asked for.
fn system_text(req: &HubRequest) -> Option<String> {
    let json_object = matches!(req.response_format, Some(HubResponseFormat::JsonObject));
    match (req.system.as_deref().filter(|s| !s.is_empty()), json_object) {
        (Some(system), true) => Some(format!("{system}\n\n{JSON_OBJECT_NOTE}")),
        (Some(system), false) => Some(system.to_owned()),
        (None, true) => Some(JSON_OBJECT_NOTE.to_owned()),
        (None, false) => None,
    }
}

/// Writes `temperature`, `top_p` and `top_k` unless something forbids them: the provider's quirk,
/// extended thinking (Anthropic rejects sampling parameters with it) or a Claude 5+ model, which
/// refuses them whatever the provider's flags say.
fn insert_sampling(
    req: &HubRequest,
    ctx: &EncodeCtx<'_>,
    thinking_on: bool,
    body: &mut Map<String, Value>,
) {
    if ctx.quirks.drops_sampling() || thinking_on || !generation::claude_takes_sampling(&req.model)
    {
        return;
    }
    insert_some(body, "temperature", req.temperature);
    insert_some(body, "top_p", req.top_p);
    insert_some(body, "top_k", req.top_k);
}

/// Writes `cache_control`: `SystemAndTools` on the last tool and the system block,
/// `LastMessage` on the last block of the last message that may carry one (thinking blocks may
/// not). Three markers at most, under Anthropic's limit of four.
fn mark_cache(
    cache: &CacheHint,
    mut system: Option<&mut Value>,
    tools: &mut [Value],
    messages: &mut [Value],
) {
    let mark = |block: &mut Value| {
        if let Some(block) = block.as_object_mut() {
            block.insert("cache_control".into(), json!({"type": "ephemeral"}));
        }
    };
    for breakpoint in &cache.breakpoints {
        match breakpoint {
            CacheBreakpoint::SystemAndTools => {
                if let Some(tool) = tools.last_mut() {
                    mark(tool);
                }
                if let Some(Value::Array(blocks)) = system.as_deref_mut()
                    && let Some(block) = blocks.last_mut()
                {
                    mark(block);
                }
            }
            CacheBreakpoint::LastMessage => {
                // `encode_messages` always writes `content` as an array of blocks.
                let markable = messages
                    .last_mut()
                    .and_then(|message| message.get_mut("content"))
                    .and_then(Value::as_array_mut)
                    .and_then(|blocks| {
                        blocks.iter_mut().rev().find(|block| {
                            !matches!(
                                block.get("type").and_then(Value::as_str),
                                Some("thinking" | "redacted_thinking")
                            )
                        })
                    });
                if let Some(block) = markable {
                    mark(block);
                }
            }
        }
    }
}

/// Applies Anthropic's rules for a trailing assistant message (prefill).
///
/// Claude 4.6 and later, and any Claude with thinking on, refuse prefill, so it is a validation
/// error there. Elsewhere Anthropic still rejects a final assistant turn that ends in whitespace:
/// trailing whitespace is trimmed, and a block (then a message) left empty is dropped.
fn prepare_prefill(
    model: &str,
    thinking_on: bool,
    messages: &mut Vec<Value>,
) -> Result<(), AppError> {
    let Some(last) = messages.last_mut() else {
        return Ok(());
    };
    if last.get("role").and_then(Value::as_str) != Some("assistant") {
        return Ok(());
    }
    if let Some(generation) = claude_prefill_generation(model) {
        let family = if generation >= (4, 6) {
            Some("Claude 4.6 and later models")
        } else if thinking_on {
            Some("Claude models with thinking enabled")
        } else {
            None
        };
        if let Some(family) = family {
            return Err(AppError::validation(format!(
                "{family} ({model}) do not accept a trailing assistant message (prefill); end the conversation with a user or tool message"
            )));
        }
    }
    let mut emptied = false;
    if let Some(blocks) = last.get_mut("content").and_then(Value::as_array_mut) {
        if let Some(last_block) = blocks.last_mut()
            && last_block.get("type").and_then(Value::as_str) == Some("text")
            && let Some(Value::String(text)) = last_block.get_mut("text")
        {
            let trimmed_len = text.trim_end().len();
            if trimmed_len == 0 {
                blocks.pop();
            } else {
                text.truncate(trimmed_len);
            }
        }
        emptied = blocks.is_empty();
    }
    if emptied {
        messages.pop();
    }
    Ok(())
}

/// One Anthropic message being assembled from consecutive hub messages of the same role.
struct Turn {
    role: &'static str,
    /// `tool_result` blocks; Anthropic wants them before any other block of a user turn.
    results: Vec<Value>,
    blocks: Vec<Value>,
}

/// Encodes hub messages as Anthropic turns: tool messages become user turns, and consecutive
/// turns of one role are merged (roles must alternate).
fn encode_messages(messages: &[HubMessage]) -> Result<Vec<Value>, AppError> {
    let mut turns: Vec<Turn> = Vec::new();
    for message in messages {
        let assistant = message.role == HubRole::Assistant;
        let mut results = Vec::new();
        let mut blocks = Vec::new();
        for part in &message.content {
            encode_part(part, assistant, &mut results, &mut blocks)?;
        }
        if results.is_empty() && blocks.is_empty() {
            continue;
        }
        push_turn(&mut turns, assistant, results, blocks);
    }
    Ok(turns
        .into_iter()
        .map(|turn| {
            let mut content = turn.results;
            content.extend(turn.blocks);
            json!({"role": turn.role, "content": content})
        })
        .collect())
}

/// Encodes one part of a user (or tool) or assistant message into `results` or `blocks`.
fn encode_part(
    part: &HubPart,
    assistant: bool,
    results: &mut Vec<Value>,
    blocks: &mut Vec<Value>,
) -> Result<(), AppError> {
    match part {
        HubPart::ToolResult {
            call_id,
            content,
            is_error,
            ..
        } if !assistant => results.push(tool_result_block(call_id, content, *is_error)?),
        HubPart::Text { text, .. } if text.is_empty() => {}
        HubPart::Text { text, .. } => blocks.push(text_block(text)),
        HubPart::Image { image } if !assistant => blocks.push(image_block(image)?),
        HubPart::Document { document } if !assistant => blocks.push(document_block(document)?),
        HubPart::Audio { .. } => return Err(no_audio()),
        HubPart::ToolCall {
            id,
            name,
            arguments,
            ..
        } if assistant => blocks.push(tool_use_block(id, name, arguments)),
        HubPart::Reasoning {
            text,
            signature,
            encrypted,
            id,
            origin,
        } if assistant => blocks.extend(reasoning_block(
            text,
            signature.as_deref(),
            encrypted.as_deref(),
            id.as_deref(),
            *origin,
        )),
        HubPart::ServerToolUse {
            id,
            name,
            input,
            origin,
        } if assistant => {
            if signature_usable(*origin, OWN) {
                blocks.push(server_tool_use_block(id, name, input));
            }
        }
        HubPart::ServerToolResult {
            call_id,
            name,
            payload,
            origin,
            ..
        } if assistant => {
            if signature_usable(*origin, OWN) {
                blocks.push(server_tool_result_block(call_id, name, payload));
            }
        }
        other => {
            return Err(AppError::unsupported(format!(
                "anthropic: a {} in {} message is not supported",
                part_kind(other),
                if assistant { "an assistant" } else { "a user" }
            )));
        }
    }
    Ok(())
}

/// Appends a turn, merging it into the previous one when the role repeats.
fn push_turn(turns: &mut Vec<Turn>, assistant: bool, results: Vec<Value>, blocks: Vec<Value>) {
    let role = if assistant { "assistant" } else { "user" };
    match turns.last_mut() {
        Some(turn) if turn.role == role => {
            turn.results.extend(results);
            turn.blocks.extend(blocks);
        }
        _ => turns.push(Turn {
            role,
            results,
            blocks,
        }),
    }
}

/// The `tools` array: function tools (only the allowed ones under an allow-list choice), then
/// native tools.
fn encode_tools(req: &HubRequest) -> Result<Vec<Value>, AppError> {
    let mut tools: Vec<Value> = req.tools_for_forced_choice().map(encode_tool).collect();
    for native in &req.native_tools {
        tools.push(encode_native_tool(native)?);
    }
    Ok(tools)
}

/// The `tool_choice` object; `None` when the request leaves the choice to Anthropic.
///
/// `parallel_tool_calls == Some(false)` becomes `disable_parallel_tool_use` on every choice but
/// `none`, and forces an explicit `auto` when no choice was given.
fn encode_tool_choice(req: &HubRequest) -> Option<Map<String, Value>> {
    let no_parallel = req.parallel_tool_calls == Some(false);
    let (kind, name) = match &req.tool_choice {
        Some(HubToolChoice::Auto) => ("auto", None),
        Some(HubToolChoice::None) => ("none", None),
        Some(HubToolChoice::Required | HubToolChoice::Allowed { .. }) => ("any", None),
        Some(HubToolChoice::Tool { name }) => ("tool", Some(name.as_str())),
        None if no_parallel => ("auto", None),
        None => return None,
    };
    let mut choice = Map::new();
    choice.insert("type".into(), json!(kind));
    insert_some(&mut choice, "name", name);
    if no_parallel && kind != "none" {
        choice.insert("disable_parallel_tool_use".into(), json!(true));
    }
    Some(choice)
}
