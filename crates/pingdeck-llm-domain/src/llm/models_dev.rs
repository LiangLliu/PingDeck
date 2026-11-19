//! models.dev `api.json`: parse it into [`ModelRecord`]s, ship the bundled snapshot, download a
//! live copy.
//!
//! Single responsibility: turn the models.dev JSON (`{ "<provider id>": { name, models: { "<model
//! id>": { … } } } }`, prices in USD per million tokens) into catalog rows. The bundled snapshot
//! is the same shape trimmed to the fields read here (`cargo xtask models-snapshot` makes it),
//! plus one extra top-level entry `_snapshot: { generated_at: "YYYY-MM-DD" }` that is not a
//! provider and dates it.
//!
//! What is imported: models of type chat (or no type) and embedding; every other type (image,
//! video, speech, transcription, reranking, realtime) is skipped, deprecated models stay. Only
//! the base `cost` is read: `tiers` and `context_over_200k` surcharges are ignored, so a call
//! above 200k tokens is priced at the base rate (the one known limitation; upgrade by adding a
//! tier column when a long-context price matters). A model or a field of the wrong shape is
//! skipped on its own, the rest still imports; only a body that is not a JSON object fails.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;
use serde::de::IgnoredAny;

use pingdeck_base::cancel::{CancelToken, until_cancelled};
use pingdeck_base::error::AppError;
use pingdeck_model::model_catalog::{
    CatalogKind, CatalogSource, CatalogSyncErrorKind, DEFAULT_CURRENCY, MAX_PRICE_PER_MTOK,
    MAX_TOKEN_LIMIT, ModelRecord,
};

use crate::llm::usage::local_midnight_ms;

/// Longest download accepted.
const MAX_BODY_BYTES: u64 = 16 * 1024 * 1024;
/// Whole-request budget of [`download`].
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// A download that parses to fewer models than this is a truncated or odd body, not a catalog.
const MIN_REMOTE_MODELS: usize = 50;
/// The bundled trimmed snapshot (models.dev shape).
const SNAPSHOT_JSON: &str = include_str!("../../../../fixtures/models-dev-snapshot.json");
/// The top-level key that dates the bundled snapshot.
const SNAPSHOT_KEY: &str = "_snapshot";

// The English sync failures start with these prefixes. They are frozen: a failure is stored in
// `model_catalog_sync.last_error` and `error_kind` reads its kind back from the stored text.
const FAILED_PREFIX: &str = "Model catalog download failed: ";
const HTTP_PREFIX: &str = "Model catalog download failed: HTTP ";
const TIMED_OUT: &str = "Model catalog download timed out";
const TOO_LARGE: &str = "Model catalog is larger than 16 MiB";
const _: () = assert!(MAX_BODY_BYTES == 16 * 1024 * 1024, "TOO_LARGE names 16 MiB");
const NOT_UTF8: &str = "Model catalog download is not valid UTF-8";
const INVALID_PREFIX: &str = "Invalid models.dev catalog: ";
const TOO_FEW_PREFIX: &str = "The models.dev catalog has only ";

/// The kind of a stored sync failure (and the HTTP status for [`CatalogSyncErrorKind::Http`]), so
/// the page can say it in the user's language; messages of other origins are `Other`.
pub fn error_kind(message: &str) -> (CatalogSyncErrorKind, Option<u16>) {
    if let Some(rest) = message.strip_prefix(HTTP_PREFIX) {
        let code = rest
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|digits| digits.parse().ok());
        return (CatalogSyncErrorKind::Http, code);
    }
    let kind = if message.starts_with(TIMED_OUT) {
        CatalogSyncErrorKind::Timeout
    } else if message.starts_with(TOO_LARGE) {
        CatalogSyncErrorKind::TooLarge
    } else if message.starts_with(NOT_UTF8) || message.starts_with(INVALID_PREFIX) {
        CatalogSyncErrorKind::Invalid
    } else if message.starts_with(TOO_FEW_PREFIX) {
        CatalogSyncErrorKind::TooFew
    } else if message.starts_with(FAILED_PREFIX) {
        CatalogSyncErrorKind::Network
    } else {
        CatalogSyncErrorKind::Other
    };
    (kind, None)
}

