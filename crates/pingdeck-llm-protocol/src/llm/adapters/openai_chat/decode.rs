//! Chat Completions JSON to hub: an upstream's response body and a client's request.

use serde_json::{Value, json};

use super::reasoning::decode_reasoning;
use super::{
    content_text, decode_finish, decode_usage, error_message, reasoning_part, reasoning_text,
    tool_call_parts, url_citations,
};
use crate::llm::adapters::{InboundHint, non_null, parse_base64_data_url, str_or_empty, to_u32};
use crate::llm::hub::{
    HubDocument, HubDocumentSource, HubImage, HubMessage, HubNativeTool, HubPart, HubRequest,
    HubResponse, HubResponseFormat, HubRole, HubTool, HubToolChoice,
};
use pingdeck_base::error::AppError;

/// Decodes an upstream's response body (its first choice).
pub(super) fn response(body: &Value) -> Result<HubResponse, AppError> {
    if let Some(error) = non_null(body, "error") {
        return Err(AppError::network(error_message(error)));
    }
    let choice = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| AppError::network("response has no choices"))?;
    let message = &choice["message"];
    let mut content = Vec::new();
    if let Some(text) = reasoning_text(message) {
        content.push(reasoning_part(text));
    }
    let text = content_text(&message["content"]);
    let citations = url_citations(&message["annotations"]);
    if !text.is_empty() || !citations.is_empty() {
        content.push(HubPart::Text {
            text,
            signature: None,
            origin: None,
            citations,
        });
    }
    content.extend(tool_call_parts(message));
    Ok(HubResponse {
        id: str_or_empty(body, "id"),
        model: str_or_empty(body, "model"),
        content,
        finish: decode_finish(choice["finish_reason"].as_str()),
        usage: body.get("usage").map(decode_usage).unwrap_or_default(),
    })
}

/// Decodes a client's request body; system and developer messages join into the system prompt.
pub(super) fn request(body: &Value, hint: &InboundHint) -> Result<HubRequest, AppError> {
    if !body.is_object() {
        return Err(AppError::validation("request body must be a JSON object"));
    }
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .ok_or_else(|| AppError::validation("`model` is required"))?;
    let raw_messages = body
        .get("messages")
        .and_then(Value::as_array)
        .filter(|m| !m.is_empty())
        .ok_or_else(|| AppError::validation("`messages` must be a non-empty array"))?;
    let mut system = Vec::new();
    let mut messages = Vec::new();
    for message in raw_messages {
        decode_message(message, &mut system, &mut messages)?;
    }
    let max_tokens = ["max_completion_tokens", "max_tokens"]
        .iter()
        .find_map(|key| body.get(*key).and_then(Value::as_u64))
        .map(to_u32);
    Ok(HubRequest {
        model: model.to_owned(),
        system: (!system.is_empty()).then(|| system.join("\n\n")),
        messages,
        tools: decode_tools(body)?,
        native_tools: decode_native_tools(body),
        tool_choice: decode_tool_choice(&body["tool_choice"]),
        parallel_tool_calls: body["parallel_tool_calls"].as_bool(),
        max_tokens,
        temperature: body["temperature"].as_f64(),
        top_p: body["top_p"].as_f64(),
        top_k: None,
        stop: decode_stop(&body["stop"]),
        seed: body["seed"].as_i64(),
        presence_penalty: body["presence_penalty"].as_f64(),
        frequency_penalty: body["frequency_penalty"].as_f64(),
        stream: body["stream"].as_bool().unwrap_or(false) || hint.stream,
        response_format: decode_response_format(&body["response_format"])?,
        reasoning: decode_reasoning(body),
        user: body["user"].as_str().map(str::to_owned),
        cache: crate::llm::adapters::shared::prompt_cache_key(body),
    })
}

