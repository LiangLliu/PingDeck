//! Response decoding (upstream) and request decoding (client) of the Anthropic Messages codec.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use super::decode_blocks::{decode_simple_part, server_tool_result, server_tool_use};
use super::{OWN, decode_stop_reason, error_text, read_usage};
use crate::llm::adapters::{InboundHint, as_u32, parse_effort, str_of, str_or_empty};
use crate::llm::hub::{
    HubEffort, HubMessage, HubNativeTool, HubPart, HubReasoning, HubRequest, HubResponse,
    HubResponseFormat, HubRole, HubTool, HubToolChoice, HubUsage, unwrap_signature,
};
use spanbay_base::error::AppError;

/// Tool-call id to tool name, collected from `tool_use` blocks so a later `tool_result` can name
/// its tool.
type ToolNames = HashMap<String, String>;

pub(super) fn response(body: &Value) -> Result<HubResponse, AppError> {
    if str_of(body, "type") == Some("error") {
        return Err(AppError::network(error_text(body)));
    }
    let blocks = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::network("anthropic response has no content array"))?;
    let mut content = Vec::new();
    for block in blocks {
        match str_of(block, "type") {
            Some("text") => content.extend(decode_simple_part(block)?),
            // An upstream signature is Anthropic's own, so it needs no unwrapping.
            Some("thinking") => {
                let signature = str_of(block, "signature")
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                content.push(HubPart::Reasoning {
                    text: str_or_empty(block, "thinking"),
                    origin: signature.as_ref().map(|_| OWN),
                    signature,
                    encrypted: None,
                    id: None,
                });
            }
            Some("redacted_thinking") => content.push(HubPart::Reasoning {
                text: String::new(),
                signature: None,
                encrypted: str_of(block, "data").map(str::to_owned),
                id: None,
                origin: Some(OWN),
            }),
            Some("tool_use") => content.push(tool_call_part(block)),
            Some("server_tool_use") => content.push(server_tool_use(block)),
            Some("web_search_tool_result") => content.push(server_tool_result(block)),
            other => {
                return Err(AppError::network(format!(
                    "anthropic response has an unsupported content block `{}`",
                    other.unwrap_or("missing")
                )));
            }
        }
    }
    Ok(HubResponse {
        id: str_or_empty(body, "id"),
        model: str_or_empty(body, "model"),
        content,
        finish: decode_stop_reason(str_of(body, "stop_reason")),
        usage: body
            .get("usage")
            .map(|usage| read_usage(usage, &HubUsage::default()))
            .unwrap_or_default(),
    })
}

/// Decodes a gateway client's `/v1/messages` body.
///
/// Errors come in a fixed order: body shape, `model`, `messages` shape, each message in turn,
/// then each tool.
pub(super) fn request(body: &Value, hint: &InboundHint) -> Result<HubRequest, AppError> {
    let obj = body
        .as_object()
        .ok_or_else(|| AppError::validation("request body must be a JSON object"))?;
    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| hint.model.clone())
        .filter(|m| !m.is_empty())
        .ok_or_else(|| AppError::validation("`model` is required"))?;
    let raw_messages = obj
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::validation("`messages` must be an array"))?;

    let mut req = HubRequest {
        model,
        system: obj.get("system").and_then(decode_system),
        stream: obj.get("stream").and_then(Value::as_bool).unwrap_or(false) || hint.stream,
        max_tokens: obj.get("max_tokens").and_then(as_u32),
        temperature: obj.get("temperature").and_then(Value::as_f64),
        top_p: obj.get("top_p").and_then(Value::as_f64),
        top_k: obj.get("top_k").and_then(as_u32),
        user: obj
            .get("metadata")
            .and_then(|m| str_of(m, "user_id"))
            .map(str::to_owned),
        ..HubRequest::default()
    };
    if let Some(stops) = obj.get("stop_sequences").and_then(Value::as_array) {
        req.stop = stops
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
    }
    decode_messages(raw_messages, &mut req)?;
    decode_tools(obj, &mut req)?;
    if let Some(choice) = obj.get("tool_choice") {
        decode_tool_choice(choice, &mut req);
    }
    decode_reasoning_and_format(obj, &mut req);
    Ok(req)
}

