//! Hub to Chat Completions JSON: the request toward an upstream and the response body toward a
//! Chat client.

use std::borrow::Cow;

use serde_json::{Map, Value, json};

use super::reasoning::encode_reasoning;
use super::{
    DEFAULT_ID, encode_finish, encode_usage, non_empty_or, reasoning_text_of, response_annotations,
    tool_calls_json,
};
use crate::llm::adapters::{EncodeCtx, image_data_url, insert_some, object, unix_now};
use crate::llm::hub::{
    HubDocument, HubDocumentSource, HubImage, HubNativeTool, HubPart, HubRequest, HubResponse,
    HubResponseFormat, HubRole, HubTool, HubToolChoice, text_of,
};
use crate::llm::wire::{MaxTokensField, Quirks, ReasoningStyle};
use pingdeck_base::error::AppError;

/// The `/chat/completions` request body of a hub request.
pub(super) fn request(req: &HubRequest, ctx: &EncodeCtx<'_>) -> Result<Value, AppError> {
    let quirks = ctx.quirks;
    let mut body = Map::new();
    body.insert("model".into(), req.model.as_str().into());
    body.insert(
        "messages".into(),
        Value::Array(encode_messages(req, quirks)?),
    );
    // Checked after the messages, so their errors come first.
    if req.ends_with_assistant() {
        return Err(AppError::validation(
            "Chat Completions upstreams are not asked to continue an assistant message (prefill); end the conversation with a user or tool message",
        ));
    }
    if !req.tools.is_empty() {
        body.insert(
            "tools".into(),
            req.tools_for_forced_choice().map(encode_tool).collect(),
        );
    }
    encode_native_tools(&req.native_tools, &mut body)?;
    if let Some(choice) = &req.tool_choice {
        body.insert("tool_choice".into(), encode_tool_choice(choice));
    }
    insert_some(&mut body, "parallel_tool_calls", req.parallel_tool_calls);
    if let Some(format) = &req.response_format {
        body.insert("response_format".into(), encode_response_format(format));
    }
    if !req.stop.is_empty() {
        body.insert("stop".into(), json!(req.stop));
    }
    insert_some(&mut body, "seed", req.seed);
    insert_some(&mut body, "user", req.user.as_deref());
    insert_some(
        &mut body,
        "prompt_cache_key",
        req.cache.as_ref().and_then(|cache| cache.key.as_deref()),
    );
    insert_max_tokens(&mut body, req, quirks);
    insert_sampling(&mut body, req, quirks);
    if let (Some(style), Some(reasoning)) = (quirks.reasoning_request, &req.reasoning) {
        encode_reasoning(&mut body, style, reasoning);
    }
    insert_stream(&mut body, req, quirks);
    Ok(Value::Object(body))
}

/// The output cap, under the field the provider takes.
///
/// A quirk names the field; without one, OpenAI reasoning models (reasoning effort and no
/// sampling) take `max_completion_tokens` and everything else the classic `max_tokens`.
fn insert_max_tokens(body: &mut Map<String, Value>, req: &HubRequest, quirks: &Quirks) {
    let Some(max) = req.max_tokens else {
        return;
    };
    let reasoning_model = quirks.reasoning_request == Some(ReasoningStyle::ReasoningEffort)
        && quirks.drops_sampling();
    let key = match quirks.max_tokens_field {
        Some(MaxTokensField::MaxTokens) => "max_tokens",
        Some(MaxTokensField::MaxCompletionTokens) => "max_completion_tokens",
        None if reasoning_model => "max_completion_tokens",
        None => "max_tokens",
    };
    body.insert(key.into(), max.into());
}

/// The sampling parameters, unless the provider rejects them.
fn insert_sampling(body: &mut Map<String, Value>, req: &HubRequest, quirks: &Quirks) {
    if quirks.drops_sampling() {
        return;
    }
    insert_some(body, "temperature", req.temperature);
    insert_some(body, "top_p", req.top_p);
    insert_some(body, "top_k", req.top_k);
    insert_some(body, "presence_penalty", req.presence_penalty);
    insert_some(body, "frequency_penalty", req.frequency_penalty);
}

