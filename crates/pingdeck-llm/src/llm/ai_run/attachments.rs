//! Attachment placeholders of a Raw JSON body, resolved at send time.
//!
//! Single responsibility: replace every `{{attachment:<sha256 or its first 12+ hex digits>}}` (an
//! optional size hint may follow the hash: `{{attachment:ab12cd34ef56 214 KB}}`) by the base64 of
//! the blob in the content-addressed store `<attachments>/<sha256[0..2]>/<sha256>`. The bytes come
//! from the app's [`AttachmentStore`] (the same CAS the chat attachments use), which owns the
//! layout, the hash validation and the reads; this module keeps the placeholder syntax and the
//! AI-API limits (images 10 MB, PDFs 20 MB). The reader accepts hexadecimal references only (no
//! path can be spelled), refuses symbolic links, checks the magic bytes and never logs a byte of
//! data. Blocking file work: callers run it on the blocking pool.

use std::fs;
use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD};

use pingdeck_model::agent::AttachmentKind;
use pingdeck_store::attachments::{AttachmentLimits, AttachmentStore, sniff};

const OPEN: &str = "{{attachment:";
const CLOSE: &str = "}}";
/// Shortest reference accepted (the builder writes 12 hex digits).
const MIN_REF: usize = 12;
const SHA256_HEX: usize = 64;
/// AI-API caps: images up to 10 MB, PDFs up to 20 MB.
const IMAGE_MB: u8 = 10;
const PDF_MB: u8 = 20;

/// Why a placeholder could not be resolved; the text is shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentError(pub String);

impl std::fmt::Display for AttachmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether `body` holds a placeholder.
#[must_use]
pub fn has_placeholder(body: &str) -> bool {
    body.contains(OPEN)
}

/// `body` with every placeholder replaced by the blob's base64, as bytes.
///
/// # Errors
/// [`AttachmentError`] for a malformed or ambiguous reference, a missing store or blob, a blob of
/// an unsupported type, or one over its size limit.
pub fn resolve(body: &str, store: Option<&Path>) -> Result<Vec<u8>, AttachmentError> {
    let mut out = Vec::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find(OPEN) {
        out.extend_from_slice(&rest.as_bytes()[..start]);
        let after = &rest[start + OPEN.len()..];
        let end = after
            .find(CLOSE)
            .ok_or_else(|| AttachmentError("An attachment placeholder is not closed".to_owned()))?;
        let token = &after[..end];
        let reference: String = token
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect::<String>()
            .to_ascii_lowercase();
        let hint = &token[reference.len()..];
        if !(MIN_REF..=SHA256_HEX).contains(&reference.len())
            || !(hint.is_empty() || hint.starts_with(' '))
            || hint.contains(['{', '}'])
        {
            return Err(AttachmentError(format!(
                "`{OPEN}{token}{CLOSE}` is not an attachment reference (a hexadecimal sha256 of at least {MIN_REF} digits)"
            )));
        }
        let bytes = read_blob(store, &reference)?;
        out.extend_from_slice(STANDARD.encode(bytes).as_bytes());
        rest = &after[end + CLOSE.len()..];
    }
    out.extend_from_slice(rest.as_bytes());
    Ok(out)
}

/// The bytes of the blob `reference` names, validated.
fn read_blob(store: Option<&Path>, reference: &str) -> Result<Vec<u8>, AttachmentError> {
    let root = store.ok_or_else(|| missing(reference))?;
    // The same store the chat attachments use: it owns the shard layout, the hash check and the
    // read. Its caps are the AI-API ones here (a request body's attachments).
    let store = AttachmentStore::new(
        root.to_path_buf(),
        AttachmentLimits::from_mb(IMAGE_MB, PDF_MB),
    );
    let file_limit = store.max_bytes();
    let sha256 = resolve_hash(root, reference)?;
    let path = root.join(&sha256[..2]).join(&sha256);
    // The store follows a symbolic link; its blobs are only ever written by the app, so refuse one.
    let metadata = fs::symlink_metadata(&path).map_err(|_| missing(reference))?;
    if !metadata.is_file() {
        return Err(missing(reference));
    }
    if metadata.len() > file_limit {
        return Err(too_big(reference, file_limit));
    }
    let bytes = store
        .read_prefix(&sha256, file_limit + 1)
        .map_err(|_| missing(reference))?;
    let limit = match sniff(&bytes, "", "") {
        Some(sniffed) if sniffed.kind == AttachmentKind::Image => store.max_image_bytes(),
        Some(sniffed) if sniffed.kind == AttachmentKind::Pdf => file_limit,
        _ => return Err(not_an_attachment(reference)),
    };
    if bytes.len() as u64 > limit {
        return Err(too_big(reference, limit));
    }
    Ok(bytes)
}

/// The full hash `reference` names: itself when it is complete, else the one blob of its shard
/// whose name starts with it.
fn resolve_hash(root: &Path, reference: &str) -> Result<String, AttachmentError> {
    if reference.len() == SHA256_HEX {
        return Ok(reference.to_owned());
    }
    let folder = root.join(&reference[..2]);
    let mut matches = fs::read_dir(&folder)
        .map_err(|_| missing(reference))?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            name.len() == SHA256_HEX
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                && name.starts_with(reference)
        });
    let found = matches.next().ok_or_else(|| missing(reference))?;
    if matches.next().is_some() {
        return Err(AttachmentError(format!(
            "Attachment reference {reference} matches more than one file"
        )));
    }
    Ok(found)
}

fn missing(reference: &str) -> AttachmentError {
    AttachmentError(format!("Attachment {} is missing", &reference[..MIN_REF]))
}

fn not_an_attachment(reference: &str) -> AttachmentError {
    AttachmentError(format!(
        "Attachment {} is not a png, jpeg, webp, gif or pdf file",
        &reference[..MIN_REF]
    ))
}

fn too_big(reference: &str, limit: u64) -> AttachmentError {
    AttachmentError(format!(
        "Attachment {} is over the {} MB limit",
        &reference[..MIN_REF],
        limit / (1024 * 1024)
    ))
}
