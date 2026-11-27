//! Attachment use-cases: put a file or bytes, describe and read dropped files, read one back,
//! thumbnails, PDF text, journal blobs, collecting unused blobs, and the retention purges.
//!
//! Single responsibility: connect `crate::agent::attachments` (the blob store, sniffing, PDF text)
//! to the attachment rows in the store and the `ai.agent.{max_image_mb, max_file_mb}` settings, and
//! offer the run service the helpers it needs: loading sendable attachments for a new message and
//! resolving references in a request. Everything is synchronous (a shell wraps the calls that
//! touch files, and PDF extraction, in `blocking`). The workspace store lock is never held across
//! file IO and the secret store is never used.
//!
//! Core makes no thumbnails and resizes nothing: the webview downscales images to
//! `ai.agent.image_max_edge_px` and stores a 96 px thumbnail with the attachment
//! ([`agent_attachment_put_bytes_impl`], [`agent_attachment_set_thumb_impl`]); core only validates
//! them (sizes, header dimensions in the store, the thumbnail's `data:` URL).

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

use crate::agent::attachments::{
    AttachmentStore, DocumentSupport, blobs_lock, extract_pdf_text, resolve_attachment_refs, sniff,
};
use crate::agent::model::{AgentAttachment, AttachmentKind};
use crate::error::AppError;
use crate::llm::hub::HubMessage;
use crate::platform::portable_file_name;

use crate::app::factory_service::new_entity_id;
use crate::app::{AppState, command_error, current_time_ms};

/// Longest display name kept, in characters.
const MAX_NAME_CHARS: usize = 255;

/// Name of an attachment that arrives without a usable one.
const DEFAULT_NAME: &str = "attachment";

/// Largest thumbnail image, in bytes (under 16 KB).
const MAX_THUMB_BYTES: usize = 16 * 1024;

/// Longest edge of a thumbnail, in pixels.
const MAX_THUMB_EDGE_PX: u32 = 96;

/// The only `data:` URL prefixes a thumbnail may have.
const THUMB_PREFIXES: [&str; 2] = ["data:image/png;base64,", "data:image/jpeg;base64,"];

/// Most attachments one message carries.
pub(crate) const MAX_PER_MESSAGE: usize = 10;

/// Most bytes all attachments of one message add up to.
const MAX_MESSAGE_BYTES: u64 = 50 * 1024 * 1024;

/// The blob store under the data directory, capped by `ai.agent.max_image_mb` and `max_file_mb`.
///
/// # Errors
/// [`AppError::Unsupported`] for an in-memory state, which has no data directory.
pub(crate) fn store_for(state: &AppState) -> Result<AttachmentStore, AppError> {
    state.attachment_store()
}

fn display_name(name: &str) -> String {
    let name: String = portable_file_name(name.trim())
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    if name.trim().is_empty() {
        DEFAULT_NAME.to_owned()
    } else {
        name
    }
}

/// Checks a thumbnail: a `data:image/(png|jpeg);base64,` URL of valid base64 holding an image of
/// at most [`MAX_THUMB_BYTES`] bytes and [`MAX_THUMB_EDGE_PX`] pixels on the long edge, read from
/// its header. The URL goes to the webview unchanged, so nothing else may pass.
fn validate_thumb(thumb_data_url: &str) -> Result<(), AppError> {
    let payload = THUMB_PREFIXES
        .iter()
        .find_map(|prefix| thumb_data_url.strip_prefix(prefix))
        .ok_or_else(|| {
            AppError::validation("thumbnail must be a data:image/png or data:image/jpeg base64 URL")
        })?;
    // Reject before decoding: base64 is 4 characters per 3 bytes.
    if payload.len() > MAX_THUMB_BYTES.div_ceil(3) * 4 + 4 {
        return Err(AppError::validation(format!(
            "thumbnail is over {} KiB",
            MAX_THUMB_BYTES / 1024
        )));
    }
    let bytes = STANDARD
        .decode(payload)
        .map_err(|error| AppError::validation(format!("thumbnail is not base64: {error}")))?;
    if bytes.len() > MAX_THUMB_BYTES {
        return Err(AppError::validation(format!(
            "thumbnail is {} KiB; the limit is {} KiB",
            bytes.len().div_ceil(1024),
            MAX_THUMB_BYTES / 1024
        )));
    }
    let fits = sniff(&bytes, "", "")
        .filter(|sniffed| sniffed.kind == AttachmentKind::Image)
        .and_then(|sniffed| sniffed.width.zip(sniffed.height))
        .is_some_and(|(width, height)| width.max(height) <= MAX_THUMB_EDGE_PX);
    if !fits {
        return Err(AppError::validation(format!(
            "thumbnail must be an image of at most {MAX_THUMB_EDGE_PX} px on the long edge"
        )));
    }
    Ok(())
}

