//! Content-addressed attachment and journal blob store with format sniffing and PDF text extraction.
//!
//! Files live at `<root>/<sha256[0..2]>/<sha256>`, owner-only. The rows (names, kinds, links to
//! messages) live in `crate::storage` (`attachments`); window path grants and model-message part
//! resolution live in `spanbay-agent`.
//!
//! Limits are the caller's [`AttachmentLimits`] plus a fixed header rule: an image whose longest
//! edge is over [`MAX_IMAGE_EDGE_PX`], or whose header cannot be read, is refused, and so is
//! anything that is not an image, a PDF or text. Core never resizes or thumbnails (no image
//! crate): the webview downscales to `ai.agent.image_max_edge_px` and makes the 96 px thumbnail
//! before storing.
//!
//! The store also keeps plain blobs for the file-change journal ([`AttachmentStore::put_blob`]):
//! content-addressed like attachments but with no row; the caller keeps them alive through
//! [`AttachmentStore::gc`]'s `keep_extra`.

mod pdf;
mod sniff;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use spanbay_base::crypto::sha256_hex;
use spanbay_base::error::AppError;
use spanbay_base::platform::{create_private_dir_all, write_atomic};
use spanbay_model::agent::AttachmentKind;

pub use pdf::extract_pdf_text;
pub use sniff::{Sniffed, sniff};

/// The lock behind [`blobs_lock`].
static BLOBS: Mutex<()> = Mutex::new(());

/// Takes the process-wide blob lock, which serialises "write blob, then insert its row" against
/// garbage collection so a collection never deletes the blob of an attachment whose row is not
/// committed yet.
///
/// There is one lock for the whole process, shared by every [`AttachmentStore`] whatever its root,
/// and no [`AttachmentStore`] method takes it. Callers hold it across the whole "write the blob,
/// then commit its row" sequence, and across [`AttachmentStore::gc`] together with the row query
/// that builds its `keep` sets. Today that is the agent service's ingest, journal `put_blob`,
/// derived PDF text and attachment GC, and the data service's "clear attachments". A poisoned lock
/// is recovered: the state it guards lives on disk and in the database, not in the mutex.
pub fn blobs_lock() -> MutexGuard<'static, ()> {
    BLOBS.lock().unwrap_or_else(PoisonError::into_inner)
}

const MIB: u64 = 1024 * 1024;

/// Longest edge, in pixels, of an image core accepts (read from its header): the 2,048 px the
/// webview downscales to, plus 10 % tolerance.
pub const MAX_IMAGE_EDGE_PX: u32 = 2250;

/// What [`AttachmentStore::ingest`] stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ingested {
    /// SHA-256 of the content, hex; the blob's name.
    pub sha256: String,
    pub media_type: String,
    pub kind: AttachmentKind,
    pub size_bytes: u64,
    /// Line count; text files only.
    pub lines: Option<u32>,
    /// Pixel width; images only.
    pub width: Option<u32>,
    /// Pixel height; images only.
    pub height: Option<u32>,
}

/// Size caps for new attachments, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachmentLimits {
    /// Largest image (`ai.agent.max_image_mb`).
    pub image_bytes: u64,
    /// Largest file of any other kind (`ai.agent.max_file_mb`).
    pub file_bytes: u64,
}

impl AttachmentLimits {
    /// Limits from the settings' megabyte values (`ai.agent.max_image_mb`, `ai.agent.max_file_mb`),
    /// converted to bytes (1 MB = 1 MiB).
    #[must_use]
    pub fn from_mb(max_image_mb: u8, max_file_mb: u8) -> Self {
        Self {
            image_bytes: u64::from(max_image_mb) * MIB,
            file_bytes: u64::from(max_file_mb) * MIB,
        }
    }
}

/// Blobs under one directory, each named by the SHA-256 of its content.
#[derive(Debug, Clone)]
pub struct AttachmentStore {
    root: PathBuf,
    limits: AttachmentLimits,
}

impl AttachmentStore {
    /// A store over `root` (`DataDirs::attachments`) that refuses content above `limits`.
    #[must_use]
    pub fn new(root: PathBuf, limits: AttachmentLimits) -> Self {
        Self { root, limits }
    }

