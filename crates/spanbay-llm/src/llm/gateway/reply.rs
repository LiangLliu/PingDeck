//! The answers of the request pipeline: errors in the client's dialect, an upstream error passed
//! on, the success headers.
//!
//! Single responsibility: build `hyper` responses for the pipeline. An error the gateway words
//! itself (every attempt failed, nothing to try, a feature it cannot translate) uses the client
//! protocol's error shape; an upstream error that is the client's own answer (any
//! other 4xx) keeps the upstream's status and body (the caller redacts it first) but never its
//! content type: the gateway says `application/json` or `text/plain` itself, and every response
//! built here carries `X-Content-Type-Options: nosniff`.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::{CONTENT_TYPE, HeaderName, HeaderValue};
use hyper::{Response, StatusCode};

use super::server::GatewayBody;

/// Response header listing the request fields a translation dropped.
pub(super) const DROPPED_HEADER: &str = "x-spanbay-dropped";

/// Response header naming the target that answered (or was tried last): `provider_id/model`,
/// percent-encoded outside visible ASCII.
pub(super) const TARGET_HEADER: &str = "x-spanbay-target";

/// Response header counting the upstream attempts of the request.
pub(super) const ATTEMPTS_HEADER: &str = "x-spanbay-attempts";

/// `X-Content-Type-Options`: no browser guesses a type for what the gateway sends.
const NOSNIFF: HeaderName = HeaderName::from_static("x-content-type-options");

/// The error `type` string for an HTTP status.
#[must_use]
pub(super) fn kind_of(status: u16) -> &'static str {
    match status {
        400 | 413 | 422 => "invalid_request_error",
        401 => "authentication_error",
        402 | 403 => "permission_error",
        404 => "not_found_error",
        408 | 504 => "timeout_error",
        429 => "rate_limit_error",
        503 | 529 => "overloaded_error",
        _ => "api_error",
    }
}

/// A response with `body` as it is.
#[must_use]
pub(super) fn bytes_response(
    status: u16,
    content_type: &str,
    body: Bytes,
) -> Response<GatewayBody> {
    let mut response = Response::new(Full::new(body).boxed_unsync());
    *response.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    if let Ok(value) = HeaderValue::from_str(content_type) {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    response
        .headers_mut()
        .insert(NOSNIFF, HeaderValue::from_static("nosniff"));
    response
}

/// The content type of a relayed upstream error body: JSON when it parses, else plain text.
#[must_use]
pub(super) fn upstream_content_type(body: &str) -> &'static str {
    if serde_json::from_str::<serde_json::Value>(body).is_ok() {
        "application/json"
    } else {
        "text/plain; charset=utf-8"
    }
}

/// An upstream error answer: its status and `body` (already redacted), under the gateway's own
/// content type.
#[must_use]
pub(super) fn upstream_error(status: u16, body: &str) -> Response<GatewayBody> {
    bytes_response(
        status,
        upstream_content_type(body),
        Bytes::from(body.to_owned()),
    )
}

/// `response` with [`TARGET_HEADER`] and [`ATTEMPTS_HEADER`] for `attempted` (`None`: no
/// upstream was tried). The target is percent-encoded, so the header is never dropped.
pub(super) fn with_target(response: &mut Response<GatewayBody>, attempted: Option<(String, u32)>) {
    let Some((target, attempts)) = attempted else {
        return;
    };
    const KEEP: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS.add(b'%').add(b' ');
    let encoded = percent_encoding::utf8_percent_encode(&target, KEEP).to_string();
    if let Ok(value) = HeaderValue::from_str(&encoded) {
        response.headers_mut().insert(TARGET_HEADER, value);
    }
    response
        .headers_mut()
        .insert(ATTEMPTS_HEADER, HeaderValue::from(attempts));
}

/// `response` with `x-spanbay-dropped: <names>` when something was dropped.
#[must_use]
pub(super) fn with_dropped(
    mut response: Response<GatewayBody>,
    names: Option<&str>,
) -> Response<GatewayBody> {
    if let Some(value) = names.and_then(|names| HeaderValue::from_str(names).ok()) {
        response.headers_mut().insert(DROPPED_HEADER, value);
    }
    response
}