fn transport_error(error: reqwest::Error) -> AppError {
    if error.is_timeout() {
        AppError::network(TIMED_OUT)
    } else {
        AppError::network(format!("{FAILED_PREFIX}{}", error.without_url()))
    }
}

/// What a models.dev body holds.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCatalog {
    /// The importable models, ordered by family then model id.
    pub records: Vec<ModelRecord>,
    /// The bundled snapshot's date (Unix ms of UTC midnight); `None` for a live download.
    pub snapshot_at_ms: Option<i64>,
}

/// A value of the wrong shape becomes `Skipped` instead of failing its parent.
#[derive(Deserialize)]
#[serde(untagged)]
enum Lenient<T> {
    Value(T),
    Skipped(IgnoredAny),
}

impl<T> Lenient<T> {
    fn value(&self) -> Option<&T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Skipped(_) => None,
        }
    }
}

#[derive(Deserialize, Default)]
struct Provider {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    generated_at: Option<String>,
    #[serde(default)]
    models: BTreeMap<String, Lenient<Model>>,
}

#[derive(Deserialize, Default)]
struct Model {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    release_date: Option<Lenient<String>>,
    #[serde(default)]
    reasoning: Option<bool>,
    #[serde(default)]
    tool_call: Option<bool>,
    #[serde(default)]
    modalities: Option<Lenient<Modalities>>,
    #[serde(default)]
    limit: Option<Lenient<Limit>>,
    #[serde(default)]
    cost: Option<Lenient<Cost>>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    canonical_model_id: Option<String>,
}

#[derive(Deserialize, Default)]
struct Modalities {
    #[serde(default)]
    input: Vec<String>,
}

/// The input modalities kept from `modalities.input`, in their canonical order.
const INPUT_MODALITIES: [&str; 5] = ["text", "image", "audio", "video", "pdf"];

#[derive(Deserialize, Default)]
struct Limit {
    #[serde(default)]
    context: Option<Lenient<f64>>,
    #[serde(default)]
    output: Option<Lenient<f64>>,
}

#[derive(Deserialize, Default)]
struct Cost {
    #[serde(default)]
    input: Option<Lenient<f64>>,
    #[serde(default)]
    output: Option<Lenient<f64>>,
    #[serde(default)]
    cache_read: Option<Lenient<f64>>,
    #[serde(default)]
    cache_write: Option<Lenient<f64>>,
    #[serde(default)]
    reasoning: Option<Lenient<f64>>,
}

fn number(value: Option<&Lenient<f64>>) -> Option<f64> {
    value?.value().copied().filter(|number| number.is_finite())
}

/// A price within `0..=`[`MAX_PRICE_PER_MTOK`]; anything else is dropped (no price).
fn price(value: Option<&Lenient<f64>>) -> Option<f64> {
    number(value).filter(|price| (0.0..=MAX_PRICE_PER_MTOK).contains(price))
}

/// A token limit within `1..=`[`MAX_TOKEN_LIMIT`]; 0 and anything else count as unset.
fn limit(value: Option<&Lenient<f64>>) -> Option<u64> {
    // Exact: both bounds are whole numbers far below 2^53.
    number(value)
        .filter(|tokens| (1.0..=MAX_TOKEN_LIMIT as f64).contains(tokens))
        .map(|tokens| tokens as u64)
}

