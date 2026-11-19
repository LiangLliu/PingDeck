//! Request options in both directions: function and native tools, the tool choice, the text
//! format and reasoning.

use serde_json::{Map, Value, json};

use super::WEB_SEARCH;
use crate::llm::adapters::{
    EncodeCtx, as_u32, effort_str, insert_some, object, parse_effort, str_of,
};
use crate::llm::hub::{
    HubNativeTool, HubReasoning, HubRequest, HubResponseFormat, HubTool, HubToolChoice,
};
use crate::llm::wire::ReasoningStyle;
use pingdeck_base::error::AppError;

// --- Encoding toward an upstream ---

/// The `tools` list: the function tools left by the tool choice's allow list, then the native
/// tools; `None` when the request declares no tool at all.
pub(super) fn tools_json(req: &HubRequest) -> Result<Option<Vec<Value>>, AppError> {
    if req.tools.is_empty() && req.native_tools.is_empty() {
        return Ok(None);
    }
    let mut tools: Vec<Value> = req.tools_for_forced_choice().map(tool_json).collect();
    for native in &req.native_tools {
        tools.push(native_tool_json(native)?);
    }
    Ok(Some(tools))
}

/// The `text` object for a structured output format; `None` for plain text.
pub(super) fn text_format_json(format: Option<&HubResponseFormat>) -> Option<Value> {
    let format = match format? {
        HubResponseFormat::JsonObject => json!({"type": "json_object"}),
        HubResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        } => {
            let mut format = Map::new();
            format.insert("type".into(), "json_schema".into());
            format.insert("name".into(), name.as_str().into());
            format.insert("schema".into(), schema.clone());
            insert_some(&mut format, "strict", *strict);
            Value::Object(format)
        }
        HubResponseFormat::Text => return None,
    };
    Some(object([("format", format)]))
}

/// Writes `reasoning` and, unless reasoning is switched off, `include`, so the encrypted
/// reasoning comes back for the next turn. Nothing for providers that take no reasoning field.
pub(super) fn insert_reasoning(
    req: &HubRequest,
    ctx: &EncodeCtx<'_>,
    body: &mut Map<String, Value>,
) {
    let reasoning = req
        .reasoning
        .as_ref()
        .filter(|_| ctx.quirks.reasoning_request != Some(ReasoningStyle::None));
    let Some(reasoning) = reasoning else {
        return;
    };
    let mut object = Map::new();
    if reasoning.enabled == Some(false) {
        object.insert("effort".into(), json!("none"));
    } else {
        insert_some(&mut object, "effort", reasoning.effort.map(effort_str));
        object.insert("summary".into(), json!("auto"));
        body.insert("include".into(), json!(["reasoning.encrypted_content"]));
    }
    body.insert("reasoning".into(), Value::Object(object));
}

/// A function tool declaration.
fn tool_json(tool: &HubTool) -> Value {
    let mut object = Map::new();
    object.insert("type".into(), json!("function"));
    object.insert("name".into(), json!(tool.name));
    insert_some(&mut object, "description", tool.description.as_deref());
    object.insert("parameters".into(), tool.parameters.clone());
    // Responses treats a missing `strict` as true, which rejects the loose schemas most tools
    // have: unset means not strict.
    object.insert("strict".into(), json!(tool.strict.unwrap_or(false)));
    Value::Object(object)
}

/// A native web search declaration; only an allow list of domains can be expressed.
fn native_tool_json(tool: &HubNativeTool) -> Result<Value, AppError> {
    let HubNativeTool::WebSearch {
        allowed_domains,
        blocked_domains,
        ..
    } = tool;
    if !blocked_domains.is_empty() {
        return Err(AppError::unsupported(
            "OpenAI Responses: web search `blocked_domains` is not supported, only \
             `allowed_domains`",
        ));
    }
    let mut entry = Map::new();
    entry.insert("type".into(), WEB_SEARCH.into());
    if !allowed_domains.is_empty() {
        entry.insert(
            "filters".into(),
            json!({"allowed_domains": allowed_domains}),
        );
    }
    Ok(Value::Object(entry))
}

/// `max_tool_calls` from the web search limits: Responses has one global limit, so the smallest
/// wins.
pub(super) fn native_search_limit(tools: &[HubNativeTool]) -> Option<u32> {
    tools
        .iter()
        .filter_map(|HubNativeTool::WebSearch { max_uses, .. }| *max_uses)
        .min()
}

