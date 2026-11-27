//! Default AI request factory, wire JSON <-> builder converters, and display formatters.

use spanbay_desktop::core::app::current_time_ms;
use spanbay_desktop::core::llm::usage::Cost;
use spanbay_desktop::core::llm::wire::Protocol;
use spanbay_desktop::core::model::ai_request::{AiRequestConfig, AiTarget, RequestMode};
use spanbay_desktop::core::model::models::Assertion;
use spanbay_desktop::core::model::{HttpMethod, RawLanguage, Request, RequestBody};

use super::model_tabs::{AiMessageDraft, AiParamsDraft};

/// Builds a fresh default AI API `Request` using the first available enabled model if provided.
#[must_use]
pub fn default_ai_request(
    workspace_id: &str,
    name: &str,
    first_model: Option<(&str, &str, &str, &str)>,
) -> Option<Request> {
    let id = format!("req-{}", current_time_ms());
    let mut req = Request::new(
        &id,
        workspace_id,
        name,
        "https://api.openai.com/v1/chat/completions",
    )
    .ok()?;
    req.method = HttpMethod::Post;
    req.mode = RequestMode::AiApi;
    let (target, model_name) = if let Some((pid, pname, base_url, mid)) = first_model {
        req.url = spanbay_desktop::core::model::ai_request::non_routable_label(
            pname,
            "/v1/chat/completions",
        );
        (
            AiTarget::Provider {
                provider_id: pid.to_owned(),
                provider_name: pname.to_owned(),
                base_url_hint: base_url.to_owned(),
                model: mid.to_owned(),
            },
            mid.to_owned(),
        )
    } else {
        req.url = "https://api.openai.com/v1/chat/completions".into();
        (
            AiTarget::Custom {
                model: "gpt-4o-mini".into(),
            },
            "gpt-4o-mini".into(),
        )
    };
    req.ai = Some(AiRequestConfig {
        protocol: Protocol::OpenaiChat,
        target,
        builder: None,
        compare_models: Vec::new(),
    });
    let payload = serde_json::json!({
        "model": model_name,
        "stream": true,
        "temperature": 0.2,
        "max_tokens": 1024,
        "messages": [
            { "role": "system", "content": "You are a concise, accurate assistant." },
            { "role": "user", "content": "Reply with the single word pong." }
        ]
    });
    req.body = RequestBody::Json {
        content: serde_json::to_string_pretty(&payload).unwrap_or_default(),
    };
    Some(req)
}

/// Parses structured AI messages, sampling parameters, and tools JSON from a request body.
#[must_use]
pub fn parse_ai_builder_from_request(
    req: &Request,
) -> (Vec<AiMessageDraft>, AiParamsDraft, String) {
    let raw = match &req.body {
        RequestBody::Json { content } | RequestBody::RawText { content, .. } => content.as_str(),
        _ => "",
    };
    let mut messages = Vec::new();
    let mut params = AiParamsDraft::default();
    let mut tools_json = String::new();

    if let Ok(val) = serde_json::from_str::<serde_json::Value>(raw) {
        if let Some(sys) = val.get("system").and_then(|v| v.as_str())
            && !sys.is_empty()
        {
            messages.push(AiMessageDraft {
                role: "system".into(),
                content: sys.to_owned(),
            });
        }
        if let Some(arr) = val.get("messages").and_then(|v| v.as_array()) {
            for item in arr {
                let role = item
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("user")
                    .to_owned();
                let content = item
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| {
                        item.get("content")
                            .map(|v| v.to_string())
                            .unwrap_or_default()
                    });
                messages.push(AiMessageDraft { role, content });
            }
        } else if let Some(input) = val.get("input").and_then(|v| v.as_str()) {
            messages.push(AiMessageDraft {
                role: "user".into(),
                content: input.to_owned(),
            });
        }
        if let Some(t) = val.get("temperature") {
            params.temperature = t.to_string();
        }
        if let Some(m) = val
            .get("max_tokens")
            .or_else(|| val.get("max_output_tokens"))
        {
            params.max_tokens = m.to_string();
        }
        if let Some(tp) = val.get("top_p") {
            params.top_p = tp.to_string();
        }
        if let Some(st) = val.get("stream").and_then(|v| v.as_bool()) {
            params.stream = st;
        }
        if let Some(re) = val.get("reasoning_effort").and_then(|v| v.as_str()) {
            params.reasoning_effort = re.to_owned();
        }
        if let Some(tools) = val.get("tools") {
            tools_json = serde_json::to_string_pretty(tools).unwrap_or_default();
        }
    }

    if messages.is_empty() {
        messages.push(AiMessageDraft {
            role: "user".into(),
            content: String::new(),
        });
    }
    (messages, params, tools_json)
}

