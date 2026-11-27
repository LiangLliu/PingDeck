//! Gemini `groundingMetadata` and hub citations, both directions.
//!
//! Gemini reports a grounded answer as `groundingChunks` (the sources), `groundingSupports`
//! (text spans and the chunks backing them) and `webSearchQueries`. The hub carries a citation
//! per (span, source) pair and the queries as the input of a web search call.

use serde_json::{Map, Value, json};

use super::field;
use crate::llm::adapters::object;
use crate::llm::hub::HubCitation;

/// The id of the synthetic web search call that carries Gemini's search queries.
pub(super) const GROUNDING_CALL_ID: &str = "grounding_1";

/// The citations and search queries of one candidate.
#[derive(Debug, Default)]
pub(super) struct Grounding {
    pub(super) citations: Vec<HubCitation>,
    pub(super) queries: Vec<String>,
}

impl Grounding {
    /// The input of the web search call: `{"queries": [...]}`; `None` without queries.
    pub(super) fn search_input(&self) -> Option<Value> {
        (!self.queries.is_empty()).then(|| json!({"queries": self.queries}))
    }
}

/// A slice of `value[key]` when it is an array; empty otherwise.
fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    field(value, key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// The grounding of a candidate; empty when it has no `groundingMetadata` object.
pub(super) fn decode(candidate: &Value) -> Grounding {
    let Some(meta) = field(candidate, "groundingMetadata").filter(|m| m.is_object()) else {
        return Grounding::default();
    };
    let queries = array(meta, "webSearchQueries")
        .iter()
        .filter_map(Value::as_str)
        .filter(|q| !q.is_empty())
        .map(str::to_owned)
        .collect();
    let chunks = array(meta, "groundingChunks");
    let mut citations = Vec::new();
    for support in array(meta, "groundingSupports") {
        support_citations(support, chunks, &mut citations);
    }
    Grounding { citations, queries }
}

/// One citation per web chunk a support points at; chunks without a URI are skipped.
///
/// A missing `startIndex` with an `endIndex` present means 0: proto3 JSON leaves out zero
/// values.
fn support_citations(support: &Value, chunks: &[Value], out: &mut Vec<HubCitation>) {
    let segment = support.get("segment");
    let index = |key: &str| {
        segment
            .and_then(|s| field(s, key))
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
    };
    let end_index = index("endIndex");
    let start_index = index("startIndex").or(end_index.map(|_| 0));
    let snippet = segment
        .and_then(|s| s.get("text"))
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    for position in array(support, "groundingChunkIndices")
        .iter()
        .filter_map(Value::as_u64)
    {
        let web = usize::try_from(position)
            .ok()
            .and_then(|pos| chunks.get(pos))
            .and_then(|chunk| chunk.get("web"));
        let Some(uri) = web
            .and_then(|w| w.get("uri"))
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty())
        else {
            continue;
        };
        out.push(HubCitation {
            url: uri.to_owned(),
            title: web
                .and_then(|w| w.get("title"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            snippet: snippet.clone(),
            start_index,
            end_index,
        });
    }
}

/// The queries of a web search call's input: `queries` (an array) or a single `query`.
pub(super) fn search_queries(input: &Value) -> Vec<String> {
    match (input.get("queries"), input.get("query")) {
        (Some(Value::Array(queries)), _) => queries
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        (_, Some(Value::String(query))) => vec![query.clone()],
        _ => Vec::new(),
    }
}

/// A support under construction: the part it cites (`None` for a search result), the span and
/// the indices of the chunks backing it.
type Support<'a> = (Option<usize>, &'a HubCitation, Vec<usize>);

/// The `groundingMetadata` of citations and queries; `None` when both are empty.
///
/// Sources are deduplicated by (url, title) into chunks; citations with the same part and span
/// share one support. A citation without a span only adds its chunk.
pub(super) fn encode<'a>(
    sources: impl IntoIterator<Item = (Option<usize>, &'a HubCitation)>,
    queries: &[String],
) -> Option<Value> {
    let mut chunks: Vec<(&str, &str)> = Vec::new();
    let mut supports: Vec<Support<'_>> = Vec::new();
    for (part, citation) in sources {
        let chunk = (citation.url.as_str(), citation.title.as_str());
        let position = chunks
            .iter()
            .position(|known| *known == chunk)
            .unwrap_or_else(|| {
                chunks.push(chunk);
                chunks.len() - 1
            });
        if citation.start_index.is_none()
            && citation.end_index.is_none()
            && citation.snippet.is_none()
        {
            continue;
        }
        let same_span = |(known_part, known, _): &Support<'_>| {
            *known_part == part
                && known.start_index == citation.start_index
                && known.end_index == citation.end_index
                && known.snippet == citation.snippet
        };
        match supports.iter_mut().find(|support| same_span(support)) {
            Some((_, _, indices)) if !indices.contains(&position) => indices.push(position),
            Some(_) => {}
            None => supports.push((part, citation, vec![position])),
        }
    }
    let mut meta = Map::new();
    if !queries.is_empty() {
        meta.insert("webSearchQueries".into(), json!(queries));
    }
    if !chunks.is_empty() {
        let chunks: Vec<Value> = chunks
            .iter()
            .map(|(uri, title)| json!({"web": {"uri": uri, "title": title}}))
            .collect();
        meta.insert("groundingChunks".into(), Value::Array(chunks));
    }
    if !supports.is_empty() {
        meta.insert("groundingSupports".into(), supports_json(supports));
    }
    (!meta.is_empty()).then_some(Value::Object(meta))
}

/// The `groundingSupports` array: each support's `segment` and its chunk indices.
fn supports_json(supports: Vec<Support<'_>>) -> Value {
    supports
        .into_iter()
        .map(|(part, citation, indices)| {
            let mut segment = Map::new();
            if let Some(part) = part {
                segment.insert("partIndex".into(), part.into());
            }
            if let Some(start) = citation.start_index {
                segment.insert("startIndex".into(), start.into());
            }
            if let Some(end) = citation.end_index {
                segment.insert("endIndex".into(), end.into());
            }
            if let Some(text) = &citation.snippet {
                segment.insert("text".into(), text.as_str().into());
            }
            object([
                ("segment", Value::Object(segment)),
                ("groundingChunkIndices", indices.into()),
            ])
        })
        .collect()
}
