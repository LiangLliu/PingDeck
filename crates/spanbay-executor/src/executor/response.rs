//! Reading a response body under the size cap, the idle and total time limits and the cancel
//! token, optionally streaming it whole to a file, and classifying image responses.

use crate::executor::types::{BodyIncomplete, ExecutionError, ExecutorLimits};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderMap};
use spanbay_base::cancel::{CancelToken, until_cancelled};
use std::borrow::Cow;
use std::path::Path;
use tokio::io::AsyncWriteExt as _;

/// Default largest response body the executor reads, in bytes; the
/// `data.response_body_cap_bytes` setting overrides it. Request bodies have their own limit
/// (`MAX_REQUEST_BODY_BYTES`).
pub const MAX_RESPONSE_BODY_BYTES: usize = 10 * 1024 * 1024;

/// Image previews are limited to avoid oversized data URLs.
const MAX_IMAGE_PREVIEW_BYTES: usize = 4 * 1024 * 1024;

/// A response body read under the size cap.
pub(super) struct ResponseBody {
    /// The bytes decoded with the charset the Content-Type declares (lossy UTF-8 when it
    /// declares none, or UTF-8).
    pub(super) text: String,
    pub(super) size_bytes: usize,
    /// Whether the cap cut the captured body.
    pub(super) truncated: bool,
    /// The full size when it differs from `size_bytes` and is known.
    pub(super) total_bytes: Option<u64>,
    /// Why the body ended before the server finished it, if it did.
    pub(super) incomplete: Option<BodyIncomplete>,
    /// Base64 of the bytes when they are not UTF-8 or are a previewable image.
    pub(super) base64: Option<String>,
    /// The captured bytes.
    pub(super) raw: bytes::Bytes,
}

/// How one body read ends besides the server finishing it.
pub(super) struct BodyReadOptions<'a> {
    pub(super) limits: &'a ExecutorLimits,
    /// The exchange's total deadline (`total_timeout_ms`), if any.
    pub(super) deadline: Option<tokio::time::Instant>,
    pub(super) cancel: Option<&'a CancelToken>,
    /// A cancel keeps what arrived instead of failing with [`ExecutionError::Cancelled`].
    pub(super) keep_partial_on_cancel: bool,
    /// Every received byte is written here, past the cap too.
    pub(super) sink: Option<&'a Path>,
}

/// What waiting for the next chunk produced.
enum Next {
    Chunk(Result<Option<bytes::Bytes>, reqwest::Error>),
    Deadline,
    Cancelled,
}

async fn next_chunk(
    response: &mut reqwest::Response,
    deadline: Option<tokio::time::Instant>,
    cancel: Option<&CancelToken>,
) -> Next {
    let timed = async {
        match deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, response.chunk())
                .await
                .map_or(Next::Deadline, Next::Chunk),
            None => Next::Chunk(response.chunk().await),
        }
    };
    until_cancelled(cancel, timed)
        .await
        .unwrap_or(Next::Cancelled)
}

/// Opens (truncating) the save-to-file destination.
async fn open_sink(path: &Path) -> Result<tokio::fs::File, ExecutionError> {
    tokio::fs::File::create(path)
        .await
        .map_err(|error| sink_error(path, &error))
}

fn sink_error(path: &Path, error: &std::io::Error) -> ExecutionError {
    ExecutionError::BodyWrite {
        path: path.display().to_string(),
        message: error.to_string(),
    }
}