/// Builds a wire JSON request body from the AI builder state for any of the 5 AI protocols.
#[must_use]
pub fn build_ai_body_json(
    protocol: Protocol,
    model: &str,
    messages: &[AiMessageDraft],
    params: &AiParamsDraft,
    tools_json: &str,
) -> String {
    let mut root = serde_json::Map::new();
    match protocol {
        Protocol::Embeddings => {
            root.insert("model".into(), serde_json::Value::String(model.to_owned()));
            let text = messages
                .iter()
                .rev()
                .find(|m| !m.content.trim().is_empty())
                .map(|m| m.content.clone())
                .unwrap_or_default();
            root.insert("input".into(), serde_json::Value::String(text));
        }
        Protocol::AnthropicMessages => {
            root.insert("model".into(), serde_json::Value::String(model.to_owned()));
            let max_t: u64 = params.max_tokens.trim().parse().unwrap_or(1024);
            root.insert("max_tokens".into(), serde_json::json!(max_t));
            root.insert("stream".into(), serde_json::Value::Bool(params.stream));
            let sys: Vec<&str> = messages
                .iter()
                .filter(|m| m.role == "system" && !m.content.trim().is_empty())
                .map(|m| m.content.as_str())
                .collect();
            if !sys.is_empty() {
                root.insert("system".into(), serde_json::Value::String(sys.join("\n\n")));
            }
            let chat: Vec<serde_json::Value> = messages
                .iter()
                .filter(|m| m.role != "system")
                .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
                .collect();
            root.insert("messages".into(), serde_json::Value::Array(chat));
        }
        Protocol::OpenaiResponses => {
            root.insert("model".into(), serde_json::Value::String(model.to_owned()));
            root.insert("stream".into(), serde_json::Value::Bool(params.stream));
            let sys: Vec<&str> = messages
                .iter()
                .filter(|m| m.role == "system" && !m.content.trim().is_empty())
                .map(|m| m.content.as_str())
                .collect();
            if !sys.is_empty() {
                root.insert(
                    "instructions".into(),
                    serde_json::Value::String(sys.join("\n\n")),
                );
            }
            let input: Vec<serde_json::Value> = messages
                .iter()
                .filter(|m| m.role != "system")
                .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
                .collect();
            root.insert("input".into(), serde_json::Value::Array(input));
            if let Ok(max_t) = params.max_tokens.trim().parse::<u64>() {
                root.insert("max_output_tokens".into(), serde_json::json!(max_t));
            }
        }
        Protocol::Gemini => {
            let contents: Vec<serde_json::Value> = messages
                .iter()
                .filter(|m| m.role != "system")
                .map(|m| {
                    let role = if m.role == "assistant" {
                        "model"
                    } else {
                        "user"
                    };
                    serde_json::json!({ "role": role, "parts": [{ "text": m.content }] })
                })
                .collect();
            root.insert("contents".into(), serde_json::Value::Array(contents));
            let sys: Vec<&str> = messages
                .iter()
                .filter(|m| m.role == "system" && !m.content.trim().is_empty())
                .map(|m| m.content.as_str())
                .collect();
            if !sys.is_empty() {
                root.insert(
                    "systemInstruction".into(),
                    serde_json::json!({ "parts": [{ "text": sys.join("\n\n") }] }),
                );
            }
            let mut gen_cfg = serde_json::Map::new();
            if let Ok(t) = params.temperature.trim().parse::<f64>() {
                gen_cfg.insert("temperature".into(), serde_json::json!(t));
            }
            if let Ok(m) = params.max_tokens.trim().parse::<u64>() {
                gen_cfg.insert("maxOutputTokens".into(), serde_json::json!(m));
            }
            if !gen_cfg.is_empty() {
                root.insert(
                    "generationConfig".into(),
                    serde_json::Value::Object(gen_cfg),
                );
            }
        }
        _ => {
            // OpenAI Chat Completions default.
            root.insert("model".into(), serde_json::Value::String(model.to_owned()));
            root.insert("stream".into(), serde_json::Value::Bool(params.stream));
            let msgs: Vec<serde_json::Value> = messages
                .iter()
                .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
                .collect();
            root.insert("messages".into(), serde_json::Value::Array(msgs));
            if let Ok(t) = params.temperature.trim().parse::<f64>() {
                root.insert("temperature".into(), serde_json::json!(t));
            }
            if let Ok(m) = params.max_tokens.trim().parse::<u64>() {
                root.insert("max_tokens".into(), serde_json::json!(m));
            }
            if let Ok(tp) = params.top_p.trim().parse::<f64>() {
                root.insert("top_p".into(), serde_json::json!(tp));
            }
            if !params.reasoning_effort.trim().is_empty() {
                root.insert(
                    "reasoning_effort".into(),
                    serde_json::Value::String(params.reasoning_effort.trim().to_owned()),
                );
            }
        }
    }

    if !tools_json.trim().is_empty()
        && let Ok(tools_val) = serde_json::from_str::<serde_json::Value>(tools_json)
        && protocol != Protocol::Embeddings
    {
        root.insert("tools".into(), tools_val);
    }

    serde_json::to_string_pretty(&serde_json::Value::Object(root)).unwrap_or_default()
}

