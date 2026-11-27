//! Encoding of request bodies (raw, form, multipart, GraphQL) and the masked preview of what was sent.

use crate::executor::request::resolve_value;
use crate::executor::types::{ExecutionError, ExecutionHeader};
use bytes::Bytes;
use reqwest::RequestBuilder;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::multipart::{Form, Part};
use serde_json::{Map, Value};
use spanbay_base::crypto;
use spanbay_base::secrets::mask_secrets;
use spanbay_model::models::bounded_body_preview;
use spanbay_model::{
    AuthConfig, FormPart, HttpMethod, KeyValue, RawLanguage, Request, RequestBody, VariableScopes,
};
use std::path::{Path, PathBuf};
use url::form_urlencoded::Serializer;

/// Largest request body (a binary file, or all parts of a multipart body) the executor sends.
/// Kept apart from the response cap: both bodies stream from disk instead of memory.
pub const MAX_REQUEST_BODY_BYTES: u64 = 1024 * 1024 * 1024;

/// Read size for streaming and hashing request-body files.
const FILE_CHUNK_BYTES: usize = 64 * 1024;

/// Cap for the captured outgoing request body preview.
const MAX_SENT_BODY_PREVIEW_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BodyKind {
    HttpRest,
    Graphql,
}

/// The body actually attached to the outgoing request, before preview capping.
pub(super) struct SentBody {
    pub(super) text: String,
    size_bytes: usize,
    /// The exact bytes on the wire when `text` is only a description of them (a streamed file
    /// or a multipart body built here); `None` when `text` is the payload itself.
    chunks: Option<Vec<Chunk>>,
}

impl SentBody {
    pub(super) fn text(text: String) -> Self {
        let size_bytes = text.len();
        Self {
            text,
            size_bytes,
            chunks: None,
        }
    }
}

/// One piece of a streamed request body.
#[derive(Debug, Clone)]
enum Chunk {
    Bytes(Bytes),
    /// The first `len` bytes of the file at `path` (its length when it was checked).
    File {
        path: PathBuf,
        len: u64,
    },
}

/// A streamed body of `chunks`. Files are read in [`FILE_CHUNK_BYTES`] steps when the request
/// goes out, never held whole; a file that shrank since its check fails the send.
fn streamed_body(chunks: Vec<Chunk>) -> reqwest::Body {
    use tokio::io::AsyncReadExt as _;
    let state = (chunks.into_iter(), None::<(tokio::fs::File, u64)>);
    let stream = futures_util::stream::try_unfold(state, |(mut rest, mut open)| async move {
        loop {
            if let Some((file, left)) = open.as_mut() {
                if *left == 0 {
                    open = None;
                    continue;
                }
                let mut buffer = vec![
                    0;
                    usize::try_from(*left).map_or(FILE_CHUNK_BYTES, |left| left
                        .min(FILE_CHUNK_BYTES))
                ];
                let read = file.read(&mut buffer).await?;
                if read == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "request body file shrank while it was sent",
                    ));
                }
                buffer.truncate(read);
                *left -= read as u64;
                return Ok(Some((Bytes::from(buffer), (rest, open))));
            }
            match rest.next() {
                None => return Ok(None),
                Some(Chunk::Bytes(bytes)) => return Ok(Some((bytes, (rest, None)))),
                Some(Chunk::File { path, len }) => {
                    open = Some((tokio::fs::File::open(&path).await?, len));
                }
            }
        }
    });
    reqwest::Body::wrap_stream(stream)
}

/// SHA-256 (lowercase hex) of the exact bytes `body` sends, for SigV4's payload hash. Files are
/// hashed in [`FILE_CHUNK_BYTES`] steps on the blocking pool, so a large upload is signed over
/// its real bytes without being loaded.
pub(super) async fn payload_sha256(body: Option<&SentBody>) -> Result<String, ExecutionError> {
    let Some(body) = body else {
        return Ok(crypto::sha256_hex(b""));
    };
    let Some(chunks) = body.chunks.clone() else {
        return Ok(crypto::sha256_hex(body.text.as_bytes()));
    };
    blocking_file_task("request body", move || {
        use sha2::Digest as _;
        use std::io::Read as _;
        let mut hasher = sha2::Sha256::new();
        let mut buffer = vec![0; FILE_CHUNK_BYTES];
        for chunk in &chunks {
            match chunk {
                Chunk::Bytes(bytes) => hasher.update(bytes),
                Chunk::File { path, len } => {
                    let display = path.display().to_string();
                    let file_error = |message: String| ExecutionError::MultipartFileRead {
                        path: display.clone(),
                        message,
                        too_large: false,
                    };
                    let mut file = std::fs::File::open(path)
                        .map_err(|error| file_error(error.to_string()))?
                        .take(*len);
                    let mut hashed = 0u64;
                    loop {
                        let read = file
                            .read(&mut buffer)
                            .map_err(|error| file_error(error.to_string()))?;
                        if read == 0 {
                            break;
                        }
                        hasher.update(&buffer[..read]);
                        hashed += read as u64;
                    }
                    if hashed != *len {
                        return Err(file_error("file changed while it was signed".to_owned()));
                    }
                }
            }
        }
        Ok(crypto::hex_lower(&hasher.finalize()))
    })
    .await
}