/// Reads the response body. The size cap in `options.limits` bounds what is captured; without a
/// sink the read stops there, with one every byte still goes to the file. The idle timeout of
/// the client, the total deadline and (with `keep_partial_on_cancel`) a cancel end the read early
/// and keep what arrived, marked in `incomplete`; a cancel without it fails with
/// [`ExecutionError::Cancelled`] carrying the bytes received.
pub(super) async fn read_response_body(
    mut response: reqwest::Response,
    response_headers: &HeaderMap,
    options: BodyReadOptions<'_>,
) -> Result<ResponseBody, ExecutionError> {
    let cap = options.limits.max_body_bytes;
    let mut sink = match options.sink {
        Some(path) => Some(open_sink(path).await?),
        None => None,
    };
    let mut body = Vec::new();
    let mut truncated = false;
    let mut incomplete = None;
    let mut received: u64 = 0;

    loop {
        let chunk = match next_chunk(&mut response, options.deadline, options.cancel).await {
            Next::Chunk(Ok(Some(chunk))) => chunk,
            Next::Chunk(Ok(None)) => break,
            // The client's read timeout restarts with every chunk: this is a stalled stream.
            Next::Chunk(Err(error)) if error.is_timeout() => {
                incomplete = Some(BodyIncomplete::IdleTimeout);
                break;
            }
            Next::Chunk(Err(error)) => {
                return Err(ExecutionError::BodyRead(error.without_url().to_string()));
            }
            Next::Deadline => {
                incomplete = Some(BodyIncomplete::TotalTimeout);
                break;
            }
            Next::Cancelled if options.keep_partial_on_cancel => {
                incomplete = Some(BodyIncomplete::Cancelled);
                break;
            }
            Next::Cancelled => {
                return Err(ExecutionError::Cancelled {
                    bytes_received: received,
                });
            }
        };
        if chunk.is_empty() {
            continue;
        }
        received = received.saturating_add(chunk.len() as u64);
        if let (Some(file), Some(path)) = (sink.as_mut(), options.sink) {
            file.write_all(&chunk)
                .await
                .map_err(|error| sink_error(path, &error))?;
        }
        if truncated {
            continue;
        }
        let remaining = cap.saturating_sub(body.len());
        if chunk.len() > remaining {
            body.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            if sink.is_none() {
                break;
            }
        } else {
            body.extend_from_slice(&chunk);
        }
    }
    if let (Some(mut file), Some(path)) = (sink, options.sink) {
        file.flush()
            .await
            .map_err(|error| sink_error(path, &error))?;
    }

    let body_size_bytes = body.len();
    let total_bytes = if options.sink.is_some() {
        (received != body_size_bytes as u64).then_some(received)
    } else if truncated {
        declared_length(response_headers).filter(|length| *length > body_size_bytes as u64)
    } else {
        None
    };
    let decoded = decode_text(&body, response_headers);
    // A body above the inline limit is stored by handle and its base64 dropped, so encoding up
    // to 10 MiB here would be thrown away; the exact bytes travel in `raw`.
    let body_base64 = (body.len() <= options.limits.inline_body_bytes
        && (!decoded.exact_utf8
            || (is_image_content_type(response_headers)
                && !truncated
                && incomplete.is_none()
                && body.len() <= MAX_IMAGE_PREVIEW_BYTES)))
        .then(|| BASE64_STANDARD.encode(&body));
    let text = decoded.text;

    Ok(ResponseBody {
        text,
        size_bytes: body_size_bytes,
        truncated,
        total_bytes,
        incomplete,
        base64: body_base64,
        raw: bytes::Bytes::from(body),
    })
}

/// The declared `Content-Length`. The client strips it when it decompresses a body, so a value
/// that is left counts the bytes the read sees.
fn declared_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok())
}

struct DecodedText {
    text: String,
    /// The bytes were valid UTF-8 and decoded as such, so they can be rebuilt from `text`.
    exact_utf8: bool,
}

/// Decodes `body` with the charset the Content-Type declares (GBK, Shift_JIS,
/// ISO-8859-1 and every other WHATWG label). Without a declared charset, or for UTF-8, it stays
/// lossy UTF-8, so an undeclared body never changes reading.
fn decode_text(body: &[u8], headers: &HeaderMap) -> DecodedText {
    let declared = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(declared_encoding);
    if let Some(encoding) = declared {
        let (text, _) = encoding.decode_without_bom_handling(body);
        return DecodedText {
            text: text.into_owned(),
            exact_utf8: false,
        };
    }
    match String::from_utf8_lossy(body) {
        Cow::Borrowed(text) => DecodedText {
            text: text.to_owned(),
            exact_utf8: true,
        },
        Cow::Owned(text) => DecodedText {
            text,
            exact_utf8: false,
        },
    }
}

/// The non-UTF-8 encoding a Content-Type value declares, if any.
fn declared_encoding(content_type: &str) -> Option<&'static encoding_rs::Encoding> {
    declared_charset(content_type)
        .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
        .filter(|encoding| *encoding != encoding_rs::UTF_8)
}

/// Decodes stored body bytes the way the executor decoded the inline text: with the charset
/// `content_type` declares, else lossy UTF-8 (a body kept by handle).
#[must_use]
pub fn decode_body_text<'a>(body: &'a [u8], content_type: Option<&str>) -> Cow<'a, str> {
    match content_type.and_then(declared_encoding) {
        Some(encoding) => encoding.decode_without_bom_handling(body).0,
        None => String::from_utf8_lossy(body),
    }
}

/// The `charset` parameter of a Content-Type value, unquoted.
fn declared_charset(content_type: &str) -> Option<&str> {
    content_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches('"'))
            .filter(|value| !value.is_empty())
    })
}

fn is_image_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim().to_ascii_lowercase().starts_with("image/"))
}
