//! Model-list parsing, capability prefill and kind derivation of the sniffer. Pure: no I/O.
//!
//! Any list body (OpenAI, new-api, vLLM, LM Studio, OpenRouter, Anthropic with `has_more`,
//! Gemini with `nextPageToken`, Ollama tags, a bare array) becomes [`SniffedModel`] rows; the
//! capabilities are prefilled in the order declared, catalogue, preset.

mod declared;

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use self::declared::{declare, declared};
use super::types::SniffedModel;
use crate::llm::{
    capabilities::{Capability, CapabilitySource, ModelCapabilities},
    presets::ProviderPreset,
};
use spanbay_model::catalog_entry::CatalogEntry;

/// Pages followed per list.
pub const MAX_PAGES: usize = 5;

/// The keys a list object keeps its entries under, in lookup order.
const LIST_KEYS: [&str; 3] = ["data", "models", "items"];

/// What a model is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    /// Chat or responses model.
    Chat,
    /// Embedding model.
    Embedding,
    /// Image generation.
    Image,
    /// Speech, transcription or audio models.
    Audio,
    /// Reranker.
    Rerank,
    /// Moderation model.
    Moderation,
    /// Anything else (video generation, OCR, …).
    Other,
}

impl ModelKind {
    /// The kind a catalogue `mode` names: `chat`, `responses` and `completion` are Chat, then
    /// `embedding`, `image_generation`, `rerank`, `moderation`, `audio_transcription` and
    /// `audio_speech`; any other mode names none.
    fn from_catalog_mode(mode: &str) -> Option<Self> {
        match mode {
            "chat" | "responses" | "completion" => Some(Self::Chat),
            "embedding" => Some(Self::Embedding),
            "image_generation" => Some(Self::Image),
            "rerank" => Some(Self::Rerank),
            "moderation" => Some(Self::Moderation),
            "audio_transcription" | "audio_speech" => Some(Self::Audio),
            _ => None,
        }
    }
}

/// Same-name variant: a scanned model's kind as a stored model's kind.
impl From<ModelKind> for crate::llm::probe::listing::ModelKind {
    fn from(kind: ModelKind) -> Self {
        match kind {
            ModelKind::Chat => Self::Chat,
            ModelKind::Embedding => Self::Embedding,
            ModelKind::Image => Self::Image,
            ModelKind::Audio => Self::Audio,
            ModelKind::Rerank => Self::Rerank,
            ModelKind::Moderation => Self::Moderation,
            ModelKind::Other => Self::Other,
        }
    }
}

/// Whether a listed model of `kind` starts enabled: Chat and Embedding do, every other kind is
/// saved disabled, for lists of any size.
#[must_use]
pub(super) const fn default_enabled(kind: ModelKind) -> bool {
    matches!(kind, ModelKind::Chat | ModelKind::Embedding)
}

/// The kind an id suggests; the last resort of the derivation.
#[must_use]
pub(super) fn kind_from_id(id: &str) -> ModelKind {
    let id = id.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| id.contains(needle));
    if has(&["embed"]) {
        ModelKind::Embedding
    } else if has(&[
        "dall-e",
        "gpt-image",
        "imagen",
        "flux",
        "stable-diffusion",
        "sdxl",
        "-image",
    ]) {
        ModelKind::Image
    } else if has(&["whisper", "tts", "audio"]) {
        ModelKind::Audio
    } else if has(&["rerank"]) {
        ModelKind::Rerank
    } else if has(&["moderation"]) {
        ModelKind::Moderation
    } else if has(&["sora", "veo-", "video", "-ocr", "ocr-"]) {
        ModelKind::Other
    } else {
        ModelKind::Chat
    }
}

/// A parsed entry before the catalogue and kind rules run.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RawModel {
    /// The row with declared data; `kind` is provisional.
    pub model: SniffedModel,
    /// The kind the list body declares.
    pub declared_kind: Option<ModelKind>,
}

/// Cursor of the next page of a model list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListCursor {
    /// Anthropic: `after_id=<last_id>`.
    AfterId(String),
    /// Gemini: `pageToken=<token>`.
    Token(String),
}

impl ListCursor {
    /// The query pair that asks for the page: `("after_id", id)` or `("pageToken", token)`.
    #[must_use]
    pub fn query_pair(&self) -> (&'static str, &str) {
        match self {
            Self::AfterId(id) => ("after_id", id),
            Self::Token(token) => ("pageToken", token),
        }
    }
}

/// One parsed page.
#[derive(Debug, Default)]
pub(super) struct ParsedPage {
    /// Entries in list order.
    pub models: Vec<RawModel>,
    /// The next page, when the body names one.
    pub next: Option<ListCursor>,
}