fn decode_message(
    message: &Value,
    system: &mut Vec<String>,
    messages: &mut Vec<HubMessage>,
) -> Result<(), AppError> {
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::validation("message without a `role`"))?;
    let content = &message["content"];
    match role {
        "system" | "developer" => {
            let text = content_text(content);
            if !text.is_empty() {
                system.push(text);
            }
        }
        "user" => {
            let parts = decode_user_parts(content)?;
            if !parts.is_empty() {
                messages.push(HubMessage {
                    role: HubRole::User,
                    content: parts,
                });
            }
        }
        "assistant" => {
            if let Some(assistant) = decode_assistant(message) {
                messages.push(assistant);
            }
        }
        "tool" => {
            let tool = decode_tool_message(message, messages)?;
            messages.push(tool);
        }
        other => {
            return Err(AppError::validation(format!(
                "unsupported message role `{other}`"
            )));
        }
    }
    Ok(())
}

/// An assistant message: reasoning, text, then tool calls; `None` when it has none of them.
fn decode_assistant(message: &Value) -> Option<HubMessage> {
    let mut parts = Vec::new();
    if let Some(text) = reasoning_text(message) {
        parts.push(reasoning_part(text));
    }
    let text = content_text(&message["content"]);
    if !text.is_empty() {
        parts.push(HubPart::text(text));
    }
    parts.extend(tool_call_parts(message));
    (!parts.is_empty()).then_some(HubMessage {
        role: HubRole::Assistant,
        content: parts,
    })
}

/// A tool message; its name comes from the latest earlier call with the same id.
fn decode_tool_message(message: &Value, messages: &[HubMessage]) -> Result<HubMessage, AppError> {
    let call_id = message
        .get("tool_call_id")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::validation("tool message without `tool_call_id`"))?;
    let name = messages
        .iter()
        .rev()
        .flat_map(|m| m.content.iter())
        .find_map(|part| match part {
            HubPart::ToolCall { id, name, .. } if id == call_id => Some(name.clone()),
            _ => None,
        });
    let content = decode_user_parts(&message["content"])?;
    Ok(HubMessage {
        role: HubRole::Tool,
        content: vec![HubPart::ToolResult {
            call_id: call_id.to_owned(),
            name,
            content,
            is_error: false,
        }],
    })
}