/// models.dev's `YYYY-MM` or `YYYY-MM-DD` (month 01..=12, day 01..=31); anything else is dropped.
fn release_date(value: Option<&Lenient<String>>) -> Option<String> {
    let date = value?.value()?.trim();
    let number = |part: &str, width: usize, max: u32| {
        part.len() == width
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && part.parse::<u32>().is_ok_and(|n| (1..=max).contains(&n))
    };
    let mut parts = date.split('-');
    let year_month = matches!(
        (parts.next(), parts.next()),
        (Some(year), Some(month)) if number(year, 4, 9999) && number(month, 2, 12)
    );
    let day = match parts.next() {
        None => true,
        Some(day) => number(day, 2, 31) && parts.next().is_none(),
    };
    (year_month && day).then(|| date.to_owned())
}

/// The catalog kind of a models.dev `type`: none or `chat` is chat, `embedding` is embedding,
/// every other type is not imported (`None`).
fn catalog_kind(kind: Option<&str>) -> Option<CatalogKind> {
    match kind {
        None | Some("chat") => Some(CatalogKind::Chat),
        Some("embedding") => Some(CatalogKind::Embedding),
        Some(_) => None,
    }
}

/// The known input modalities of `listed` in canonical order; unknown words are dropped.
fn input_modalities(listed: Option<&Modalities>) -> Vec<String> {
    INPUT_MODALITIES
        .iter()
        .filter(|known| listed.is_some_and(|m| m.input.iter().any(|input| input == *known)))
        .map(|known| (*known).to_owned())
        .collect()
}

/// `value` trimmed when it is not blank, else `fallback`.
fn non_blank_or<'a>(value: Option<&'a str>, fallback: &'a str) -> &'a str {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback)
}

fn record(
    family: &str,
    family_name: &str,
    id: &str,
    model: &Model,
    synced_at_ms: i64,
) -> Option<ModelRecord> {
    let kind = catalog_kind(model.kind.as_deref())?;
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    let cost = model.cost.as_ref().and_then(Lenient::value);
    let cost_of =
        |pick: fn(&Cost) -> Option<&Lenient<f64>>| cost.and_then(|cost| price(pick(cost)));
    let limits = model.limit.as_ref().and_then(Lenient::value);
    let cache_read = cost_of(|cost| cost.cache_read.as_ref());
    let cache_write = cost_of(|cost| cost.cache_write.as_ref());
    let inputs = input_modalities(model.modalities.as_ref().and_then(Lenient::value));
    Some(ModelRecord {
        family: family.to_owned(),
        family_name: family_name.to_owned(),
        model_id: id.to_owned(),
        name: non_blank_or(model.name.as_deref(), id).to_owned(),
        release_date: release_date(model.release_date.as_ref()),
        kind,
        context_tokens: limits.and_then(|limits| limit(limits.context.as_ref())),
        max_output_tokens: limits.and_then(|limits| limit(limits.output.as_ref())),
        reasoning: model.reasoning.unwrap_or(false),
        vision: inputs.iter().any(|input| input == "image"),
        input_modalities: inputs,
        tools: model.tool_call.unwrap_or(false),
        prompt_caching: cache_read.is_some() || cache_write.is_some(),
        canonical_id: model.canonical_model_id.clone().unwrap_or_default(),
        input_per_mtok: cost_of(|cost| cost.input.as_ref()),
        output_per_mtok: cost_of(|cost| cost.output.as_ref()),
        cache_read_per_mtok: cache_read,
        cache_write_per_mtok: cache_write,
        reasoning_per_mtok: cost_of(|cost| cost.reasoning.as_ref()),
        currency: DEFAULT_CURRENCY.to_owned(),
        source: CatalogSource::ModelsDev,
        synced_at_ms,
    })
}