/// The wire tool choice. An allow list becomes `"required"`: [`tools_json`] already sent only the
/// listed tools (when none of them is declared the request still says `"required"`, a known gap
/// kept as it is).
pub(super) fn tool_choice_json(choice: &HubToolChoice) -> Value {
    match choice {
        HubToolChoice::Auto => json!("auto"),
        HubToolChoice::None => json!("none"),
        HubToolChoice::Required | HubToolChoice::Allowed { .. } => json!("required"),
        HubToolChoice::Tool { name } => json!({"type": "function", "name": name}),
    }
}

// --- Decoding a client request ---

/// The function tools and the native web search tools a client declares. `max_tool_calls` becomes
/// every web search's `max_uses`: an approximation, since it limits every tool call.
pub(super) fn decode_tools(
    request: &Map<String, Value>,
) -> Result<(Vec<HubTool>, Vec<HubNativeTool>), AppError> {
    let Some(Value::Array(declared)) = request.get("tools") else {
        return Ok((Vec::new(), Vec::new()));
    };
    let max_uses = request.get("max_tool_calls").and_then(as_u32);
    let (mut tools, mut native) = (Vec::new(), Vec::new());
    for tool in declared {
        let kind = str_of(tool, "type").unwrap_or("function");
        if kind == WEB_SEARCH || kind.starts_with("web_search_preview") {
            let allowed_domains = tool
                .pointer("/filters/allowed_domains")
                .and_then(Value::as_array)
                .map(|domains| {
                    domains
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            native.push(HubNativeTool::WebSearch {
                max_uses,
                allowed_domains,
                blocked_domains: Vec::new(),
            });
        } else {
            tools.push(decode_function_tool(tool, kind)?);
        }
    }
    Ok((tools, native))
}

/// A `function` tool; a missing schema becomes an empty object schema.
fn decode_function_tool(tool: &Value, kind: &str) -> Result<HubTool, AppError> {
    if kind != "function" {
        return Err(AppError::unsupported(format!(
            "Responses tool type `{kind}` is not supported, only `function` and web search tools"
        )));
    }
    let name = str_of(tool, "name")
        .filter(|name| !name.is_empty())
        .ok_or_else(|| AppError::validation("function tool needs a `name`"))?;
    Ok(HubTool {
        name: name.to_owned(),
        description: str_of(tool, "description").map(str::to_owned),
        parameters: tool
            .get("parameters")
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or_else(|| json!({"type": "object"})),
        strict: tool.get("strict").and_then(Value::as_bool),
    })
}

/// A tool choice: a mode word or a named function.
pub(super) fn decode_tool_choice(choice: &Value) -> Result<HubToolChoice, AppError> {
    match choice {
        Value::String(mode) => match mode.as_str() {
            "auto" => Ok(HubToolChoice::Auto),
            "none" => Ok(HubToolChoice::None),
            "required" => Ok(HubToolChoice::Required),
            other => Err(AppError::validation(format!(
                "unknown tool_choice `{other}`"
            ))),
        },
        object => match (str_of(object, "type"), str_of(object, "name")) {
            (Some("function"), Some(name)) => Ok(HubToolChoice::Tool {
                name: name.to_owned(),
            }),
            _ => Err(AppError::unsupported(
                "only `auto`, `none`, `required` and function tool_choice are supported",
            )),
        },
    }
}

/// The structured output format of `text.format`; plain text is none. A schema without a name is
/// named `response`.
pub(super) fn decode_format(text: Option<&Value>) -> Result<Option<HubResponseFormat>, AppError> {
    let Some(format) = text.and_then(|t| t.get("format")) else {
        return Ok(None);
    };
    match str_of(format, "type") {
        Some("text") | None => Ok(None),
        Some("json_object") => Ok(Some(HubResponseFormat::JsonObject)),
        Some("json_schema") => Ok(Some(HubResponseFormat::JsonSchema {
            name: str_of(format, "name").unwrap_or("response").to_owned(),
            schema: format.get("schema").cloned().unwrap_or_else(|| json!({})),
            strict: format.get("strict").and_then(Value::as_bool),
        })),
        Some(other) => Err(AppError::unsupported(format!(
            "text format `{other}` is not supported"
        ))),
    }
}

/// The reasoning settings of `reasoning.effort`: `none` switches reasoning off; an unknown effort
/// reads as no setting.
pub(super) fn decode_reasoning(reasoning: Option<&Value>) -> Option<HubReasoning> {
    let effort = reasoning.and_then(|r| str_of(r, "effort"))?;
    let mut out = HubReasoning::default();
    if effort == "none" {
        out.enabled = Some(false);
    } else {
        out.effort = Some(parse_effort(effort)?);
    }
    Some(out)
}