fn decode_user_parts(content: &Value) -> Result<Vec<HubPart>, AppError> {
    match content {
        Value::String(text) if !text.is_empty() => Ok(vec![HubPart::text(text.clone())]),
        Value::Array(parts) => {
            let mut out = Vec::new();
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = part["text"].as_str().filter(|t| !t.is_empty()) {
                            out.push(HubPart::text(text));
                        }
                    }
                    Some("image_url") => {
                        let url = part["image_url"]["url"]
                            .as_str()
                            .or_else(|| part["image_url"].as_str())
                            .ok_or_else(|| {
                                AppError::validation("`image_url` part without a url")
                            })?;
                        out.push(HubPart::Image {
                            image: decode_image_url(url)?,
                        });
                    }
                    Some("file") => out.push(decode_file(&part["file"])?),
                    Some("input_audio") => out.push(decode_audio(&part["input_audio"])?),
                    other => {
                        return Err(AppError::unsupported(format!(
                            "`{}` content parts are not supported",
                            other.unwrap_or("untyped")
                        )));
                    }
                }
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

fn decode_file(file: &Value) -> Result<HubPart, AppError> {
    if non_null(file, "file_id").is_some() {
        return Err(AppError::unsupported(
            "`file` content parts by `file_id` are not supported; send `file_data` inline",
        ));
    }
    let (media_type, data) = file["file_data"]
        .as_str()
        .and_then(parse_base64_data_url)
        .ok_or_else(|| {
            AppError::validation("`file` part needs `file_data` as a base64 `data:` URL")
        })?;
    Ok(HubPart::Document {
        document: HubDocument {
            media_type: media_type.to_owned(),
            name: file["filename"]
                .as_str()
                .filter(|n| !n.is_empty())
                .map(str::to_owned),
            source: HubDocumentSource::Base64 {
                data: data.to_owned(),
            },
        },
    })
}

fn decode_audio(audio: &Value) -> Result<HubPart, AppError> {
    let data = audio["data"]
        .as_str()
        .ok_or_else(|| AppError::validation("`input_audio` part without `data`"))?;
    let media_type = match audio["format"].as_str() {
        Some("wav") => "audio/wav",
        Some("mp3") => "audio/mpeg",
        other => {
            return Err(AppError::unsupported(format!(
                "`input_audio` format `{}` is not supported (only wav and mp3)",
                other.unwrap_or("missing")
            )));
        }
    };
    Ok(HubPart::Audio {
        media_type: media_type.to_owned(),
        data: data.to_owned(),
    })
}

/// An image URL: `http(s)` as is, or a base64 `data:` URL; each malformed shape has its own
/// message.
fn decode_image_url(url: &str) -> Result<HubImage, AppError> {
    if let Some(rest) = url.strip_prefix("data:") {
        let (meta, data) = rest
            .split_once(',')
            .ok_or_else(|| AppError::validation("malformed `data:` image URL"))?;
        let media_type = meta
            .strip_suffix(";base64")
            .ok_or_else(|| AppError::validation("`data:` image URLs must be base64"))?;
        return Ok(HubImage::Base64 {
            media_type: media_type.to_owned(),
            data: data.to_owned(),
        });
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return Ok(HubImage::Url {
            url: url.to_owned(),
        });
    }
    Err(AppError::validation(
        "image URL must be `http(s)` or a base64 `data:` URL",
    ))
}

fn decode_tools(body: &Value) -> Result<Vec<HubTool>, AppError> {
    let Some(tools) = body.get("tools").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    tools
        .iter()
        .map(|tool| {
            let kind = tool["type"].as_str();
            if kind != Some("function") {
                return Err(AppError::unsupported(format!(
                    "`{}` tools are not supported (only `function`; use `web_search_options` for web search)",
                    kind.unwrap_or("untyped")
                )));
            }
            let function = &tool["function"];
            let name = function["name"]
                .as_str()
                .ok_or_else(|| AppError::validation("function tool without a `name`"))?;
            Ok(HubTool {
                name: name.to_owned(),
                description: function["description"].as_str().map(str::to_owned),
                parameters: non_null(function, "parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                strict: function["strict"].as_bool(),
            })
        })
        .collect()
}

fn decode_native_tools(body: &Value) -> Vec<HubNativeTool> {
    non_null(body, "web_search_options")
        .map(|_| HubNativeTool::WebSearch {
            max_uses: None,
            allowed_domains: Vec::new(),
            blocked_domains: Vec::new(),
        })
        .into_iter()
        .collect()
}

fn decode_tool_choice(choice: &Value) -> Option<HubToolChoice> {
    match choice {
        Value::String(mode) => match mode.as_str() {
            "auto" => Some(HubToolChoice::Auto),
            "none" => Some(HubToolChoice::None),
            "required" => Some(HubToolChoice::Required),
            _ => None,
        },
        Value::Object(_) if choice["type"].as_str() == Some("function") => {
            Some(HubToolChoice::Tool {
                name: choice["function"]["name"].as_str()?.to_owned(),
            })
        }
        _ => None,
    }
}

fn decode_response_format(format: &Value) -> Result<Option<HubResponseFormat>, AppError> {
    match format["type"].as_str() {
        Some("text") => Ok(Some(HubResponseFormat::Text)),
        Some("json_object") => Ok(Some(HubResponseFormat::JsonObject)),
        Some("json_schema") => {
            let spec = &format["json_schema"];
            let schema = non_null(spec, "schema").cloned().ok_or_else(|| {
                AppError::validation("`json_schema` response format needs a `schema`")
            })?;
            Ok(Some(HubResponseFormat::JsonSchema {
                name: spec["name"].as_str().unwrap_or("response").to_owned(),
                schema,
                strict: spec["strict"].as_bool(),
            }))
        }
        _ => Ok(None),
    }
}

fn decode_stop(stop: &Value) -> Vec<String> {
    match stop {
        Value::String(single) => vec![single.clone()],
        Value::Array(many) => many
            .iter()
            .filter_map(|s| s.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}