/// Reads a regular file of at most the store's cap; the size is checked before any byte is read
/// and the read itself stops one byte past the cap so a growing file cannot exhaust memory.
fn read_file(store: &AttachmentStore, path: &Path) -> Result<Vec<u8>, AppError> {
    let shown = path.display();
    let io_error = |error: io::Error| match error.kind() {
        io::ErrorKind::NotFound => AppError::not_found(format!("{shown}: file not found")),
        _ => AppError::io(format!("{shown}: {error}")),
    };
    let metadata = fs::metadata(path).map_err(io_error)?;
    if !metadata.is_file() {
        return Err(AppError::validation(format!(
            "{shown} is not a regular file"
        )));
    }
    if metadata.len() > store.max_bytes() {
        return Err(store.too_large(metadata.len()));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(io_error)?
        .take(store.max_bytes() + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    Ok(bytes)
}

/// Stores `bytes` and its row. A thumbnail is validated first and only an image may have one.
fn put(
    state: &AppState,
    store: &AttachmentStore,
    name: &str,
    hint: &str,
    bytes: &[u8],
    thumb_data_url: Option<String>,
) -> Result<AgentAttachment, AppError> {
    if let Some(thumb) = &thumb_data_url {
        validate_thumb(thumb)?;
        if !sniff(bytes, name, hint).is_some_and(|sniffed| sniffed.kind == AttachmentKind::Image) {
            return Err(AppError::validation("only an image has a thumbnail"));
        }
    }
    let name = display_name(name);
    let _guard = blobs_lock();
    let ingested = store.ingest(&name, hint, bytes)?;
    let row = AgentAttachment {
        id: new_entity_id("att"),
        sha256: ingested.sha256,
        name,
        media_type: ingested.media_type,
        size_bytes: ingested.size_bytes,
        kind: ingested.kind,
        width: ingested.width,
        height: ingested.height,
        thumb_data_url,
        lines: ingested.lines,
        removed: false,
        created_at_ms: current_time_ms(),
    };
    state.store().insert_attachment(&row).map_err(command_error)
}

/// Reads the PDF or text file at `path` and stores it. Putting the same content twice returns the
/// first row. An image is refused: the window reads images, downscales them and puts the result
/// by bytes ([`agent_attachment_put_bytes_impl`]), so the 2,048 px limit holds on every route.
///
/// # Errors
/// [`AppError::Unsupported`] without a data directory, [`AppError::Validation`] for a relative
/// path, a non-regular file, an image, a type that is neither PDF nor text, or a file over
/// `ai.agent.max_file_mb`, [`AppError::NotFound`] for a missing file, [`AppError::Io`] when a
/// file or blob IO fails.
pub fn agent_attachment_put_path_impl(
    state: &AppState,
    path: &str,
    name: Option<String>,
) -> Result<AgentAttachment, AppError> {
    let store = store_for(state)?;
    let file = Path::new(path);
    if !file.is_absolute() {
        return Err(AppError::validation("attachment path must be absolute"));
    }
    let name = name.unwrap_or_else(|| portable_file_name(path).to_owned());
    let bytes = read_file(&store, file)?;
    if sniff(&bytes, &name, "").is_some_and(|sniffed| sniffed.kind == AttachmentKind::Image) {
        return Err(AppError::validation(format!(
            "{name} is an image: images are stored from the window as bytes, not by path"
        )));
    }
    put(state, &store, &name, "", &bytes, None)
}

/// Stores bytes the webview holds (a downscaled image, pasted text) received as a raw IPC body.
/// `thumb_data_url` is the webview's 96 px thumbnail; see [`agent_attachment_set_thumb_impl`] for
/// its form. Putting the same content twice returns the first row (a thumbnail it lacked is
/// added).
///
/// # Errors
/// As [`agent_attachment_put_path_impl`], plus [`AppError::Validation`] for an image over 2,250 px
/// on its long edge or a thumbnail that is not an image's `data:image/(png|jpeg);base64,` URL of
/// at most 16 KiB and 96 px.
pub fn agent_attachment_put_bytes_impl(
    state: &AppState,
    name: &str,
    media_type: &str,
    bytes: &[u8],
    thumb_data_url: Option<String>,
) -> Result<AgentAttachment, AppError> {
    let store = store_for(state)?;
    if bytes.len() as u64 > store.max_bytes() {
        return Err(store.too_large(bytes.len() as u64));
    }
    put(state, &store, name, media_type, bytes, thumb_data_url)
}

/// One attachment row by id, with its thumbnail.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown id.
pub fn agent_attachment_get_impl(state: &AppState, id: &str) -> Result<AgentAttachment, AppError> {
    state
        .store()
        .get_attachment(id)
        .map_err(command_error)?
        .ok_or_else(|| AppError::not_found(format!("attachment {id} not found")))
}

/// Stores the thumbnail the webview made lazily for an attachment put by path; a later call
/// replaces it. Returns the updated row.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown id, [`AppError::Validation`] for a non-image attachment
/// or a thumbnail that is not a `data:image/(png|jpeg|webp);base64,` URL of at most 64 KiB.
pub fn agent_attachment_set_thumb_impl(
    state: &AppState,
    id: &str,
    thumb_data_url: &str,
) -> Result<AgentAttachment, AppError> {
    validate_thumb(thumb_data_url)?;
    if agent_attachment_get_impl(state, id)?.kind != AttachmentKind::Image {
        return Err(AppError::validation("only an image has a thumbnail"));
    }
    state
        .store()
        .set_attachment_thumb(id, thumb_data_url)
        .map_err(command_error)?;
    agent_attachment_get_impl(state, id)
}

/// The raw content of an attachment, for the raw IPC response (image previews, downloads).
///
/// # Errors
/// [`AppError::NotFound`] for an unknown id or a lost blob, [`AppError::Unsupported`] without a
/// data directory.
pub fn agent_attachment_get_bytes_impl(state: &AppState, id: &str) -> Result<Vec<u8>, AppError> {
    let attachment = agent_attachment_get_impl(state, id)?;
    store_for(state)?.read(&attachment.sha256)
}

/// Stores `bytes` as a plain blob (no attachment row) and returns its SHA-256: the file-change
/// journal's pre-edit bytes. Name the hash in `agent_file_changes` right away; until then a
/// collection could delete it, so [`agent_attachment_gc_impl`] must not run during a run.
///
/// # Errors
/// [`AppError::Unsupported`] without a data directory, [`AppError::Io`] when writing fails.
pub fn agent_blob_put_impl(state: &AppState, bytes: &[u8]) -> Result<String, AppError> {
    let store = store_for(state)?;
    let _guard = blobs_lock();
    store.put_blob(bytes)
}

/// The content of a blob by its SHA-256 (see [`agent_blob_put_impl`]).
///
/// # Errors
/// [`AppError::Validation`] for a malformed hash, [`AppError::NotFound`] without a blob,
/// [`AppError::Unsupported`] without a data directory.
pub fn agent_blob_read_impl(state: &AppState, sha256: &str) -> Result<Vec<u8>, AppError> {
    store_for(state)?.read(sha256)
}

/// Deletes attachment rows no message links to (a derived PDF text lives while its PDF does),
/// then every blob no remaining row and no file-change journal entry names; returns how many
/// blobs were deleted. A file added but not yet sent is unreferenced too: run this when no
/// composer holds pending attachments and no run is active (at startup, after a chat is deleted).
///
/// # Errors
/// [`AppError::Unsupported`] without a data directory, [`AppError::Io`] when deleting fails.
pub fn agent_attachment_gc_impl(state: &AppState) -> Result<usize, AppError> {
    let store = store_for(state)?;
    let _guard = blobs_lock();
    let orphan_ids: Vec<String> = state
        .store()
        .list_unreferenced_attachments()
        .map_err(command_error)?
        .into_iter()
        .map(|attachment| attachment.id)
        .collect();
    state
        .store()
        .delete_attachment_rows(&orphan_ids)
        .map_err(command_error)?;
    let (keep, journal): (HashSet<String>, HashSet<String>) = {
        let workspace = state.store();
        (
            workspace.referenced_shas().map_err(command_error)?,
            workspace.journal_blob_shas().map_err(command_error)?,
        )
    };
    store.gc(&keep, &journal)
}

/// The text attachment derived from the PDF `attachment_id`, made on first use: the text
/// [`extract_pdf_text`] finds is stored as a second attachment linked to the PDF
/// (`agent_attachments.extracted_of`) and returned again afterwards. `None` for an unknown id, a
/// non-PDF, a lost blob or a PDF without a text layer (scanned, encrypted). CPU-bound for a big
/// PDF: call it in `blocking`.
#[must_use]
pub fn ensure_extracted_text(state: &AppState, attachment_id: &str) -> Option<AgentAttachment> {
    let store = store_for(state).ok()?;
    let source = state.store().get_attachment(attachment_id).ok().flatten()?;
    extracted_text_for(state, &store, &source)
}

fn extracted_text_for(
    state: &AppState,
    store: &AttachmentStore,
    source: &AgentAttachment,
) -> Option<AgentAttachment> {
    if source.kind != AttachmentKind::Pdf {
        return None;
    }
    let existing = state
        .store()
        .get_extracted_attachment(&source.id)
        .ok()
        .flatten();
    if let Some(derived) = existing
        && store.exists(&derived.sha256)
    {
        return Some(derived);
    }
    let text = extract_pdf_text(&store.read(&source.sha256).ok()?)?;
    let name = format!("{}.txt", source.name);
    let _guard = blobs_lock();
    let ingested = store.ingest(&name, "text/plain", text.as_bytes()).ok()?;
    let row = AgentAttachment {
        id: new_entity_id("att"),
        sha256: ingested.sha256,
        name,
        media_type: ingested.media_type,
        size_bytes: ingested.size_bytes,
        kind: ingested.kind,
        width: None,
        height: None,
        thumb_data_url: None,
        lines: ingested.lines,
        removed: false,
        created_at_ms: current_time_ms(),
    };
    state
        .store()
        .insert_extracted_attachment(&row, &source.id)
        .ok()
}

/// The attachments to put on a new message, in the order of `ids`: at most 10 and 50 MB
/// together, none whose blob was cleared.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown id, [`AppError::Validation`] for too many or too large
/// attachments or one that was removed from the store.
pub(crate) fn load_sendable(
    state: &AppState,
    ids: &[String],
) -> Result<Vec<AgentAttachment>, AppError> {
    if ids.len() > MAX_PER_MESSAGE {
        return Err(AppError::validation(format!(
            "a message can have {MAX_PER_MESSAGE} attachments or 50 MB"
        )));
    }
    let store = store_for(state)?;
    let attachments = ids
        .iter()
        .map(|id| {
            let attachment = agent_attachment_get_impl(state, id)?;
            if !store.exists(&attachment.sha256) {
                return Err(AppError::validation(format!(
                    "{}: this attachment was removed from the store",
                    attachment.name
                )));
            }
            Ok(attachment)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let total: u64 = attachments
        .iter()
        .map(|attachment| attachment.size_bytes)
        .sum();
    if total > MAX_MESSAGE_BYTES {
        return Err(AppError::validation(format!(
            "a message can have {MAX_PER_MESSAGE} attachments or 50 MB"
        )));
    }
    Ok(attachments)
}

/// Replaces the attachment references of a request with their content; see
/// [`resolve_attachment_refs`]. `caps` says whether the target takes PDFs natively; without it a
/// PDF is sent as its [extracted text](ensure_extracted_text). A state without a data directory
/// has no attachments, so there is nothing to resolve. Blocking (file IO, PDF extraction).
pub(crate) fn resolve_for_model(
    state: &AppState,
    caps: DocumentSupport,
    messages: &mut [HubMessage],
) {
    let Ok(store) = store_for(state) else {
        return;
    };
    resolve_attachment_refs(
        &store,
        &|id| state.store().get_attachment(id).ok().flatten(),
        &|source| extracted_text_for(state, &store, source),
        caps,
        messages,
    );
}

/// What a dropped or picked path is, so the window knows how to bring it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PickedKind {
    /// PNG, JPEG, GIF or WebP: the window reads it ([`agent_attachment_read_image_impl`]),
    /// downscales it and stores the bytes.
    Image,
    /// A PDF: stored by path.
    Pdf,
    /// UTF-8 text or source: stored by path.
    Text,
    /// A folder: offered as "Use as folder".
    Folder,
    /// Anything else: refused with "not a supported file".
    Unsupported,
}

/// A dropped or picked path as the window sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickedFile {
    /// The canonical path; what the other path commands take.
    pub path: String,
    /// File name for the chip.
    pub name: String,
    /// Size in bytes (0 for a folder).
    pub size_bytes: u64,
    /// What it is, decided by its first bytes.
    pub kind: PickedKind,
}

/// Bytes of a file examined to tell what it is: enough for every image header and `%PDF-`.
const SNIFF_BYTES: u64 = 64 * 1024;

/// Opens `path` (already canonical) as a regular file and returns the handle with its size:
/// the size and the kind of file come from the open handle, so a swap after the check changes
/// nothing.
fn open_regular(path: &Path) -> Result<(File, u64), AppError> {
    let shown = path.display();
    let io_error = |error: io::Error| match error.kind() {
        io::ErrorKind::NotFound => AppError::not_found(format!("{shown}: file not found")),
        _ => AppError::io(format!("{shown}: {error}")),
    };
    let file = File::open(path).map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file() {
        return Err(AppError::validation(format!(
            "{shown} is not a regular file"
        )));
    }
    Ok((file, metadata.len()))
}

/// Names a dropped or picked path: its kind, size and display name. The path should be one the
/// shell resolved ([`crate::agent::attachments::PathGrants`]); a folder is `Folder` and a path
/// that does not exist `NotFound`.
///
/// # Errors
/// [`AppError::NotFound`] for a missing path, [`AppError::Io`] when the file cannot be read.
pub fn agent_attachment_describe_path_impl(path: &Path) -> Result<PickedFile, AppError> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| AppError::not_found(format!("{}: {error}", path.display())))?;
    let name = canonical.file_name().map_or_else(
        || DEFAULT_NAME.to_owned(),
        |name| display_name(&name.to_string_lossy()),
    );
    let shown = canonical.to_string_lossy().into_owned();
    if canonical.is_dir() {
        return Ok(PickedFile {
            path: shown,
            name,
            size_bytes: 0,
            kind: PickedKind::Folder,
        });
    }
    let (file, size_bytes) = open_regular(&canonical)?;
    let mut head = Vec::new();
    file.take(SNIFF_BYTES)
        .read_to_end(&mut head)
        .map_err(|error| AppError::io(format!("{shown}: {error}")))?;
    let kind = match sniff(&head, &name, "") {
        Some(sniffed) => match sniffed.kind {
            AttachmentKind::Image => PickedKind::Image,
            AttachmentKind::Pdf => PickedKind::Pdf,
            // A text file is valid UTF-8 as a whole; a first block cut mid-character still is.
            AttachmentKind::Text => PickedKind::Text,
        },
        None if size_bytes > SNIFF_BYTES && std::str::from_utf8(&head).is_err() => {
            // The 64 KiB cut may split a character: the block is text when only its end is bad.
            match std::str::from_utf8(&head) {
                Err(error) if error.error_len().is_none() && memchr::memchr(0, &head).is_none() => {
                    PickedKind::Text
                }
                _ => PickedKind::Unsupported,
            }
        }
        None => PickedKind::Unsupported,
    };
    Ok(PickedFile {
        path: shown,
        name,
        size_bytes,
        kind,
    })
}