#[must_use]
pub fn method_str(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Head => "HEAD",
        HttpMethod::Options => "OPTIONS",
    }
}

#[must_use]
pub fn ai_protocol_label(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::OpenaiChat => "Chat Completions",
        Protocol::OpenaiResponses => "OpenAI Responses",
        Protocol::AnthropicMessages => "Anthropic Messages",
        Protocol::Gemini => "Gemini",
        Protocol::Embeddings => "Embeddings",
        _ => "AI API",
    }
}

#[must_use]
pub fn raw_lang_label(lang: Option<RawLanguage>) -> &'static str {
    match lang.unwrap_or(RawLanguage::Text) {
        RawLanguage::Text => "Text",
        RawLanguage::Javascript => "JavaScript",
        RawLanguage::Html => "HTML",
        RawLanguage::Xml => "XML",
    }
}

#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", (bytes as f64) / 1024.0)
    } else {
        format!("{:.2} MB", (bytes as f64) / (1024.0 * 1024.0))
    }
}

#[must_use]
pub fn format_cost_short(cost: &Cost) -> String {
    let amount = (cost.micro as f64) / 1_000_000.0;
    let sym = match cost.currency.as_str() {
        "CNY" => "¥",
        "USD" => "$",
        other => other,
    };
    if amount < 0.001 && amount > 0.0 {
        format!("{sym}{amount:.6}")
    } else {
        format!("{sym}{amount:.4}")
    }
}

#[must_use]
pub fn format_time_hms(ts_ms: i64) -> String {
    let total_s = (ts_ms.max(0) / 1000) % 86_400;
    let h = total_s / 3600;
    let m = (total_s % 3600) / 60;
    let s = total_s % 60;
    let ms = ts_ms.max(0) % 1000;
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

#[must_use]
pub fn assertion_short_label(a: &Assertion) -> (String, String, String) {
    match a {
        Assertion::StatusEquals { value } => ("status".into(), "==".into(), value.to_string()),
        Assertion::StatusIn { values } => (
            "status".into(),
            "in".into(),
            values
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        Assertion::HeaderExists { key } => (format!("header:{key}"), "exists".into(), "—".into()),
        Assertion::HeaderEquals { key, value } => {
            (format!("header:{key}"), "==".into(), value.clone())
        }
        Assertion::JsonPathExists { path } => (path.clone(), "exists".into(), "—".into()),
        Assertion::JsonPathEquals { path, value } => (path.clone(), "==".into(), value.to_string()),
        Assertion::BodyContains { value } => ("body".into(), "contains".into(), value.clone()),
        Assertion::ResponseTimeLessThanMs { value } => {
            ("time_ms".into(), "<".into(), format!("{value} ms"))
        }
        Assertion::BodySizeLessThanBytes { value } => {
            ("size_bytes".into(), "<".into(), format!("{value} B"))
        }
        Assertion::AiReplyContains { value } => {
            ("ai.reply".into(), "contains".into(), value.clone())
        }
        Assertion::AiReplyMatches { pattern } => {
            ("ai.reply".into(), "matches".into(), pattern.clone())
        }
        Assertion::AiJsonSchemaValid { schema } => (
            "ai.reply".into(),
            "schema".into(),
            schema.to_string().chars().take(60).collect(),
        ),
        Assertion::AiTtftLessThanMs { value } => {
            ("ai.ttft_ms".into(), "<".into(), format!("{value} ms"))
        }
        Assertion::AiTokensLessThan { value } => {
            ("ai.tokens".into(), "<".into(), value.to_string())
        }
        Assertion::AiNoError {} => ("ai.status".into(), "no_error".into(), "ok".into()),
        Assertion::Check { target, op, value } => {
            let t = match target {
                spanbay_desktop::core::model::models::AssertionTarget::Status => "status".into(),
                spanbay_desktop::core::model::models::AssertionTarget::Header { name } => {
                    format!("header:{name}")
                }
                spanbay_desktop::core::model::models::AssertionTarget::JsonPath { path } => {
                    path.clone()
                }
                spanbay_desktop::core::model::models::AssertionTarget::Body => "body".into(),
                spanbay_desktop::core::model::models::AssertionTarget::ResponseTimeMs => {
                    "time_ms".into()
                }
                spanbay_desktop::core::model::models::AssertionTarget::BodySizeBytes => {
                    "size_bytes".into()
                }
            };
            let op_s = format!("{op:?}").to_lowercase();
            let v = if value.is_null() {
                "—".into()
            } else {
                value.to_string()
            };
            (t, op_s, v)
        }
        Assertion::JsonSchemaValid { schema, path } => (
            path.clone().unwrap_or_else(|| "$".into()),
            "schema".into(),
            schema.to_string().chars().take(60).collect(),
        ),
    }
}