/// `stream`, with `stream_options.include_usage` so the last chunk reports usage, unless the
/// provider rejects the option.
fn insert_stream(body: &mut Map<String, Value>, req: &HubRequest, quirks: &Quirks) {
    if !req.stream {
        return;
    }
    body.insert("stream".into(), true.into());
    if quirks.stream_usage != Some(false) {
        body.insert("stream_options".into(), json!({"include_usage": true}));
    }
}

/// The response body toward a Chat client.
pub(super) fn response(response: &HubResponse) -> Value {
    let text = response.text();
    let reasoning = reasoning_text_of(&response.content);
    let calls = tool_calls_json(&response.content);
    let annotations = response_annotations(&response.content);
    let mut message = Map::new();
    message.insert("role".into(), "assistant".into());
    message.insert(
        "content".into(),
        if text.is_empty() {
            Value::Null
        } else {
            Value::String(text)
        },
    );
    if !annotations.is_empty() {
        message.insert("annotations".into(), Value::Array(annotations));
    }
    if !reasoning.is_empty() {
        message.insert("reasoning_content".into(), Value::String(reasoning));
    }
    if !calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(calls));
    }
    let choice = object([
        ("index", 0.into()),
        ("message", Value::Object(message)),
        ("finish_reason", encode_finish(&response.finish).into()),
    ]);
    object([
        ("id", non_empty_or(&response.id, DEFAULT_ID).into()),
        ("object", "chat.completion".into()),
        ("created", unix_now().into()),
        ("model", response.model.as_str().into()),
        ("choices", Value::Array(vec![choice])),
        ("usage", encode_usage(&response.usage)),
    ])
}

fn unresolved(kind: &str) -> AppError {
    AppError::validation(format!(
        "attachment {kind} must be resolved before the request is encoded"
    ))
}

/// The `messages` array: the system prompt (as `developer` when the provider asks for it),
/// then each message.
fn encode_messages(req: &HubRequest, quirks: &Quirks) -> Result<Vec<Value>, AppError> {
    let mut out = Vec::new();
    if let Some(system) = req.system.as_deref().filter(|s| !s.is_empty()) {
        let role = if quirks.developer_role_as_system == Some(false) {
            "developer"
        } else {
            "system"
        };
        out.push(json!({"role": role, "content": system}));
    }
    for message in &req.messages {
        match message.role {
            HubRole::Assistant => out.push(encode_assistant(&message.content, quirks)?),
            HubRole::User | HubRole::Tool => encode_user_turn(&message.content, &mut out)?,
        }
    }
    Ok(out)
}

/// A user or tool turn: each tool result becomes a `tool` message, then the rest one `user`
/// message.
///
/// Text-only content is sent as a plain string, which every server accepts; with any media it
/// is the part array. Errors follow the order of the parts.
fn encode_user_turn(parts: &[HubPart], out: &mut Vec<Value>) -> Result<(), AppError> {
    let is_media = |part: &HubPart| {
        matches!(
            part,
            HubPart::Image { .. } | HubPart::Document { .. } | HubPart::Audio { .. }
        )
    };
    let text_only = !parts.iter().any(is_media);
    let has_content = parts
        .iter()
        .any(|part| is_media(part) || matches!(part, HubPart::Text { .. }));
    let mut plain = String::new();
    let mut wire = Vec::new();
    for part in parts {
        match part {
            HubPart::ToolResult {
                call_id,
                content: result,
                ..
            } => out.push(object([
                ("role", "tool".into()),
                ("tool_call_id", call_id.as_str().into()),
                ("content", Value::String(tool_result_text(result)?)),
            ])),
            HubPart::Text { text, .. } if text_only => plain.push_str(text),
            HubPart::Text { text, .. } => wire.push(json!({"type": "text", "text": text})),
            HubPart::Image { image } => {
                let url = image_data_url(image).ok_or_else(|| unresolved("image"))?;
                wire.push(object([
                    ("type", "image_url".into()),
                    ("image_url", object([("url", Value::String(url))])),
                ]));
            }
            HubPart::Document { document } => wire.push(encode_document(document)?),
            HubPart::Audio { media_type, data } => wire.push(encode_audio(media_type, data)?),
            HubPart::ToolCall { .. }
            | HubPart::Reasoning { .. }
            | HubPart::ServerToolUse { .. }
            | HubPart::ServerToolResult { .. } => {}
        }
    }
    if !has_content {
        return Ok(());
    }
    let content = if text_only {
        Value::String(plain)
    } else {
        Value::Array(wire)
    };
    out.push(object([("role", "user".into()), ("content", content)]));
    Ok(())
}