/// Decodes the messages: a user message splits into a tool message (its `tool_result` blocks)
/// followed by a user message (the rest); empty messages are dropped.
fn decode_messages(raw_messages: &[Value], req: &mut HubRequest) -> Result<(), AppError> {
    let mut names = ToolNames::new();
    for message in raw_messages {
        let role = str_of(message, "role")
            .ok_or_else(|| AppError::validation("message `role` is required"))?;
        let (parts, results) = decode_content(message.get("content"), &mut names)?;
        let mut push = |role, content: Vec<HubPart>| {
            if !content.is_empty() {
                req.messages.push(HubMessage { role, content });
            }
        };
        match role {
            // Tool results inside an assistant turn have no hub meaning and are dropped.
            "assistant" => push(HubRole::Assistant, parts),
            "user" => {
                push(HubRole::Tool, results);
                push(HubRole::User, parts);
            }
            other => {
                return Err(AppError::validation(format!(
                    "unsupported message role `{other}`"
                )));
            }
        }
    }
    Ok(())
}

/// Decodes `tools`: function tools and the `web_search_*` native tool.
fn decode_tools(obj: &Map<String, Value>, req: &mut HubRequest) -> Result<(), AppError> {
    let Some(tools) = obj.get("tools").and_then(Value::as_array) else {
        return Ok(());
    };
    for tool in tools {
        match str_of(tool, "type") {
            None | Some("custom") => req.tools.push(decode_tool(tool)?),
            Some(kind) if kind.starts_with("web_search_") => {
                req.native_tools.push(decode_native_search(tool));
            }
            Some(kind) => {
                return Err(AppError::unsupported(format!(
                    "anthropic: tool type `{kind}` is not supported"
                )));
            }
        }
    }
    Ok(())
}

/// Decodes `tool_choice`; an unknown type leaves the choice unset.
fn decode_tool_choice(choice: &Value, req: &mut HubRequest) {
    req.tool_choice = match str_of(choice, "type") {
        Some("auto") => Some(HubToolChoice::Auto),
        Some("any") => Some(HubToolChoice::Required),
        Some("none") => Some(HubToolChoice::None),
        Some("tool") => str_of(choice, "name").map(|name| HubToolChoice::Tool {
            name: name.to_owned(),
        }),
        _ => None,
    };
    if choice
        .get("disable_parallel_tool_use")
        .and_then(Value::as_bool)
        == Some(true)
    {
        req.parallel_tool_calls = Some(false);
    }
}

/// Decodes `thinking` and `output_config` (effort and JSON schema format).
fn decode_reasoning_and_format(obj: &Map<String, Value>, req: &mut HubRequest) {
    let mut reasoning = HubReasoning::default();
    if let Some(thinking) = obj.get("thinking") {
        match str_of(thinking, "type") {
            Some("enabled") => {
                reasoning.enabled = Some(true);
                reasoning.budget_tokens = thinking.get("budget_tokens").and_then(as_u32);
            }
            Some("adaptive") => reasoning.enabled = Some(true),
            Some("disabled") => reasoning.enabled = Some(false),
            _ => {}
        }
    }
    if let Some(config) = obj.get("output_config") {
        // Anthropic has no `minimal` effort.
        reasoning.effort = str_of(config, "effort")
            .and_then(parse_effort)
            .filter(|effort| *effort != HubEffort::Minimal);
        if let Some(format) = config.get("format")
            && str_of(format, "type") == Some("json_schema")
        {
            req.response_format = Some(HubResponseFormat::JsonSchema {
                name: "response".to_owned(),
                schema: format.get("schema").cloned().unwrap_or_else(|| json!({})),
                strict: None,
            });
        }
    }
    if reasoning != HubReasoning::default() {
        req.reasoning = Some(reasoning);
    }
}

/// A string system prompt, or the text blocks of an array joined by blank lines; `None` when
/// empty.
fn decode_system(system: &Value) -> Option<String> {
    let text = match system {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| str_of(block, "text"))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => return None,
    };
    (!text.is_empty()).then_some(text)
}