/// Attaches `chunks` as a streamed body with an exact Content-Length.
fn attach_streamed(builder: RequestBuilder, chunks: &[Chunk], size_bytes: u64) -> RequestBuilder {
    builder
        .header(CONTENT_LENGTH, size_bytes)
        .body(streamed_body(chunks.to_vec()))
}

/// Longest a secret grows when encoded by [`mask_secrets`] (a JSON `\u00XX` escape per byte).
const MAX_ENCODING_GROWTH: usize = 6;

/// Masks secrets in the sent body and caps it for display: `(preview, size, truncated)`.
///
/// Only the leading bytes are masked: the window reaches past the preview cap by the longest
/// encoded secret, so a secret straddling the cap is masked whole before the cut.
pub(super) fn sent_body_preview(
    body: Option<SentBody>,
    secret_values: &[String],
) -> (Option<String>, usize, bool) {
    let Some(body) = body else {
        return (None, 0, false);
    };

    let longest = secret_values.iter().map(String::len).max().unwrap_or(0);
    let window = body.text.floor_char_boundary(
        MAX_SENT_BODY_PREVIEW_BYTES.saturating_add(longest.saturating_mul(MAX_ENCODING_GROWTH)),
    );
    let masked = mask_secrets(&body.text[..window], secret_values);
    let (text, truncated) = bounded_body_preview(&masked, MAX_SENT_BODY_PREVIEW_BYTES);

    (
        Some(text),
        body.size_bytes,
        truncated || window < body.text.len(),
    )
}

/// Attaches the REST request body to `builder` and records the content type and sent body.
pub(super) async fn apply_http_rest_body(
    mut builder: RequestBuilder,
    request: &Request,
    scopes: &VariableScopes,
    content_type_header: Option<&str>,
    sent_headers: &mut Vec<ExecutionHeader>,
) -> Result<(RequestBuilder, Option<SentBody>), ExecutionError> {
    // GET/HEAD carry a body only when the editor holds actual content for one.
    let skip_empty = matches!(request.method, HttpMethod::Get | HttpMethod::Head);
    match &request.body {
        RequestBody::None => Ok((builder, None)),
        RequestBody::Json { content } => {
            let body = resolve_value(content, scopes)?;
            if skip_empty && body.is_empty() {
                return Ok((builder, None));
            }
            if content_type_header.is_none() {
                builder = add_content_type(builder, "application/json", sent_headers);
            }
            Ok((builder.body(body.clone()), Some(SentBody::text(body))))
        }
        RequestBody::RawText { content, language } => {
            let body = resolve_value(content, scopes)?;
            if skip_empty && body.is_empty() {
                return Ok((builder, None));
            }
            if content_type_header.is_none() {
                let content_type = language.unwrap_or(RawLanguage::Text).content_type();
                builder = add_content_type(builder, content_type, sent_headers);
            }
            Ok((builder.body(body.clone()), Some(SentBody::text(body))))
        }
        RequestBody::FormUrlEncoded { fields } => {
            let body = encode_form_urlencoded(fields, scopes)?;
            if skip_empty && body.is_empty() {
                return Ok((builder, None));
            }
            if content_type_header.is_none() {
                builder =
                    add_content_type(builder, "application/x-www-form-urlencoded", sent_headers);
            }
            Ok((builder.body(body.clone()), Some(SentBody::text(body))))
        }
        RequestBody::Multipart { parts } => {
            if skip_empty && !parts.iter().any(|part| part.enabled) {
                return Ok((builder, None));
            }
            let signed = matches!(request.auth, AuthConfig::AwsSignatureV4 { .. });
            apply_multipart_body(
                builder,
                parts,
                scopes,
                content_type_header,
                signed,
                sent_headers,
            )
            .await
        }
        RequestBody::Binary {
            file_path,
            content_type,
        } => {
            // Trimmed like the multipart parts and the validation command, so a path the UI
            // reported as ready is the path that is read.
            let path = resolve_value(file_path, scopes)?.trim().to_owned();
            let len = checked_file_len_async(&path).await?;
            let content_type = content_type
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("application/octet-stream");
            if content_type_header.is_none() {
                builder = add_content_type(builder, content_type, sent_headers);
            }
            let chunks = vec![Chunk::File {
                path: PathBuf::from(&path),
                len,
            }];
            Ok((
                attach_streamed(builder, &chunks, len),
                Some(SentBody {
                    text: format!("<binary file: {}, {len} bytes>", file_name(&path)),
                    size_bytes: usize::try_from(len).unwrap_or(usize::MAX),
                    chunks: Some(chunks),
                }),
            ))
        }
        RequestBody::Graphql { .. } => Err(ExecutionError::UnsupportedBody),
    }
}