/// The file name of a document: its own, else `document.<extension of its media type>`.
fn document_name(document: &HubDocument) -> String {
    if let Some(name) = document.name.as_deref().filter(|n| !n.is_empty()) {
        return name.to_owned();
    }
    let ext = match document.media_type.as_str() {
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "text/markdown" => "md",
        "text/csv" => "csv",
        "application/json" => "json",
        _ => "bin",
    };
    format!("document.{ext}")
}

/// A text document as a `<document name=".." type="..">` block.
fn document_block(document: &HubDocument, text: &str) -> String {
    let attr = |v: &str| {
        v.replace('&', "&amp;")
            .replace('"', "&quot;")
            .replace('<', "&lt;")
    };
    format!(
        "<document name=\"{}\" type=\"{}\">\n{text}\n</document>",
        attr(&document_name(document)),
        attr(&document.media_type),
    )
}

fn encode_document(document: &HubDocument) -> Result<Value, AppError> {
    match &document.source {
        HubDocumentSource::Base64 { data } => Ok(object([
            ("type", "file".into()),
            (
                "file",
                object([
                    ("filename", Value::String(document_name(document))),
                    (
                        "file_data",
                        Value::String(format!("data:{};base64,{data}", document.media_type)),
                    ),
                ]),
            ),
        ])),
        HubDocumentSource::Text { text } => Ok(object([
            ("type", "text".into()),
            ("text", Value::String(document_block(document, text))),
        ])),
        HubDocumentSource::Url { .. } => Err(AppError::unsupported(
            "openai_chat: document URLs are not supported (Chat Completions has no file URL; send the document inline)",
        )),
        HubDocumentSource::AttachmentRef { .. } => Err(unresolved("document")),
    }
}

fn encode_audio(media_type: &str, data: &str) -> Result<Value, AppError> {
    let format = match media_type.to_ascii_lowercase().as_str() {
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/mpeg" | "audio/mp3" => "mp3",
        _ => {
            return Err(AppError::unsupported(format!(
                "openai_chat: audio type `{media_type}` is not supported (only wav and mp3)"
            )));
        }
    };
    Ok(json!({"type": "input_audio", "input_audio": {"data": data, "format": format}}))
}

/// A tool result as text, since Chat tool messages are text-only: media become placeholders
/// and text documents `<document>` blocks, one per line.
fn tool_result_text(parts: &[HubPart]) -> Result<String, AppError> {
    let mut lines: Vec<Cow<'_, str>> = Vec::new();
    for part in parts {
        match part {
            HubPart::Text { text, .. } => lines.push(Cow::Borrowed(text)),
            HubPart::Image { image } => {
                if matches!(image, HubImage::AttachmentRef { .. }) {
                    return Err(unresolved("image"));
                }
                lines.push(Cow::Borrowed("[image]"));
            }
            HubPart::Document { document } => lines.push(Cow::Owned(match &document.source {
                HubDocumentSource::Text { text } => document_block(document, text),
                HubDocumentSource::Base64 { .. } | HubDocumentSource::Url { .. } => {
                    format!("[document: {}]", document_name(document))
                }
                HubDocumentSource::AttachmentRef { .. } => return Err(unresolved("document")),
            })),
            HubPart::Audio { .. } => {
                return Err(AppError::unsupported(
                    "openai_chat: tool results cannot carry audio (Chat tool messages are text-only)",
                ));
            }
            _ => {}
        }
    }
    Ok(lines.join("\n"))
}

