//! Small JSON, SSE and effort helpers shared by every chat codec.
//!
//! Re-exported from the parent module under explicit names, so codecs import them as
//! `crate::llm::adapters::{...}`.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::llm::hub::{HubEffort, HubImage};
use crate::llm::sse::{OVERFLOW_EVENT, SseEvent};
use pingdeck_base::error::AppError;

/// Turns the parser's in-band [`OVERFLOW_EVENT`] into a network error carrying its reason.
fn reject_overflow(event: &SseEvent) -> Result<(), AppError> {
    if event.event.as_deref() == Some(OVERFLOW_EVENT) {
        return Err(AppError::network(format!("invalid stream: {}", event.data)));
    }
    Ok(())
}

/// Parses the JSON `data` of an upstream SSE event.
///
/// Returns `Ok(None)` for blank data and the `[DONE]` sentinel.
///
/// # Errors
///
/// A network error for an [`OVERFLOW_EVENT`] or data that is not JSON; `label` names the stream
/// in the message.
pub(crate) fn parse_sse_json(event: &SseEvent, label: &str) -> Result<Option<Value>, AppError> {
    reject_overflow(event)?;
    let data = event.data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
    }
    serde_json::from_str(data)
        .map(Some)
        .map_err(|error| AppError::network(format!("invalid {label}: {error}")))
}

/// The string at `value[key]`, when it is one.
pub(crate) fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// The string at `value[key]` as an owned value; empty when missing or not a string.
pub(crate) fn str_or_empty(value: &Value, key: &str) -> String {
    str_of(value, key).unwrap_or_default().to_owned()
}

/// Moves the string at `value[key]` out, leaving an empty string behind (no copy).
pub(crate) fn take_str(value: &mut Value, key: &str) -> Option<String> {
    match value.get_mut(key)? {
        Value::String(text) => Some(std::mem::take(text)),
        _ => None,
    }
}

/// `value[key]` unless it is missing or JSON `null`.
pub(crate) fn non_null<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key).filter(|inner| !inner.is_null())
}

/// Narrows a token count to `u32`, saturating at `u32::MAX`.
pub(crate) fn to_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// The length of `text` in UTF-16 code units, the unit of hub citation indices; saturating.
/// The cache hint of a client's OpenAI `prompt_cache_key` (a routing key, no breakpoints).
pub(crate) fn prompt_cache_key(body: &Value) -> Option<crate::llm::hub::CacheHint> {
    let key = body.get("prompt_cache_key")?.as_str()?.trim();
    (!key.is_empty()).then(|| crate::llm::hub::CacheHint {
        breakpoints: Vec::new(),
        key: Some(key.to_owned()),
    })
}

pub(crate) fn utf16_len(text: &str) -> u32 {
    u32::try_from(text.encode_utf16().count()).unwrap_or(u32::MAX)
}

/// A JSON number as `u32`; `None` when it is not an unsigned integer or does not fit.
pub(crate) fn as_u32(value: &Value) -> Option<u32> {
    value.as_u64().and_then(|n| u32::try_from(n).ok())
}

/// A JSON object of `entries`, inserted in order; the values are moved in, not copied the way
/// `json!` copies an interpolated owned value.
pub(crate) fn object<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Object(object_map(entries))
}

/// The map of [`object`], for a caller that adds more fields before wrapping it.
pub(crate) fn object_map<const N: usize>(entries: [(&str, Value); N]) -> Map<String, Value> {
    let mut map = Map::new();
    for (key, value) in entries {
        map.insert(key.to_owned(), value);
    }
    map
}

/// Inserts `body[key] = value` only when `value` is `Some`.
pub(crate) fn insert_some<T: Into<Value>>(
    body: &mut Map<String, Value>,
    key: &str,
    value: Option<T>,
) {
    if let Some(value) = value {
        body.insert(key.into(), value.into());
    }
}

/// Seconds since the Unix epoch; 0 when the clock is before it.
pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Tool-call arguments as text: a string is used as is, `null` or a missing value is empty and
/// any other JSON value is re-serialized.
pub(crate) fn arguments_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Tool-call arguments as the JSON object protocols require.
///
/// Blank text becomes `{}`; text that is not a JSON object is wrapped as `{"_raw": text}`, so the
/// call still reaches the model. Every codec relies on this wire shape.
pub(crate) fn parse_args_object(arguments: &str) -> Value {
    if arguments.trim().is_empty() {
        return json!({});
    }
    match serde_json::from_str::<Value>(arguments) {
        Ok(value @ Value::Object(_)) => value,
        _ => json!({"_raw": arguments}),
    }
}

/// Splits `data:<media type>;base64,<data>` into its media type and payload.
pub(crate) fn parse_base64_data_url(url: &str) -> Option<(&str, &str)> {
    let (header, data) = url.strip_prefix("data:")?.split_once(',')?;
    Some((header.strip_suffix(";base64")?, data))
}

/// An image as a URL: remote URLs as is, inline bytes as a base64 `data:` URL. `None` for an
/// unresolved attachment.
pub(crate) fn image_data_url(image: &HubImage) -> Option<String> {
    match image {
        HubImage::Url { url } => Some(url.clone()),
        HubImage::Base64 { media_type, data } => Some(format!("data:{media_type};base64,{data}")),
        HubImage::AttachmentRef { .. } => None,
    }
}

/// The wire word of an effort level.
pub(crate) fn effort_str(effort: HubEffort) -> &'static str {
    match effort {
        HubEffort::Minimal => "minimal",
        HubEffort::Low => "low",
        HubEffort::Medium => "medium",
        HubEffort::High => "high",
        HubEffort::XHigh => "xhigh",
        HubEffort::Max => "max",
    }
}

/// The effort level a wire word names; the inverse of [`effort_str`].
pub(crate) fn parse_effort(effort: &str) -> Option<HubEffort> {
    match effort {
        "minimal" => Some(HubEffort::Minimal),
        "low" => Some(HubEffort::Low),
        "medium" => Some(HubEffort::Medium),
        "high" => Some(HubEffort::High),
        "xhigh" => Some(HubEffort::XHigh),
        "max" => Some(HubEffort::Max),
        _ => None,
    }
}