fn decode_tool(tool: &Value) -> Result<HubTool, AppError> {
    Ok(HubTool {
        name: str_of(tool, "name")
            .ok_or_else(|| AppError::validation("tool `name` is required"))?
            .to_owned(),
        description: str_of(tool, "description").map(str::to_owned),
        parameters: tool
            .get("input_schema")
            .cloned()
            .unwrap_or_else(|| json!({"type": "object"})),
        strict: tool.get("strict").and_then(Value::as_bool),
    })
}

fn decode_native_search(tool: &Value) -> HubNativeTool {
    let domains = |key: &str| -> Vec<String> {
        tool.get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    HubNativeTool::WebSearch {
        max_uses: tool.get("max_uses").and_then(as_u32),
        allowed_domains: domains("allowed_domains"),
        blocked_domains: domains("blocked_domains"),
    }
}

/// Decodes a message's `content` into `(parts, tool results)`.
fn decode_content(
    content: Option<&Value>,
    names: &mut ToolNames,
) -> Result<(Vec<HubPart>, Vec<HubPart>), AppError> {
    let blocks = match content {
        Some(Value::String(text)) => {
            let parts = if text.is_empty() {
                Vec::new()
            } else {
                vec![HubPart::text(text.clone())]
            };
            return Ok((parts, Vec::new()));
        }
        Some(Value::Array(blocks)) => blocks,
        _ => {
            return Err(AppError::validation(
                "message `content` must be a string or an array",
            ));
        }
    };
    let (mut parts, mut results) = (Vec::new(), Vec::new());
    for block in blocks {
        match str_of(block, "type") {
            Some("tool_use") => {
                let part = tool_call_part(block);
                if let HubPart::ToolCall { id, name, .. } = &part {
                    names.insert(id.clone(), name.clone());
                }
                parts.push(part);
            }
            Some("tool_result") => results.push(decode_tool_result(block, names)?),
            Some("thinking") => parts.push(decode_client_thinking(block)),
            Some("redacted_thinking") => parts.push(decode_client_redacted(block)),
            Some("server_tool_use") => parts.push(server_tool_use(block)),
            Some("web_search_tool_result") => parts.push(server_tool_result(block)),
            _ => parts.extend(decode_simple_part(block)?),
        }
    }
    Ok((parts, results))
}

fn tool_call_part(block: &Value) -> HubPart {
    HubPart::ToolCall {
        id: str_or_empty(block, "id"),
        name: str_or_empty(block, "name"),
        arguments: block
            .get("input")
            .map_or_else(|| "{}".to_owned(), Value::to_string),
        signature: None,
        origin: None,
    }
}

/// A `tool_result` block, named through the earlier `tool_use` with the same id.
fn decode_tool_result(block: &Value, names: &ToolNames) -> Result<HubPart, AppError> {
    let call_id = str_or_empty(block, "tool_use_id");
    let content = match block.get("content") {
        Some(Value::String(text)) if !text.is_empty() => vec![HubPart::text(text.clone())],
        Some(Value::Array(items)) => {
            let mut inner = Vec::new();
            for item in items {
                inner.extend(decode_simple_part(item)?);
            }
            inner
        }
        _ => Vec::new(),
    };
    Ok(HubPart::ToolResult {
        name: names.get(&call_id).cloned(),
        call_id,
        content,
        is_error: block
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// A client's thinking block; its signature may carry a `pdk1.` envelope naming its producer.
fn decode_client_thinking(block: &Value) -> HubPart {
    let unwrapped = str_of(block, "signature")
        .filter(|s| !s.is_empty())
        .map(|raw| unwrap_signature(raw, OWN));
    HubPart::Reasoning {
        text: str_or_empty(block, "thinking"),
        origin: unwrapped.as_ref().map(|(origin, _)| *origin),
        signature: unwrapped.map(|(_, signature)| signature),
        encrypted: None,
        id: None,
    }
}

/// A client's redacted thinking block; its data may carry a `pdk1.` envelope.
fn decode_client_redacted(block: &Value) -> HubPart {
    let unwrapped = str_of(block, "data").map(|raw| unwrap_signature(raw, OWN));
    HubPart::Reasoning {
        text: String::new(),
        signature: None,
        origin: unwrapped.as_ref().map(|(origin, _)| *origin),
        encrypted: unwrapped.map(|(_, data)| data),
        id: None,
    }
}