/// Length of the request-body file at `path`, rejecting anything that is not a regular file
/// (FIFOs and devices report a zero length yet never stop yielding) and files over the body
/// size limit before any byte is read. `display` is the path reported in errors. Blocking: call
/// it from [`blocking_file_task`].
fn checked_file_len(path: &Path, display: &str) -> Result<u64, ExecutionError> {
    let file_error = |message: &str| ExecutionError::MultipartFileRead {
        path: display.to_owned(),
        message: message.to_owned(),
        too_large: false,
    };
    let metadata = std::fs::metadata(path).map_err(|error| file_error(&error.to_string()))?;
    if !metadata.is_file() {
        return Err(file_error("not a regular file"));
    }
    if metadata.len() > MAX_REQUEST_BODY_BYTES {
        return Err(file_too_large(display));
    }
    Ok(metadata.len())
}

/// A file over the request body size limit.
fn file_too_large(display: &str) -> ExecutionError {
    ExecutionError::MultipartFileRead {
        path: display.to_owned(),
        message: format!(
            "file is larger than the {} MiB request body limit",
            MAX_REQUEST_BODY_BYTES >> 20
        ),
        too_large: true,
    }
}

/// Runs blocking file work on the blocking pool so it never stalls an async worker thread.
async fn blocking_file_task<T: Send + 'static>(
    display: &str,
    work: impl FnOnce() -> Result<T, ExecutionError> + Send + 'static,
) -> Result<T, ExecutionError> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|error| {
            Err(ExecutionError::MultipartFileRead {
                path: display.to_owned(),
                message: format!("file task failed: {error}"),
                too_large: false,
            })
        })
}

/// Checks (regular file, size limit) a multipart file part without blocking the async runtime.
async fn checked_file_len_async(path: &str) -> Result<u64, ExecutionError> {
    let display = path.to_owned();
    let target = PathBuf::from(path.trim());
    blocking_file_task(path, move || checked_file_len(&target, &display)).await
}

fn add_content_type(
    builder: RequestBuilder,
    content_type: &str,
    sent_headers: &mut Vec<ExecutionHeader>,
) -> RequestBuilder {
    record_content_type(content_type, sent_headers);
    builder.header(CONTENT_TYPE, content_type)
}

fn record_content_type(content_type: &str, sent_headers: &mut Vec<ExecutionHeader>) {
    sent_headers.push(ExecutionHeader {
        key: "Content-Type".to_owned(),
        value: content_type.to_owned(),
    });
}

/// Multipart part filename, honoring both `/` and `\\` separators so a
/// collection exported on Windows still names parts correctly elsewhere.
fn file_name(path: &str) -> &str {
    spanbay_base::platform::portable_file_name(path)
}

/// Attaches the GraphQL JSON payload to `builder` and records the content type and sent body.
pub(super) fn apply_graphql_body(
    mut builder: RequestBuilder,
    request: &Request,
    scopes: &VariableScopes,
    content_type_header: Option<&str>,
    sent_headers: &mut Vec<ExecutionHeader>,
) -> Result<(RequestBuilder, Option<SentBody>), ExecutionError> {
    match &request.body {
        RequestBody::Graphql {
            query,
            variables_json,
            operation_name,
        } => {
            let body = encode_graphql_body(
                query,
                variables_json.as_deref(),
                operation_name.as_deref(),
                scopes,
            )?;
            if content_type_header.is_none() {
                builder = add_content_type(builder, "application/json", sent_headers);
            }
            Ok((builder.body(body.clone()), Some(SentBody::text(body))))
        }
        _ => Err(ExecutionError::UnsupportedBody),
    }
}