    /// The largest size any attachment may have: a source over it is refused before it is read.
    #[must_use]
    pub fn max_bytes(&self) -> u64 {
        self.limits.image_bytes.max(self.limits.file_bytes)
    }

    /// The image cap, in bytes.
    #[must_use]
    pub fn max_image_bytes(&self) -> u64 {
        self.limits.image_bytes
    }

    /// The validation error for content of `size` bytes over [`AttachmentStore::max_bytes`]: it
    /// is over the limit of every kind, so it names the larger one.
    #[must_use]
    pub fn too_large(&self, size: u64) -> AppError {
        if self.limits.file_bytes >= self.limits.image_bytes {
            over_limit("file", size, self.limits.file_bytes)
        } else {
            over_limit("image", size, self.limits.image_bytes)
        }
    }

    /// Sniffs `bytes`, stores them unless a blob with the same content exists, and describes them.
    ///
    /// # Errors
    /// [`AppError::Validation`] when the content is not a supported file (`is not a supported
    /// file`), for an image over the image cap or [`MAX_IMAGE_EDGE_PX`] or whose header cannot be
    /// read (`the image header cannot be read`), and for any other content over the file cap;
    /// [`AppError::Io`] when the blob cannot be written. Nothing is written when it fails.
    pub fn ingest(
        &self,
        name: &str,
        media_type_hint: &str,
        bytes: &[u8],
    ) -> Result<Ingested, AppError> {
        let size_bytes = bytes.len() as u64;
        let sniffed = sniff(bytes, name, media_type_hint).ok_or_else(|| {
            AppError::validation(format!(
                "{name} is not a supported file: attach an image (PNG, JPEG, WebP, GIF), a PDF or a text file"
            ))
        })?;
        self.check_limits(name, sniffed, size_bytes)?;
        let sha256 = self.put_blob(bytes)?;
        Ok(Ingested {
            sha256,
            media_type: sniffed.media_type.to_owned(),
            kind: sniffed.kind,
            size_bytes,
            lines: (sniffed.kind == AttachmentKind::Text).then(|| count_lines(bytes)),
            width: sniffed.width,
            height: sniffed.height,
        })
    }

    /// Stores `bytes` unless a blob with the same content exists and returns the SHA-256 (hex).
    /// No row, no size cap and no sniffing: for blobs a caller references itself (the file-change
    /// journal's pre-edit bytes), kept by naming them in [`AttachmentStore::gc`]'s `keep_extra`.
    ///
    /// # Errors
    /// [`AppError::Io`] when the blob cannot be written.
    pub fn put_blob(&self, bytes: &[u8]) -> Result<String, AppError> {
        let sha256 = sha256_hex(bytes);
        let path = self.blob_path(&sha256)?;
        if !path.exists() {
            if let Some(shard) = path.parent() {
                create_private_dir_all(shard).map_err(io_error(shard))?;
            }
            write_atomic(&path, bytes, true).map_err(io_error(&path))?;
        }
        Ok(sha256)
    }

    /// The whole blob.
    ///
    /// # Errors
    /// [`AppError::Validation`] for a malformed hash, [`AppError::NotFound`] without a blob.
    pub fn read(&self, sha256: &str) -> Result<Vec<u8>, AppError> {
        self.read_prefix(sha256, u64::MAX)
    }