/// The trimmed string under `key`, when it is not blank.
fn text_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn text(value: &Value, key: &str) -> Option<String> {
    text_str(value, key).map(str::to_owned)
}

/// A positive count from a number (fractions truncated) or a numeric string.
fn number(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_f64().filter(|n| *n >= 0.0).map(|n| n as u64))
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        .filter(|n| *n > 0)
}

fn first_number(entry: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| entry.get(key).and_then(number))
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// USD per token → USD per million tokens; negative prices mean "variable" and are dropped.
fn per_mtok(value: Option<&Value>) -> Option<f64> {
    let price = value.and_then(|value| {
        value
            .as_f64()
            .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
    })?;
    (price >= 0.0).then_some(price * 1_000_000.0)
}

/// The reasoning-effort options a model declares (LM Studio v1 `reasoning.allowed_options`).
fn reasoning_options(entry: &Value) -> Vec<String> {
    strings(
        entry
            .get("capabilities")
            .and_then(|caps| caps.get("reasoning"))
            .and_then(|reasoning| reasoning.get("allowed_options")),
    )
}

/// The id of a list entry: a bare string, or the first id-like field of an object, with
/// Gemini's `models/` prefix cut; an id that is blank once cut does not count.
fn entry_id(entry: &Value) -> Option<String> {
    let id = |text: &str| {
        let id = text.trim().trim_start_matches("models/");
        (!id.is_empty()).then(|| id.to_owned())
    };
    if let Some(text) = entry.as_str() {
        return id(text);
    }
    ["id", "key", "name", "model", "slug"]
        .iter()
        .find_map(|key| text_str(entry, key).and_then(id))
}

fn parse_entry(entry: &Value) -> Option<RawModel> {
    let id = entry_id(entry)?;
    if !entry.is_object() {
        return Some(RawModel {
            model: bare_model(id),
            declared_kind: None,
        });
    }
    let endpoint_types = strings(entry.get("supported_endpoint_types"));
    let reasoning = reasoning_options(entry);
    let mut modalities = Vec::new();
    let (capabilities, declared_kind) = declared(
        entry,
        &endpoint_types,
        !reasoning.is_empty(),
        &mut modalities,
    );
    let display_name = ["display_name", "displayName", "name"]
        .iter()
        .find_map(|key| text_str(entry, key))
        .map(|name| name.trim_start_matches("models/"))
        .filter(|name| *name != id)
        .map(str::to_owned);
    let pricing = entry.get("pricing");
    let input_cost = per_mtok(pricing.and_then(|p| p.get("prompt")));
    let output_cost = per_mtok(pricing.and_then(|p| p.get("completion")));
    Some(RawModel {
        model: SniffedModel {
            display_name,
            owned_by: text(entry, "owned_by"),
            context_window: first_number(
                entry,
                &[
                    "max_context_length",
                    "max_model_len",
                    "context_length",
                    "inputTokenLimit",
                    "max_input_tokens",
                    "context_window",
                ],
            ),
            max_output: first_number(
                entry,
                &["outputTokenLimit", "max_tokens", "max_output_tokens"],
            )
            .or_else(|| {
                entry
                    .get("top_provider")
                    .and_then(|provider| provider.get("max_completion_tokens"))
                    .and_then(number)
            }),
            capabilities,
            declared_endpoint_types: endpoint_types,
            reasoning_options: reasoning,
            modalities,
            input_cost_per_mtok: input_cost,
            output_cost_per_mtok: output_cost,
            price_declared: input_cost.is_some() && output_cost.is_some(),
            ..bare_model(id)
        },
        declared_kind,
    })
}

fn bare_model(id: String) -> SniffedModel {
    SniffedModel {
        id,
        display_name: None,
        owned_by: None,
        kind: ModelKind::Chat,
        context_window: None,
        max_output: None,
        capabilities: ModelCapabilities::default(),
        declared_endpoint_types: Vec::new(),
        reasoning_options: Vec::new(),
        modalities: Vec::new(),
        input_cost_per_mtok: None,
        output_cost_per_mtok: None,
        price_declared: false,
        default_enabled: false,
    }
}

/// The entries of a list body: a bare array, or the array under the first of [`LIST_KEYS`].
fn list_entries(body: &Value) -> Option<&Vec<Value>> {
    body.as_array().or_else(|| {
        LIST_KEYS
            .iter()
            .find_map(|key| body.get(key).and_then(Value::as_array))
    })
}

