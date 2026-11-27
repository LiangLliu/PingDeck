//! The SpanBay mock-server export file and identity re-issuing.
//!
//! Single responsibility: serialise mock servers to the portable
//! `{ "format": "spanbay-mock-servers", "version": 1, … }` document, parse and validate such a
//! document, leave literal credentials out of an export, and give a server (or a copy of one)
//! fresh ids with every internal reference rewritten. Storage, port choice and naming are the
//! use-cases' business.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use spanbay_base::secrets::is_secret_header_name;
use spanbay_model::credentials::{UrlPart, is_literal, url_credentials};

use crate::workspace::items::{MockBodySource, MockServer};
use crate::workspace::models::KeyValue;

/// `format` value of an export file.
pub const EXPORT_FORMAT: &str = "spanbay-mock-servers";

/// The export version this build writes and the newest it reads.
pub const EXPORT_VERSION: u32 = 1;

/// A mock-server export file.
#[derive(Debug, Serialize, Deserialize)]
struct ExportFile {
    format: String,
    version: u32,
    exported_at_ms: i64,
    servers: Vec<MockServer>,
}

/// Why an export file was refused.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TransferError {
    /// Not JSON, not a SpanBay mock export, or a malformed server inside it.
    #[error("not a SpanBay mock server export: {0}")]
    Invalid(String),
    /// Written by a newer SpanBay.
    #[error(
        "this export has version {found}; this SpanBay reads versions up to {EXPORT_VERSION} (update SpanBay)"
    )]
    Newer {
        /// The version the file declares.
        found: u64,
    },
}

/// Serialises `servers` as an export file stamped `exported_at_ms`.
///
/// # Errors
/// The servers cannot be serialised (not expected for stored servers).
pub fn export_json(
    servers: Vec<MockServer>,
    exported_at_ms: i64,
) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&ExportFile {
        format: EXPORT_FORMAT.to_owned(),
        version: EXPORT_VERSION,
        exported_at_ms,
        servers,
    })
}

/// Leaves empty the literal credentials of `servers` at the credential positions collection
/// export uses (`spanbay-model::credentials`): credential-named header values (`Authorization`,
/// `X-Api-Key`, `Cookie`, ...) in the proxy request headers and callback headers, and the URL
/// userinfo (with its `@`) and credential-named query values of the proxy target and callback
/// URLs. `{{variable}}` references and blank values stay. Returns how many values were left
/// empty (D-272 applies to mock exports the same way).
pub fn strip_literal_credentials(servers: &mut [MockServer]) -> usize {
    fn strip(headers: &mut [KeyValue]) -> usize {
        let mut stripped = 0;
        for header in headers {
            if is_literal(&header.value) && is_secret_header_name(&header.key) {
                header.value.clear();
                stripped += 1;
            }
        }
        stripped
    }
    servers
        .iter_mut()
        .map(|server| {
            strip(&mut server.proxy_req_headers)
                + strip_url(&mut server.proxy_base_url)
                + server
                    .callbacks
                    .iter_mut()
                    .map(|callback| strip(&mut callback.headers) + strip_url(&mut callback.url))
                    .sum::<usize>()
        })
        .sum()
}

/// Leaves empty the literal userinfo (with its `@`) and credential-named query values of `url`;
/// returns how many were left empty.
fn strip_url(url: &mut String) -> usize {
    let mut stripped = 0;
    for found in url_credentials(url).iter().rev() {
        let Some(text) = url.get(found.range.clone()) else {
            continue;
        };
        if !is_literal(text) {
            continue;
        }
        match found.part {
            UrlPart::Userinfo => url.replace_range(found.range.start..=found.range.end, ""),
            UrlPart::Query(_) => url.replace_range(found.range.clone(), ""),
        }
        stripped += 1;
    }
    stripped
}

/// The local file paths the responses of `server` serve as bodies, in route order without
/// duplicates. Imported content is not authorization: importers name these paths in a warning so
/// the user sees which local files the mock exposes once started.
pub fn file_body_paths(server: &MockServer) -> Vec<&str> {
    let mut paths: Vec<&str> = Vec::new();
    for response in server.routes.iter().flat_map(|route| &route.responses) {
        let path = response.body.trim();
        if response.body_source == Some(MockBodySource::File) && !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

/// Parses an export file, checking `format` and `version` before reading the servers, so a file
/// of a newer version is reported as such and not as a malformed one.
///
/// # Errors
/// [`TransferError::Invalid`] for text that is not a mock export, [`TransferError::Newer`] for a
/// version above [`EXPORT_VERSION`].
pub fn parse_export(json_text: &str) -> Result<Vec<MockServer>, TransferError> {
    let invalid = |message: &str| TransferError::Invalid(message.to_owned());
    let mut document: serde_json::Value = serde_json::from_str(json_text)
        .map_err(|error| TransferError::Invalid(error.to_string()))?;
    if document.get("format").and_then(serde_json::Value::as_str) != Some(EXPORT_FORMAT) {
        return Err(invalid("the `format` is not \"spanbay-mock-servers\""));
    }
    let version = document
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .filter(|version| *version >= 1)
        .ok_or_else(|| invalid("the `version` is missing"))?;
    if version > u64::from(EXPORT_VERSION) {
        return Err(TransferError::Newer { found: version });
    }
    let servers = document
        .get_mut("servers")
        .map(serde_json::Value::take)
        .ok_or_else(|| invalid("the `servers` list is missing"))?;
    serde_json::from_value(servers).map_err(|error| TransferError::Invalid(error.to_string()))
}

/// Gives `server`, its routes, responses, rules, callbacks and data buckets new ids from
/// `new_id(prefix)` and rewrites the references between them (`data_bucket_id` of CRUD routes,
/// `callback_ids` of responses). A reference to an id that does not exist stays untouched.
pub fn reissue_ids(server: &mut MockServer, mut new_id: impl FnMut(&str) -> String) {
    server.id = new_id("mock");
    let buckets: HashMap<String, String> = server
        .data_buckets
        .iter_mut()
        .map(|bucket| {
            (
                std::mem::replace(&mut bucket.id, new_id("bucket")),
                bucket.id.clone(),
            )
        })
        .collect();
    let callbacks: HashMap<String, String> = server
        .callbacks
        .iter_mut()
        .map(|callback| {
            (
                std::mem::replace(&mut callback.id, new_id("callback")),
                callback.id.clone(),
            )
        })
        .collect();
    for route in &mut server.routes {
        route.id = new_id("route");
        if let Some(bucket) = buckets.get(&route.data_bucket_id) {
            route.data_bucket_id.clone_from(bucket);
        }
        for response in &mut route.responses {
            response.id = new_id("resp");
            for callback_id in &mut response.callback_ids {
                if let Some(callback) = callbacks.get(callback_id.as_str()) {
                    callback_id.clone_from(callback);
                }
            }
            for rule in &mut response.rules {
                rule.id = new_id("rule");
            }
        }
    }
}