    /// At most the first `limit` bytes of the blob.
    ///
    /// # Errors
    /// As [`AttachmentStore::read`].
    pub fn read_prefix(&self, sha256: &str, limit: u64) -> Result<Vec<u8>, AppError> {
        let path = self.blob_path(sha256)?;
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(AppError::not_found(format!(
                    "attachment blob {sha256} not found"
                )));
            }
            Err(error) => return Err(io_error(&path)(error)),
        };
        let mut bytes = Vec::new();
        file.take(limit)
            .read_to_end(&mut bytes)
            .map_err(io_error(&path))?;
        Ok(bytes)
    }

    /// Whether a blob with this hash is stored; a malformed hash is never stored.
    #[must_use]
    pub fn exists(&self, sha256: &str) -> bool {
        self.blob_path(sha256).is_ok_and(|path| path.is_file())
    }

    /// Deletes one blob; `false` when there was none.
    ///
    /// # Errors
    /// [`AppError::Validation`] for a malformed hash, [`AppError::Io`] when deleting fails.
    pub fn remove(&self, sha256: &str) -> Result<bool, AppError> {
        let path = self.blob_path(sha256)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(io_error(&path)(error)),
        }
    }

    /// Deletes every blob whose hash is in neither `keep` (attachment rows) nor `keep_extra`
    /// (blobs referenced elsewhere, e.g. the file-change journal) and the shard directories that
    /// empties. Returns how many blobs went. Foreign files (anything not named by a full hash) are
    /// left. That includes the `.<sha>.<pid>.<n>.tmp` staging files a crash in the middle of
    /// `write_atomic` leaves behind, which also keep their shard directory alive.
    ///
    /// # Errors
    /// [`AppError::Io`] when a directory cannot be read or a blob deleted.
    pub fn gc(
        &self,
        keep: &HashSet<String>,
        keep_extra: &HashSet<String>,
    ) -> Result<usize, AppError> {
        let shards = match fs::read_dir(&self.root) {
            Ok(shards) => shards,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(io_error(&self.root)(error)),
        };
        let mut removed = 0;
        for shard in shards {
            let shard = shard.map_err(io_error(&self.root))?.path();
            if !shard.is_dir() {
                continue;
            }
            for blob in fs::read_dir(&shard).map_err(io_error(&shard))? {
                let blob = blob.map_err(io_error(&shard))?.path();
                let Some(name) = blob.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if valid_hash(name) && !keep.contains(name) && !keep_extra.contains(name) {
                    fs::remove_file(&blob).map_err(io_error(&blob))?;
                    removed += 1;
                }
            }
            // Fails (and is ignored) while the shard still holds blobs.
            let _ = fs::remove_dir(&shard);
        }
        Ok(removed)
    }

    /// Refuses content `ingest` must not store. Images are checked for size, then for a readable
    /// header, then for the longest edge; anything else only for size.
    fn check_limits(&self, name: &str, sniffed: Sniffed, size_bytes: u64) -> Result<(), AppError> {
        if sniffed.kind != AttachmentKind::Image {
            if size_bytes > self.limits.file_bytes {
                return Err(over_limit("file", size_bytes, self.limits.file_bytes));
            }
            return Ok(());
        }
        if size_bytes > self.limits.image_bytes {
            return Err(over_limit("image", size_bytes, self.limits.image_bytes));
        }
        let (Some(width), Some(height)) = (sniffed.width, sniffed.height) else {
            return Err(AppError::validation(format!(
                "{name}: the image header cannot be read"
            )));
        };
        if width.max(height) > MAX_IMAGE_EDGE_PX {
            return Err(AppError::validation(format!(
                "image is {width} x {height} px; the limit is {MAX_IMAGE_EDGE_PX} px on the longest edge"
            )));
        }
        Ok(())
    }

    fn blob_path(&self, sha256: &str) -> Result<PathBuf, AppError> {
        if !valid_hash(sha256) {
            return Err(AppError::validation(
                "attachment hash must be 64 lowercase hex characters",
            ));
        }
        Ok(self.root.join(&sha256[..2]).join(sha256))
    }
}

/// 64 lowercase hex characters: the only names the store creates, and no path separator can
/// pass.
fn valid_hash(text: &str) -> bool {
    // Not `is_ascii_hexdigit`: it accepts upper case, which the store never writes and which a
    // case-insensitive file system would resolve to the same blob.
    text.len() == 64
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// The validation error for `size` bytes of `kind` over `limit` bytes, both shown in MB.
fn over_limit(kind: &str, size: u64, limit: u64) -> AppError {
    AppError::validation(format!(
        "{kind} is {} MB; the {kind} limit is {} MB",
        size.div_ceil(MIB),
        limit / MIB
    ))
}

/// Maps an I/O error on `path` to [`AppError::Io`]; the message is only built on failure.
fn io_error(path: &Path) -> impl FnOnce(io::Error) -> AppError {
    move |error| AppError::io(format!("{}: {error}", path.display()))
}

/// How many lines `bytes` holds: its newlines, plus one for a last line without one.
#[must_use]
pub fn count_lines(bytes: &[u8]) -> u32 {
    let newlines = memchr::memchr_iter(b'\n', bytes).count();
    let open_line = usize::from(bytes.last().is_some_and(|last| *last != b'\n'));
    u32::try_from(newlines + open_line).unwrap_or(u32::MAX)
}