async fn apply_multipart_body(
    builder: RequestBuilder,
    parts: &[FormPart],
    scopes: &VariableScopes,
    content_type_header: Option<&str>,
    signed: bool,
    sent_headers: &mut Vec<ExecutionHeader>,
) -> Result<(RequestBuilder, Option<SentBody>), ExecutionError> {
    let resolved_parts = resolve_multipart_parts(parts, scopes)?;
    let mut form = Form::new();
    // SigV4 signs the exact bytes, so a signed body is framed here and streamed from disk;
    // otherwise reqwest frames (and streams) it.
    let boundary = if signed {
        signed_boundary()?
    } else {
        form.boundary().to_owned()
    };
    let mut chunks = Vec::new();
    let mut descriptors = Vec::new();
    let mut size_bytes: usize = 0;

    for part in resolved_parts {
        match part {
            ResolvedMultipartPart::File { key, path } => {
                let file_len = checked_file_len_async(&path).await?;
                descriptors.push(format!("file: {}", file_name(&path)));
                size_bytes =
                    size_bytes.saturating_add(usize::try_from(file_len).unwrap_or(usize::MAX));
                ensure_multipart_within_limit(size_bytes, &path)?;
                if signed {
                    chunks.push(Chunk::Bytes(part_head(
                        &boundary,
                        &key,
                        Some(file_name(&path)),
                    )));
                    chunks.push(Chunk::File {
                        path: PathBuf::from(path.trim()),
                        len: file_len,
                    });
                    chunks.push(Chunk::Bytes(Bytes::from_static(b"\r\n")));
                } else {
                    let file_part = Part::file(Path::new(path.trim())).await.map_err(|error| {
                        ExecutionError::MultipartFileRead {
                            path: path.clone(),
                            message: error.to_string(),
                            too_large: false,
                        }
                    })?;
                    form = form.part(key, file_part);
                }
            }
            ResolvedMultipartPart::Text { key, value } => {
                descriptors.push(format!("text: {key}"));
                size_bytes = size_bytes.saturating_add(value.len());
                ensure_multipart_within_limit(size_bytes, &key)?;
                if signed {
                    let mut text = part_head(&boundary, &key, None).to_vec();
                    text.extend_from_slice(value.as_bytes());
                    text.extend_from_slice(b"\r\n");
                    chunks.push(Chunk::Bytes(Bytes::from(text)));
                } else {
                    form = form.text(key, value);
                }
            }
        }
    }

    let part_count = descriptors.len();
    let mut summary = SentBody {
        text: format!(
            "multipart/form-data: {part_count} {} ({})",
            if part_count == 1 { "part" } else { "parts" },
            descriptors.join(", ")
        ),
        size_bytes,
        chunks: None,
    };

    let content_type = multipart_content_type(content_type_header, &boundary);
    let value =
        HeaderValue::from_str(&content_type).map_err(|error| ExecutionError::InvalidHeader {
            key: "Content-Type".into(),
            message: error.to_string(),
        })?;
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, value);
    let builder = if signed {
        chunks.push(Chunk::Bytes(Bytes::from(format!("--{boundary}--\r\n"))));
        let wire_len = chunks
            .iter()
            .map(|chunk| match chunk {
                Chunk::Bytes(bytes) => bytes.len() as u64,
                Chunk::File { len, .. } => *len,
            })
            .sum();
        let builder = attach_streamed(builder.headers(headers), &chunks, wire_len);
        summary.chunks = Some(chunks);
        builder
    } else {
        builder.multipart(form).headers(headers)
    };
    sent_headers.retain(|header| !header.key.eq_ignore_ascii_case("content-type"));
    record_content_type(&content_type, sent_headers);

    Ok((builder, Some(summary)))
}

/// A fresh multipart boundary for a body framed here.
fn signed_boundary() -> Result<String, ExecutionError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| ExecutionError::ClientBuild(format!("no randomness: {error}")))?;
    Ok(format!("spanbay-{}", crypto::hex_lower(&bytes)))
}

/// The delimiter and headers that open one part; quotes and line breaks in names are
/// percent-encoded as browsers do.
fn part_head(boundary: &str, name: &str, file: Option<&str>) -> Bytes {
    let quoted = |value: &str| {
        value
            .replace('"', "%22")
            .replace('\r', "%0D")
            .replace('\n', "%0A")
    };
    let mut head = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"",
        quoted(name)
    );
    if let Some(file) = file {
        head.push_str(&format!(
            "; filename=\"{}\"\r\nContent-Type: application/octet-stream",
            quoted(file)
        ));
    }
    head.push_str("\r\n\r\n");
    Bytes::from(head)
}

