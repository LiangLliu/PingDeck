//! `generationConfig`, `thinkingConfig`, `tools` and `toolConfig`, both directions.

use serde_json::{Map, Value, json};

use super::field;
use super::schema::schema_from_gemini;
use crate::llm::adapters::generation::{self, Thinking};
use crate::llm::adapters::{insert_some, object};
use crate::llm::hub::{
    HubEffort, HubNativeTool, HubReasoning, HubRequest, HubResponseFormat, HubTool, HubToolChoice,
};
use pingdeck_base::error::AppError;

/// The `generationConfig` of a request; `drop_sampling` leaves out the sampling parameters.
pub(super) fn generation_config(req: &HubRequest, drop_sampling: bool) -> Map<String, Value> {
    let mut config = Map::new();
    insert_some(&mut config, "maxOutputTokens", req.max_tokens);
    insert_some(&mut config, "seed", req.seed);
    if !req.stop.is_empty() {
        config.insert("stopSequences".into(), json!(req.stop));
    }
    if !drop_sampling {
        insert_some(&mut config, "temperature", req.temperature);
        insert_some(&mut config, "topP", req.top_p);
        insert_some(&mut config, "topK", req.top_k);
        insert_some(&mut config, "presencePenalty", req.presence_penalty);
        insert_some(&mut config, "frequencyPenalty", req.frequency_penalty);
    }
    match &req.response_format {
        Some(HubResponseFormat::JsonObject) => {
            config.insert("responseMimeType".into(), "application/json".into());
        }
        Some(HubResponseFormat::JsonSchema { schema, .. }) => {
            config.insert("responseMimeType".into(), "application/json".into());
            config.insert("responseJsonSchema".into(), schema.clone());
        }
        Some(HubResponseFormat::Text) | None => {}
    }
    let generation = generation::gemini(&req.model);
    if let Some(thinking) = req
        .reasoning
        .as_ref()
        .and_then(|reasoning| thinking_config(reasoning, generation))
    {
        config.insert("thinkingConfig".into(), thinking);
    }
    config
}

/// Gemini 2.x takes only `thinkingBudget`, 3 and later prefer `thinkingLevel`.
///
/// For 2.x an explicit budget wins; without one the effort is turned into a budget (see
/// [`effort_budget`]).
fn thinking_config(reasoning: &HubReasoning, generation: Thinking) -> Option<Value> {
    if reasoning.enabled == Some(false) {
        return Some(json!({"thinkingBudget": 0}));
    }
    if generation == Thinking::Budget
        && let Some(budget) = reasoning
            .budget_tokens
            .or_else(|| reasoning.effort.map(effort_budget))
    {
        return Some(json!({"includeThoughts": true, "thinkingBudget": budget}));
    }
    if let Some(effort) = reasoning.effort {
        let level = match effort {
            HubEffort::Minimal => "minimal",
            HubEffort::Low => "low",
            HubEffort::Medium => "medium",
            HubEffort::High | HubEffort::XHigh | HubEffort::Max => "high",
        };
        return Some(json!({"includeThoughts": true, "thinkingLevel": level}));
    }
    if let Some(budget) = reasoning.budget_tokens {
        return Some(json!({"includeThoughts": true, "thinkingBudget": budget}));
    }
    (reasoning.enabled == Some(true)).then(|| json!({"includeThoughts": true}))
}

/// The thinking budget for an effort level.
///
/// Low, medium and high map to the budgets Gemini's OpenAI-compatible endpoint uses for
/// `reasoning_effort` (1 024, 8 192 and 24 576 tokens); minimal gets 512 and the levels above
/// high stay at the high budget.
fn effort_budget(effort: HubEffort) -> u32 {
    match effort {
        HubEffort::Minimal => 512,
        HubEffort::Low => 1_024,
        HubEffort::Medium => 8_192,
        HubEffort::High | HubEffort::XHigh | HubEffort::Max => 24_576,
    }
}

fn function_declaration(tool: &HubTool) -> Value {
    let mut declaration = Map::new();
    declaration.insert("name".into(), tool.name.as_str().into());
    insert_some(&mut declaration, "description", tool.description.as_deref());
    declaration.insert("parametersJsonSchema".into(), tool.parameters.clone());
    Value::Object(declaration)
}

/// The `toolConfig` of a tool choice.
pub(super) fn tool_config(choice: &HubToolChoice) -> Value {
    let config = match choice {
        HubToolChoice::Auto => json!({"mode": "AUTO"}),
        HubToolChoice::None => json!({"mode": "NONE"}),
        HubToolChoice::Required => json!({"mode": "ANY"}),
        HubToolChoice::Tool { name } => json!({"mode": "ANY", "allowedFunctionNames": [name]}),
        HubToolChoice::Allowed { names } => json!({"mode": "ANY", "allowedFunctionNames": names}),
    };
    object([("functionCallingConfig", config)])
}

/// The `tools` array: one `functionDeclarations` entry, then `google_search` at most once.
pub(super) fn encode_tools(
    functions: &[HubTool],
    native: &[HubNativeTool],
) -> Result<Vec<Value>, AppError> {
    let mut out = Vec::new();
    if !functions.is_empty() {
        let declarations: Vec<Value> = functions.iter().map(function_declaration).collect();
        out.push(object([(
            "functionDeclarations",
            Value::Array(declarations),
        )]));
    }
    let mut searching = false;
    for tool in native {
        match tool {
            HubNativeTool::WebSearch {
                allowed_domains,
                blocked_domains,
                ..
            } => {
                if !allowed_domains.is_empty() || !blocked_domains.is_empty() {
                    return Err(AppError::unsupported(
                        "Gemini: web search domain filters (allowed_domains / blocked_domains) \
                         are not supported by google_search",
                    ));
                }
                if !searching {
                    searching = true;
                    out.push(json!({"google_search": {}}));
                }
            }
        }
    }
    Ok(out)
}