/// Parses one list body. A body that is no list gives an empty page.
pub(super) fn parse_page(body: &Value) -> ParsedPage {
    let Some(entries) = list_entries(body) else {
        return ParsedPage::default();
    };
    let next = if body.get("has_more").and_then(Value::as_bool) == Some(true) {
        text(body, "last_id").map(ListCursor::AfterId)
    } else {
        text(body, "nextPageToken").map(ListCursor::Token)
    };
    ParsedPage {
        models: entries.iter().filter_map(parse_entry).collect(),
        next,
    }
}

/// Parses a body that was cut at the size cap up to its last complete list entry.
///
/// The scan works on bytes: every structural character is ASCII and no byte of a multi-byte
/// UTF-8 character can equal one, so an escape that swallows the first byte of such a character
/// leaves the rest inert, as a char-wise scan would.
pub(super) fn parse_truncated(bytes: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(bytes)
        .or_else(|error| std::str::from_utf8(&bytes[..error.valid_up_to()]))
        .ok()?;
    let mut stack: Vec<u8> = Vec::new();
    let (mut in_string, mut escaped) = (false, false);
    let mut best: Option<(usize, Vec<u8>)> = None;
    for (index, &byte) in text.as_bytes().iter().enumerate() {
        if in_string {
            match (escaped, byte) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => stack.push(byte),
            b'}' | b']' => {
                stack.pop();
                if stack.last() == Some(&b'[') {
                    best = Some((index + 1, stack.clone()));
                }
            }
            _ => {}
        }
    }
    let (end, open) = best?;
    let mut repaired = text[..end].to_owned();
    for opener in open.iter().rev() {
        repaired.push(if *opener == b'[' { ']' } else { '}' });
    }
    serde_json::from_str(&repaired).ok()
}

/// Applies the catalogue and preset prefill, the kind rules and the default enabled flag to the
/// parsed rows; duplicates by id keep the first.
pub(super) fn finalize(
    raw: Vec<RawModel>,
    catalog: &(dyn Fn(&str) -> Option<CatalogEntry> + Send + Sync),
    preset: Option<&ProviderPreset>,
) -> Vec<SniffedModel> {
    let mut seen = HashSet::new();
    let mut models = Vec::with_capacity(raw.len());
    for RawModel {
        mut model,
        declared_kind,
    } in raw
    {
        if !seen.insert(model.id.clone()) {
            continue;
        }
        let entry = catalog(&model.id);
        if let Some(entry) = &entry {
            model.capabilities.apply_catalog(entry);
            model.context_window = model.context_window.or(entry.max_input_tokens);
            model.max_output = model.max_output.or(entry.max_output_tokens);
            model.input_cost_per_mtok = model.input_cost_per_mtok.or(entry.input_cost_per_mtok);
            model.output_cost_per_mtok = model.output_cost_per_mtok.or(entry.output_cost_per_mtok);
        }
        if let Some(hint) = preset.and_then(|p| p.models.iter().find(|m| m.id == model.id)) {
            model.context_window = model.context_window.or(hint.context);
            model.max_output = model.max_output.or(hint.max_output);
            let caps = &mut model.capabilities;
            declare(
                caps,
                Capability::Vision,
                hint.vision,
                CapabilitySource::Catalog,
            );
            declare(
                caps,
                Capability::Tools,
                hint.tools,
                CapabilitySource::Catalog,
            );
        }
        model.kind = entry
            .as_ref()
            .and_then(|entry| ModelKind::from_catalog_mode(&entry.mode))
            .or(declared_kind)
            .unwrap_or_else(|| kind_from_id(&model.id));
        model.default_enabled = default_enabled(model.kind);
        models.push(model);
    }
    models
}

/// One page of a model list, ready to show or save.
#[derive(Debug, Clone, PartialEq)]
pub struct ListPage {
    /// Models in list order, deduplicated by id, with catalogue prefill, kind and
    /// `default_enabled`.
    pub models: Vec<SniffedModel>,
    /// The next page, when the body names one.
    pub next: Option<ListCursor>,
}

/// Parses one model-list body with the sniffer's tolerant rules. `None` when the body is no
/// model list at all; a body cut at the size cap (`truncated`) is parsed up to its last complete
/// entry. Nothing is sanitised here: the caller owns what it shows.
#[must_use]
pub fn parse_list_page(
    body: &[u8],
    truncated: bool,
    catalog: &(dyn Fn(&str) -> Option<CatalogEntry> + Send + Sync),
) -> Option<ListPage> {
    let value = serde_json::from_slice(body)
        .ok()
        .or_else(|| truncated.then(|| parse_truncated(body)).flatten())?;
    list_entries(&value)?;
    let page = parse_page(&value);
    Some(ListPage {
        models: finalize(page.models, catalog, None),
        next: page.next,
    })
}

#[cfg(test)]
mod tests;
