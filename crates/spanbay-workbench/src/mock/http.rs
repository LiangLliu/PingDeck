//! Translates between hyper's request/response types and the mock server's plain structs.
//!
//! Single responsibility: HTTP framing is hyper's job; this file only applies the mock's limits
//! (1 MiB bodies), flattens a request into a [`ParsedRequest`] (the body kept byte for byte, plus
//! a lossy text view for templates, rules and logs) and turns a reply into a response with
//! sanitized headers.

use std::time::Duration;

use bytes::Bytes;
use http::{HeaderName, HeaderValue, StatusCode, header};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{Request, Response, body::Incoming};
use thiserror::Error;

/// Largest request header block hyper may buffer.
pub(super) const MAX_HEADER_BYTES: usize = 64 * 1024;
/// Largest request body the server will buffer.
pub(super) const MAX_BODY_BYTES: usize = 1024 * 1024;

/// A request read off the socket, with its target split into path and query.
#[derive(Debug, Default)]
pub(super) struct ParsedRequest {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) query: String,
    pub(super) headers: Vec<(String, String)>,
    /// The body decoded as UTF-8 (invalid sequences replaced), for templates, rules and logs.
    pub(super) body: String,
    /// The body exactly as received; what the proxy forwards.
    pub(super) raw_body: Bytes,
}

impl ParsedRequest {
    /// Method, path, query and headers of a request whose body is not read (it is refused).
    pub(super) fn head_of<B>(request: &Request<B>) -> Self {
        Self {
            method: request.method().as_str().to_ascii_uppercase(),
            path: request.uri().path().to_owned(),
            query: request.uri().query().unwrap_or_default().to_owned(),
            headers: request
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_owned(),
                        String::from_utf8_lossy(value.as_bytes()).into_owned(),
                    )
                })
                .collect(),
            body: String::new(),
            raw_body: Bytes::new(),
        }
    }
}

/// Why a request body could not be read; maps to an HTTP status.
#[derive(Debug, Error)]
pub(super) enum RequestError {
    #[error("bad request: {0}")]
    BadRequest(&'static str),
    #[error("request body exceeds {MAX_BODY_BYTES} bytes")]
    BodyTooLarge,
}

impl RequestError {
    /// HTTP status sent back for this error.
    pub(super) fn status(&self) -> u16 {
        match self {
            Self::BadRequest(_) => 400,
            Self::BodyTooLarge => 413,
        }
    }
}

/// Flattens `request` (headers lower-cased, body buffered up to [`MAX_BODY_BYTES`] within
/// `body_timeout`) into a [`ParsedRequest`]. Chunked and length-delimited bodies are treated alike.
pub(super) async fn read_request(
    request: Request<Incoming>,
    body_timeout: Duration,
) -> Result<ParsedRequest, RequestError> {
    let (parts, body) = request.into_parts();

    // Reject an oversized declaration before reading a byte of it.
    let declared = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if declared.is_some_and(|length| length > MAX_BODY_BYTES) {
        return Err(RequestError::BodyTooLarge);
    }

    let collected =
        tokio::time::timeout(body_timeout, Limited::new(body, MAX_BODY_BYTES).collect())
            .await
            .map_err(|_| RequestError::BadRequest("timed out reading the request body"))?
            .map_err(|error| {
                if error.is::<http_body_util::LengthLimitError>() {
                    RequestError::BodyTooLarge
                } else {
                    RequestError::BadRequest("could not read the request body")
                }
            })?;

    let path = parts.uri.path();
    let raw_body = collected.to_bytes();
    Ok(ParsedRequest {
        method: parts.method.as_str().to_ascii_uppercase(),
        path: if path.is_empty() { "/" } else { path }.to_owned(),
        query: parts.uri.query().unwrap_or_default().to_owned(),
        headers: parts
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect(),
        body: String::from_utf8_lossy(&raw_body).into_owned(),
        raw_body,
    })
}

/// Builds a response with sanitized headers. hyper adds `Date` and `Content-Length`, so
/// configured framing headers are dropped; an unusable status becomes `500`.
pub(super) fn build_response(
    status: u16,
    headers: &[(String, String)],
    body: impl Into<Bytes>,
    close: bool,
) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(body.into()));
    *response.status_mut() =
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let map = response.headers_mut();
    for (key, value) in headers {
        if let Some((name, value)) = sanitized_header(key, value) {
            map.append(name, value);
        }
    }
    if close {
        map.insert(header::CONNECTION, HeaderValue::from_static("close"));
    }
    response
}

/// A configured header made safe to put on the wire, or `None` when nothing usable is left.
///
/// The name is reduced to RFC 9110 token characters, so a configured name containing CR/LF (or
/// `:`) can never inject an extra header line; CR, LF and NUL are stripped from the value so a
/// rendered template cannot split the header line; framing headers are dropped because the
/// sender writes them itself and a configured copy would make the message ambiguous. Used for
/// mock responses, proxied requests and callbacks alike.
pub(super) fn sanitized_header(name: &str, value: &str) -> Option<(HeaderName, HeaderValue)> {
    let name: String = name
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(*character)
        })
        .collect();
    if is_framing_header(&name) {
        return None;
    }
    Some((
        HeaderName::from_bytes(name.as_bytes()).ok()?,
        HeaderValue::from_bytes(sanitize_header_value(value).as_bytes()).ok()?,
    ))
}

/// Strips CR, LF and NUL so a rendered template cannot split the header line.
fn sanitize_header_value(value: &str) -> String {
    value
        .chars()
        .filter(|character| !matches!(character, '\r' | '\n' | '\0'))
        .collect()
}

/// Headers the sender writes itself; a configured copy would duplicate framing.
fn is_framing_header(name: &str) -> bool {
    [
        "content-length",
        "connection",
        "transfer-encoding",
        "date",
        "keep-alive",
        "upgrade",
        "trailer",
    ]
    .iter()
    .any(|reserved| name.eq_ignore_ascii_case(reserved))
}

/// Headers for a JSON reply.
pub(super) fn json_headers() -> Vec<(String, String)> {
    vec![("Content-Type".into(), "application/json".into())]
}