/// Reads a client's `generationConfig` into the request.
pub(super) fn apply_generation_config(req: &mut HubRequest, config: &Value) {
    let float = |key: &str| field(config, key).and_then(Value::as_f64);
    let small = |key: &str| {
        field(config, key)
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
    };
    req.max_tokens = small("maxOutputTokens");
    req.temperature = float("temperature");
    req.top_p = float("topP");
    req.top_k = small("topK");
    req.seed = field(config, "seed").and_then(Value::as_i64);
    req.presence_penalty = float("presencePenalty");
    req.frequency_penalty = float("frequencyPenalty");
    req.stop = field(config, "stopSequences")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_owned))
        .collect();
    let schema = field(config, "responseJsonSchema")
        .cloned()
        .or_else(|| field(config, "responseSchema").map(schema_from_gemini));
    req.response_format = match (
        schema,
        field(config, "responseMimeType").and_then(Value::as_str),
    ) {
        (Some(schema), _) => Some(HubResponseFormat::JsonSchema {
            name: "response".to_owned(),
            schema,
            strict: None,
        }),
        (None, Some("application/json")) => Some(HubResponseFormat::JsonObject),
        _ => None,
    };
    req.reasoning = field(config, "thinkingConfig").and_then(decode_thinking);
}

fn decode_thinking(config: &Value) -> Option<HubReasoning> {
    let mut reasoning = HubReasoning::default();
    if let Some(level) = field(config, "thinkingLevel").and_then(Value::as_str) {
        reasoning.effort = match level.to_ascii_lowercase().as_str() {
            "minimal" => Some(HubEffort::Minimal),
            "low" => Some(HubEffort::Low),
            "medium" => Some(HubEffort::Medium),
            "high" => Some(HubEffort::High),
            _ => None,
        };
    }
    match field(config, "thinkingBudget").and_then(Value::as_i64) {
        Some(0) => reasoning.enabled = Some(false),
        Some(budget) => reasoning.budget_tokens = u32::try_from(budget).ok(),
        None => {}
    }
    if reasoning == HubReasoning::default()
        && field(config, "includeThoughts").and_then(Value::as_bool) == Some(true)
    {
        reasoning.enabled = Some(true);
    }
    (reasoning != HubReasoning::default()).then_some(reasoning)
}

/// A hub tool of a function declaration; `parameters` (OpenAPI) is converted when
/// `parametersJsonSchema` is absent.
fn decode_tool(declaration: &Value) -> Result<HubTool, AppError> {
    let name = declaration
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::validation("a Gemini function declaration needs a `name`"))?;
    Ok(HubTool {
        name: name.to_owned(),
        description: declaration
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
        parameters: field(declaration, "parametersJsonSchema")
            .cloned()
            .or_else(|| declaration.get("parameters").map(schema_from_gemini))
            .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
        strict: None,
    })
}

/// The tool choice of a client's `toolConfig.functionCallingConfig`.
pub(super) fn decode_tool_choice(body: &Value) -> Option<HubToolChoice> {
    let config = field(field(body, "toolConfig")?, "functionCallingConfig")?;
    let mode = config.get("mode")?.as_str()?.to_ascii_uppercase();
    match mode.as_str() {
        "AUTO" => Some(HubToolChoice::Auto),
        "NONE" => Some(HubToolChoice::None),
        "ANY" | "VALIDATED" => {
            let names: Vec<&str> = field(config, "allowedFunctionNames")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            match names.as_slice() {
                [] => Some(HubToolChoice::Required),
                [name] => Some(HubToolChoice::Tool {
                    name: (*name).to_owned(),
                }),
                _ => Some(HubToolChoice::Allowed {
                    names: names.iter().map(|n| (*n).to_owned()).collect(),
                }),
            }
        }
        _ => None,
    }
}

/// A client's function declarations and native tools (any Google Search spelling becomes one
/// web search tool).
pub(super) fn decode_tools(body: &Value) -> Result<(Vec<HubTool>, Vec<HubNativeTool>), AppError> {
    let mut functions = Vec::new();
    let mut native = Vec::new();
    let Some(tools) = field(body, "tools") else {
        return Ok((functions, native));
    };
    let tools = tools
        .as_array()
        .ok_or_else(|| AppError::validation("Gemini `tools` must be an array"))?;
    for tool in tools {
        let tool = tool
            .as_object()
            .ok_or_else(|| AppError::validation("a Gemini tool must be an object"))?;
        for (kind, value) in tool {
            match kind.as_str() {
                "functionDeclarations" | "function_declarations" => {
                    let declarations = value.as_array().ok_or_else(|| {
                        AppError::validation("Gemini `functionDeclarations` must be an array")
                    })?;
                    for declaration in declarations {
                        functions.push(decode_tool(declaration)?);
                    }
                }
                "google_search"
                | "googleSearch"
                | "googleSearchRetrieval"
                | "google_search_retrieval" => {
                    if native.is_empty() {
                        native.push(HubNativeTool::WebSearch {
                            max_uses: None,
                            allowed_domains: Vec::new(),
                            blocked_domains: Vec::new(),
                        });
                    }
                }
                other => {
                    return Err(AppError::unsupported(format!(
                        "Gemini: the `{other}` tool is not supported; only function declarations \
                         and Google Search are"
                    )));
                }
            }
        }
    }
    Ok((functions, native))
}
