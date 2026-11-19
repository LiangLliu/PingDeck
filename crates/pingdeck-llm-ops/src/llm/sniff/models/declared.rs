//! The capability fields a list entry declares, one reader per vendor shape, and the kind they
//! imply. Readers run in a fixed order and a later kind replaces an earlier one: LM Studio
//! `type`, Gemini methods, new-api endpoint types, OpenRouter output modalities.

use serde_json::Value;

use super::ModelKind;
use crate::llm::capabilities::{Capability, CapabilitySource, ModelCapabilities};

/// The strings of an array value; nothing for anything else.
fn str_items(value: Option<&Value>) -> impl Iterator<Item = &str> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

/// `entry[key].supported` (Anthropic's capability objects).
fn supported(entry: &Value, key: &str) -> bool {
    entry
        .get(key)
        .and_then(|flag| flag.get("supported"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Marks `capability` supported from `source` when `on`.
pub(super) fn declare(
    caps: &mut ModelCapabilities,
    capability: Capability,
    on: bool,
    source: CapabilitySource,
) {
    if on {
        caps.apply(capability, true, source);
    }
}

/// Marks `capability` supported by the provider's own list entry when `on`.
fn declare_own(caps: &mut ModelCapabilities, capability: Capability, on: bool) {
    declare(caps, capability, on, CapabilitySource::Declared);
}

/// Anthropic `capabilities: {image_input, pdf_input, thinking, structured_outputs}`.
fn anthropic_caps(caps: &mut ModelCapabilities, object: &Value) {
    declare_own(caps, Capability::Vision, supported(object, "image_input"));
    declare_own(caps, Capability::Document, supported(object, "pdf_input"));
    declare_own(caps, Capability::Reasoning, supported(object, "thinking"));
    declare_own(
        caps,
        Capability::JsonSchema,
        supported(object, "structured_outputs"),
    );
}

/// LM Studio v1 `capabilities: {vision, trained_for_tool_use, reasoning.allowed_options}`.
fn lm_studio_v1_caps(caps: &mut ModelCapabilities, object: &Value, reasoning: bool) {
    let flag = |name: &str| object.get(name).and_then(Value::as_bool) == Some(true);
    declare_own(caps, Capability::Vision, flag("vision"));
    declare_own(caps, Capability::Tools, flag("trained_for_tool_use"));
    declare_own(caps, Capability::Reasoning, reasoning);
}

/// LM Studio legacy `capabilities: ["tool_use"]` and `type`.
fn lm_studio_legacy(caps: &mut ModelCapabilities, entry: &Value) -> Option<ModelKind> {
    declare_own(
        caps,
        Capability::Tools,
        str_items(entry.get("capabilities")).any(|c| c == "tool_use"),
    );
    match entry.get("type").and_then(Value::as_str) {
        Some("vlm") => {
            declare_own(caps, Capability::Vision, true);
            None
        }
        Some("embeddings") => Some(ModelKind::Embedding),
        _ => None,
    }
}

/// Gemini `supportedGenerationMethods`: an embedder that cannot chat is an embedding model.
fn gemini_methods(caps: &mut ModelCapabilities, entry: &Value) -> Option<ModelKind> {
    let methods = || str_items(entry.get("supportedGenerationMethods"));
    let chat = methods().any(|m| m == "generateContent");
    let embed = methods().any(|m| m == "embedContent" || m == "batchEmbedContents");
    declare_own(caps, Capability::Embeddings, embed);
    (embed && !chat).then_some(ModelKind::Embedding)
}

/// Gemini's top-level `thinking: true`.
fn gemini_thinking_flag(caps: &mut ModelCapabilities, entry: &Value) {
    declare_own(
        caps,
        Capability::Reasoning,
        entry.get("thinking").and_then(Value::as_bool) == Some(true),
    );
}

/// new-api `supported_endpoint_types`: a model without a chat endpoint takes the kind of its
/// embedding, rerank or image endpoint.
fn new_api_endpoint_types(
    caps: &mut ModelCapabilities,
    endpoint_types: &[String],
) -> Option<ModelKind> {
    let has = |name: &str| endpoint_types.iter().any(|t| t == name);
    let chat = ["openai", "openai-response", "anthropic", "gemini"]
        .iter()
        .any(|t| has(t));
    if has("embeddings") {
        declare_own(caps, Capability::Embeddings, true);
        (!chat).then_some(ModelKind::Embedding)
    } else if has("jina-rerank") && !chat {
        Some(ModelKind::Rerank)
    } else if has("image-generation") && !chat {
        Some(ModelKind::Image)
    } else {
        None
    }
}

/// OpenRouter `architecture`: input modalities declare capabilities, output modalities the
/// kind; every modality is collected once, inputs first.
fn openrouter_architecture(
    caps: &mut ModelCapabilities,
    entry: &Value,
    modalities: &mut Vec<String>,
) -> Option<ModelKind> {
    let architecture = entry.get("architecture")?;
    let inputs = || str_items(architecture.get("input_modalities"));
    let outputs = || str_items(architecture.get("output_modalities"));
    declare_own(caps, Capability::Vision, inputs().any(|m| m == "image"));
    declare_own(caps, Capability::Document, inputs().any(|m| m == "file"));
    declare_own(caps, Capability::AudioIn, inputs().any(|m| m == "audio"));
    let kind = if outputs().any(|m| m == "image") {
        Some(ModelKind::Image)
    } else if outputs().any(|m| m == "embeddings") {
        Some(ModelKind::Embedding)
    } else {
        None
    };
    for modality in inputs().chain(outputs()) {
        if !modalities.iter().any(|known| known == modality) {
            modalities.push(modality.to_owned());
        }
    }
    kind
}

/// OpenRouter `supported_parameters`.
fn supported_parameters(caps: &mut ModelCapabilities, entry: &Value) {
    let has =
        |names: &[&str]| str_items(entry.get("supported_parameters")).any(|p| names.contains(&p));
    declare_own(caps, Capability::Tools, has(&["tools"]));
    declare_own(
        caps,
        Capability::Reasoning,
        has(&["reasoning", "include_reasoning"]),
    );
    declare_own(
        caps,
        Capability::JsonSchema,
        has(&["structured_outputs", "response_format"]),
    );
    declare_own(caps, Capability::WebSearch, has(&["web_search_options"]));
}

/// Reads the declared capability fields of one entry and the kind they imply. `reasoning` says
/// whether the entry lists LM Studio reasoning options; OpenRouter modalities go to
/// `modalities`.
pub(super) fn declared(
    entry: &Value,
    endpoint_types: &[String],
    reasoning: bool,
    modalities: &mut Vec<String>,
) -> (ModelCapabilities, Option<ModelKind>) {
    let mut caps = ModelCapabilities::default();
    if let Some(object) = entry.get("capabilities").filter(|value| value.is_object()) {
        anthropic_caps(&mut caps, object);
        lm_studio_v1_caps(&mut caps, object, reasoning);
    }
    let mut kind = lm_studio_legacy(&mut caps, entry);
    kind = gemini_methods(&mut caps, entry).or(kind);
    gemini_thinking_flag(&mut caps, entry);
    kind = new_api_endpoint_types(&mut caps, endpoint_types).or(kind);
    kind = openrouter_architecture(&mut caps, entry, modalities).or(kind);
    supported_parameters(&mut caps, entry);
    (caps, kind)
}