/// Native tools: web search is `web_search_options`, which has no domain filters.
fn encode_native_tools(
    tools: &[HubNativeTool],
    body: &mut Map<String, Value>,
) -> Result<(), AppError> {
    for tool in tools {
        match tool {
            HubNativeTool::WebSearch {
                allowed_domains,
                blocked_domains,
                ..
            } => {
                if !allowed_domains.is_empty() || !blocked_domains.is_empty() {
                    return Err(AppError::unsupported(
                        "openai_chat: web search domain filters are not supported by `web_search_options`",
                    ));
                }
                body.insert("web_search_options".into(), json!({}));
            }
        }
    }
    Ok(())
}

/// An assistant message; reasoning is echoed back only for providers that require it.
fn encode_assistant(parts: &[HubPart], quirks: &Quirks) -> Result<Value, AppError> {
    if parts.iter().any(|part| {
        matches!(
            part,
            HubPart::Image { .. } | HubPart::Document { .. } | HubPart::Audio { .. }
        )
    }) {
        return Err(AppError::unsupported(
            "openai_chat: assistant messages cannot carry images, documents or audio",
        ));
    }
    let text = text_of(parts);
    let calls = tool_calls_json(parts);
    let mut message = Map::new();
    message.insert("role".into(), "assistant".into());
    message.insert(
        "content".into(),
        if text.is_empty() && !calls.is_empty() {
            Value::Null
        } else {
            Value::String(text)
        },
    );
    if quirks.echoes_reasoning() {
        let reasoning = reasoning_text_of(parts);
        if !reasoning.is_empty() {
            let field = quirks
                .reasoning_response_field
                .as_deref()
                .unwrap_or("reasoning_content");
            message.insert(field.into(), Value::String(reasoning));
        }
    }
    if !calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(calls));
    }
    Ok(Value::Object(message))
}

fn encode_tool(tool: &HubTool) -> Value {
    let mut function = Map::new();
    function.insert("name".into(), tool.name.as_str().into());
    insert_some(&mut function, "description", tool.description.as_deref());
    function.insert("parameters".into(), tool.parameters.clone());
    insert_some(&mut function, "strict", tool.strict);
    object([
        ("type", "function".into()),
        ("function", Value::Object(function)),
    ])
}

/// The `tool_choice`; Chat has no allow-list, so `Allowed` is `required` over the filtered
/// tools (see `HubRequest::tools_for_forced_choice`).
fn encode_tool_choice(choice: &HubToolChoice) -> Value {
    match choice {
        HubToolChoice::Auto => json!("auto"),
        HubToolChoice::None => json!("none"),
        HubToolChoice::Required | HubToolChoice::Allowed { .. } => json!("required"),
        HubToolChoice::Tool { name } => json!({"type": "function", "function": {"name": name}}),
    }
}

fn encode_response_format(format: &HubResponseFormat) -> Value {
    match format {
        HubResponseFormat::Text => json!({"type": "text"}),
        HubResponseFormat::JsonObject => json!({"type": "json_object"}),
        HubResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        } => {
            let mut inner = Map::new();
            inner.insert("name".into(), name.as_str().into());
            inner.insert("schema".into(), schema.clone());
            insert_some(&mut inner, "strict", *strict);
            object([
                ("type", "json_schema".into()),
                ("json_schema", Value::Object(inner)),
            ])
        }
    }
}