/// The editor's Content-Type (any `boundary` parameter dropped) or `multipart/form-data`, with
/// the body's boundary.
fn multipart_content_type(header: Option<&str>, boundary: &str) -> String {
    let mut content_type = String::new();
    if let Some(header) = header {
        let mut start = 0;
        let mut quoted = false;
        let mut escaped = false;
        for (index, character) in header
            .char_indices()
            .chain(std::iter::once((header.len(), ';')))
        {
            if index == header.len() || (character == ';' && !quoted) {
                let part = header[start..index].trim();
                if start == 0
                    || !part
                        .split_once('=')
                        .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case("boundary"))
                {
                    if !content_type.is_empty() {
                        content_type.push_str("; ");
                    }
                    content_type.push_str(part);
                }
                start = index + 1;
            } else if escaped {
                escaped = false;
            } else if quoted && character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = !quoted;
            }
        }
    } else {
        content_type.push_str("multipart/form-data");
    }
    content_type.push_str("; boundary=");
    content_type.push_str(boundary);
    content_type
}

/// Rejects a multipart body whose parts together exceed the request body limit; each file is
/// already limited on its own, but ten of them would stream far more than the limit.
fn ensure_multipart_within_limit(size_bytes: usize, part: &str) -> Result<(), ExecutionError> {
    if size_bytes as u64 > MAX_REQUEST_BODY_BYTES {
        return Err(ExecutionError::MultipartFileRead {
            path: part.to_owned(),
            message: format!(
                "multipart body is larger than the {} MiB request body limit",
                MAX_REQUEST_BODY_BYTES >> 20
            ),
            too_large: true,
        });
    }
    Ok(())
}

enum ResolvedMultipartPart {
    Text { key: String, value: String },
    File { key: String, path: String },
}

fn resolve_multipart_parts(
    parts: &[FormPart],
    scopes: &VariableScopes,
) -> Result<Vec<ResolvedMultipartPart>, ExecutionError> {
    let mut resolved_parts = Vec::new();

    for part in parts.iter().filter(|part| part.enabled) {
        let key = resolve_value(&part.key, scopes)?;
        let key = key.trim().to_owned();
        if key.is_empty() {
            continue;
        }

        match part.file_path.as_deref() {
            Some(file_path) if !file_path.trim().is_empty() => {
                let file_path = resolve_value(file_path, scopes)?;
                resolved_parts.push(ResolvedMultipartPart::File {
                    key,
                    path: file_path,
                });
            }
            _ => {
                let value = resolve_value(&part.value, scopes)?;
                resolved_parts.push(ResolvedMultipartPart::Text { key, value });
            }
        }
    }

    Ok(resolved_parts)
}

fn encode_form_urlencoded(
    fields: &[KeyValue],
    scopes: &VariableScopes,
) -> Result<String, ExecutionError> {
    let mut serializer = Serializer::new(String::new());

    for field in fields.iter().filter(|field| field.enabled) {
        let key = resolve_value(&field.key, scopes)?;
        let value = resolve_value(&field.value, scopes)?;
        serializer.append_pair(&key, &value);
    }

    Ok(serializer.finish())
}

fn encode_graphql_body(
    query: &str,
    variables_json: Option<&str>,
    operation_name: Option<&str>,
    scopes: &VariableScopes,
) -> Result<String, ExecutionError> {
    let mut payload = Map::new();

    payload.insert("query".into(), Value::String(resolve_value(query, scopes)?));

    if let Some(variables_json) = variables_json {
        let variables_json = resolve_value(variables_json, scopes)?;
        if !variables_json.trim().is_empty() {
            let variables = serde_json::from_str(&variables_json)
                .map_err(|error| ExecutionError::InvalidGraphqlVariablesJson(error.to_string()))?;
            payload.insert("variables".into(), variables);
        }
    }

    if let Some(operation_name) = operation_name {
        let operation_name = resolve_value(operation_name, scopes)?;
        if !operation_name.trim().is_empty() {
            payload.insert("operationName".into(), Value::String(operation_name));
        }
    }

    serde_json::to_string(&Value::Object(payload))
        .map_err(|error| ExecutionError::GraphqlBodySerialize(error.to_string()))
}