/// Reads an image the user dropped or picked and returns its bytes for the window to downscale:
/// the path must be a regular file, at most `ai.agent.max_image_mb`, and
/// start with the magic bytes of PNG, JPEG, GIF or WebP; a `.png` that is an executable, or a
/// folder, is refused. The size cap applies to the original, which is larger than the 2,048 px
/// image the window stores, so the 2,250 px edge rule is not applied here.
///
/// # Errors
/// [`AppError::Unsupported`] without a data directory, [`AppError::NotFound`] for a missing
/// file, [`AppError::Validation`] for a non-regular file, an oversize file or content that is not
/// an image.
pub fn agent_attachment_read_image_impl(
    state: &AppState,
    path: &Path,
) -> Result<Vec<u8>, AppError> {
    let store = store_for(state)?;
    let canonical = fs::canonicalize(path)
        .map_err(|error| AppError::not_found(format!("{}: {error}", path.display())))?;
    let (file, size) = open_regular(&canonical)?;
    let limit = store.max_image_bytes();
    if size > limit {
        return Err(AppError::validation(format!(
            "image is {} MB; the image limit is {} MB",
            size.div_ceil(1024 * 1024),
            limit / (1024 * 1024)
        )));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| AppError::io(format!("{}: {error}", canonical.display())))?;
    if bytes.len() as u64 > limit {
        return Err(AppError::validation(
            "the image grew past the size limit while it was read",
        ));
    }
    let is_image =
        sniff(&bytes, "", "").is_some_and(|sniffed| sniffed.kind == AttachmentKind::Image);
    if !is_image {
        return Err(AppError::validation(format!(
            "{} is not an image (PNG, JPEG, WebP or GIF)",
            canonical.display()
        )));
    }
    Ok(bytes)
}