/// Parses a models.dev body into rows stamped `synced_at_ms`, ordered by family then model id.
///
/// # Errors
/// [`AppError::validation`] when `json` is not a JSON object.
pub fn parse(json: &str, synced_at_ms: i64) -> Result<ParsedCatalog, AppError> {
    let providers: BTreeMap<String, Lenient<Provider>> = serde_json::from_str(json)
        .map_err(|error| AppError::validation(format!("{INVALID_PREFIX}{error}")))?;
    let mut records = Vec::new();
    let mut snapshot_at_ms = None;
    for (family, provider) in &providers {
        let Some(provider) = provider.value() else {
            continue;
        };
        if family == SNAPSHOT_KEY {
            snapshot_at_ms = provider
                .generated_at
                .as_deref()
                .and_then(|day| local_midnight_ms(day, 0));
            continue;
        }
        let family_name = non_blank_or(provider.name.as_deref(), family);
        for (id, model) in &provider.models {
            if let Some(record) = model
                .value()
                .and_then(|model| record(family, family_name, id, model, synced_at_ms))
            {
                records.push(record);
            }
        }
    }
    Ok(ParsedCatalog {
        records,
        snapshot_at_ms,
    })
}

/// [`parse`] of a live download, rejected when it holds fewer than the minimum model count.
///
/// # Errors
/// [`AppError::validation`] when the body is not a JSON object or holds too few models.
pub fn parse_remote(json: &str, synced_at_ms: i64) -> Result<ParsedCatalog, AppError> {
    let parsed = parse(json, synced_at_ms)?;
    if parsed.records.len() < MIN_REMOTE_MODELS {
        return Err(AppError::validation(format!(
            "{TOO_FEW_PREFIX}{} models; expected at least {MIN_REMOTE_MODELS}",
            parsed.records.len()
        )));
    }
    Ok(parsed)
}

/// The rows shipped inside the binary, stamped with the snapshot's date.
///
/// # Errors
/// [`AppError::internal`] when the bundled fixture is corrupt or undated.
pub fn bundled_snapshot() -> Result<ParsedCatalog, AppError> {
    let corrupt =
        |why: String| AppError::internal(format!("Bundled model catalog is corrupt: {why}"));
    let mut parsed =
        parse(SNAPSHOT_JSON, 0).map_err(|error| corrupt(error.message().to_owned()))?;
    let at = parsed
        .snapshot_at_ms
        .ok_or_else(|| corrupt(format!("no {SNAPSHOT_KEY}.generated_at")))?;
    for record in &mut parsed.records {
        record.synced_at_ms = at;
    }
    Ok(parsed)
}

/// The body of `url` fetched with `client`: 30 s for the whole request, at most 16 MiB.
///
/// The caller's client must not follow redirects (the shared upstream client of `pingdeck-llm`
/// uses `Policy::none()`); a 3xx answer fails and its message says redirects are not followed.
///
/// # Errors
/// [`AppError::cancelled`] when `cancel` fires, [`AppError::network`] on transport, status,
/// size or encoding problems.
pub async fn download(
    client: &reqwest::Client,
    url: &str,
    cancel: Option<&CancelToken>,
) -> Result<String, AppError> {
    until_cancelled(cancel, fetch_body(client, url))
        .await
        .ok_or_else(AppError::cancelled)?
}

async fn fetch_body(client: &reqwest::Client, url: &str) -> Result<String, AppError> {
    let mut response = client
        .get(url)
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .map_err(transport_error)?;
    let status = response.status();
    if !status.is_success() {
        let note = if status.is_redirection() {
            " (redirects are not followed)"
        } else {
            ""
        };
        return Err(AppError::network(format!("{HTTP_PREFIX}{status}{note}")));
    }
    let too_large = || AppError::network(TOO_LARGE);
    let announced = response.content_length();
    if announced.is_some_and(|len| len > MAX_BODY_BYTES) {
        return Err(too_large());
    }
    // At most MAX_BODY_BYTES (checked above), so the reservation is bounded.
    let mut body =
        Vec::with_capacity(announced.map_or(0, |len| usize::try_from(len).unwrap_or_default()));
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if u64::try_from(body.len() + chunk.len()).map_or(true, |len| len > MAX_BODY_BYTES) {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| AppError::network(NOT_UTF8))
}

#[cfg(test)]
mod tests;
