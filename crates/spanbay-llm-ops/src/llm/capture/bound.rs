//! Bounded copy on the request path: the size caps and base64 elision. One pass over the bytes,
//! no JSON parse and no redaction (the log-writer thread redacts).
//!
//! Order: the writer redacts the full text, then elides base64, then cuts to the caps, so a
//! secret that straddles a cut cannot survive as a fragment. For that the request path copies a
//! *margin* beyond each cap ([`BODY_COPY_MARGIN`], [`FRAME_COPY_MARGIN_FRAMES`],
//! [`FRAME_COPY_MARGIN_BYTES`]): a secret that starts before the cap is whole in the copy.
//! Base64 payloads over [`BASE64_ELISION_THRESHOLD`] are elided here already, before the cut,
//! because a run of base64 characters that long cannot hold a key (keys carry `-`, `_`, `.` or a
//! prefix that ends the run) and otherwise a 4 MiB image would push the rest of the body out of
//! the copy.

use std::borrow::Cow;
use std::fmt;

use super::model::{CapturedFrame, CapturedFrames};

/// The most body text one capture slot stores.
pub const MAX_BODY_BYTES: usize = 256 * 1024;
/// The most frames one stream stores.
pub const MAX_FRAMES: usize = 2_000;
/// The most frame text (event names plus data) one stream stores.
pub const MAX_FRAME_BYTES: usize = 256 * 1024;
/// A base64 payload longer than this is replaced by an elision marker.
pub const BASE64_ELISION_THRESHOLD: usize = 4 * 1024;
/// Bytes copied beyond [`MAX_BODY_BYTES`] so the writer can redact a secret that straddles the cap.
pub const BODY_COPY_MARGIN: usize = 4 * 1024;
/// Frames copied beyond [`MAX_FRAMES`] (a key streamed token by token spans a few dozen frames).
pub const FRAME_COPY_MARGIN_FRAMES: usize = 64;
/// Frame bytes copied beyond [`MAX_FRAME_BYTES`].
pub const FRAME_COPY_MARGIN_BYTES: usize = 16 * 1024;
/// How an elision marker starts (`<base64 214 KB elided>`).
pub(super) const ELISION_PREFIX: &str = "<base64 ";
/// How an elision marker ends.
const ELISION_SUFFIX: &str = " KB elided>";

/// A body as copied on the request path: bounded, not redacted. `Debug` shows the sizes and
/// flags, never the text.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct BoundedBody {
    /// At most [`MAX_BODY_BYTES`] + [`BODY_COPY_MARGIN`] of text (lossy for non-UTF-8 bytes); the
    /// writer cuts it to [`MAX_BODY_BYTES`] after redaction.
    pub text: String,
    /// The text was cut at the copy cap (always at a UTF-8 boundary).
    pub truncated: bool,
    /// A base64 payload was replaced by an elision marker.
    pub elided: bool,
    /// Size of the original bytes.
    pub original_bytes: u64,
}

impl fmt::Debug for BoundedBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoundedBody")
            .field("len", &self.text.len())
            .field("truncated", &self.truncated)
            .field("elided", &self.elided)
            .field("original_bytes", &self.original_bytes)
            .finish()
    }
}

/// Copies `bytes` into a [`BoundedBody`]: base64 payloads over [`BASE64_ELISION_THRESHOLD`]
/// (`data:…;base64,` URLs and the value of a JSON `"data"` key, which covers Anthropic
/// `source.data` and Gemini `inlineData.data`) become `<base64 214 KB elided>`, then the text is
/// cut at [`MAX_BODY_BYTES`] plus [`BODY_COPY_MARGIN`].
#[must_use]
pub fn bound_body(bytes: &[u8]) -> BoundedBody {
    let cap = MAX_BODY_BYTES + BODY_COPY_MARGIN;
    let (text, truncated, elided) = match elide_base64(bytes) {
        Some(mut work) => {
            let cut = cut_at_char_boundary(&work, cap).len();
            let truncated = cut < work.len();
            work.truncate(cut);
            (into_string_lossy(work), truncated, true)
        }
        None => {
            let cut = cut_at_char_boundary(bytes, cap);
            let truncated = cut.len() < bytes.len();
            (String::from_utf8_lossy(cut).into_owned(), truncated, false)
        }
    };
    BoundedBody {
        text,
        truncated,
        elided,
        original_bytes: bytes.len() as u64,
    }
}

/// `bytes` as text without a copy when they are UTF-8; lossy otherwise.
fn into_string_lossy(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

/// The longest prefix of `bytes` within `cap` that does not end inside a UTF-8 character. A
/// character spans at most four bytes, so the cut backs off at most three; when that finds no
/// character start (the bytes are not UTF-8), it stays at `cap` and the lossy decode replaces
/// the stray bytes.
fn cut_at_char_boundary(bytes: &[u8], cap: usize) -> &[u8] {
    if bytes.len() <= cap {
        return bytes;
    }
    let is_continuation = |at: usize| bytes[at] & 0xC0 == 0x80;
    let floor = cap.saturating_sub(3);
    let mut end = cap;
    // `bytes[end]` is the first excluded byte; a continuation byte means the cut is mid-character.
    while end > floor && is_continuation(end) {
        end -= 1;
    }
    if is_continuation(end) {
        end = cap;
    }
    &bytes[..end]
}

fn is_base64(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')
}

/// Whether the value starting at `at` is a candidate payload: right after `;base64,` or as the
/// string value of a `"data"` key.
fn payload_starts_at(bytes: &[u8], at: usize) -> bool {
    let before = &bytes[..at];
    if before.ends_with(b";base64,") {
        return true;
    }
    let Some(quote) = before.strip_suffix(b"\"") else {
        return false;
    };
    let colon = quote.trim_ascii_end();
    colon
        .strip_suffix(b":")
        .is_some_and(|key| key.trim_ascii_end().ends_with(b"\"data\""))
}

/// Replaces every long base64 payload; `None` when there was nothing to replace. Single pass:
/// only the bytes after a `,` (data URL) or a `"` (JSON string value) can start a payload.
fn elide_base64(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() <= BASE64_ELISION_THRESHOLD {
        return None;
    }
    let mut out: Option<Vec<u8>> = None;
    let mut copied = 0;
    let mut from = 0;
    while let Some(offset) = memchr::memchr2(b',', b'"', &bytes[from..]) {
        let start = from + offset + 1;
        from = start;
        if !payload_starts_at(bytes, start) {
            continue;
        }
        let run = bytes[start..]
            .iter()
            .take_while(|byte| is_base64(**byte))
            .count();
        let end = start + run;
        let closed = bytes[start - 1] == b',' || bytes.get(end) == Some(&b'"');
        if run > BASE64_ELISION_THRESHOLD && closed {
            let out = out.get_or_insert_with(|| Vec::with_capacity(bytes.len() / 2));
            out.extend_from_slice(&bytes[copied..start]);
            out.extend_from_slice(
                format!("{ELISION_PREFIX}{}{ELISION_SUFFIX}", (run + 512) / 1024).as_bytes(),
            );
            copied = end;
            from = end;
        }
    }
    let mut out = out?;
    out.extend_from_slice(&bytes[copied..]);
    Some(out)
}

/// `text` with its base64 payloads over [`BASE64_ELISION_THRESHOLD`] elided; borrowed when there
/// are none.
pub(super) fn elide_text(text: &str) -> Cow<'_, str> {
    match elide_base64(text.as_bytes()) {
        Some(bytes) => Cow::Owned(into_string_lossy(bytes)),
        None => Cow::Borrowed(text),
    }
}

/// The longest prefix of `text` within `cap` bytes that ends on a character boundary.
pub(super) fn cut_text(text: &str, cap: usize) -> &str {
    if text.len() <= cap {
        return text;
    }
    let mut end = cap;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Collects the frames of one stream: the caps plus the copy margin the writer needs to redact a
/// secret split across the cut. The writer cuts to the caps.
#[derive(Debug, Default)]
pub struct FrameRecorder {
    frames: Vec<CapturedFrame>,
    total: u64,
    bytes: usize,
    first_ms: Option<u64>,
    full: bool,
}

impl FrameRecorder {
    /// An empty recorder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one frame that arrived `at_ms` after the stream started. The first frame defines
    /// offset 0. Frames past the caps plus margins ([`MAX_FRAMES`] and [`FRAME_COPY_MARGIN_FRAMES`],
    /// [`MAX_FRAME_BYTES`] and [`FRAME_COPY_MARGIN_BYTES`]) are only counted; once one frame does
    /// not fit, no later frame is stored (the list stays a prefix of the stream).
    pub fn push(&mut self, at_ms: u64, event: Option<&str>, data: &str) {
        self.total += 1;
        let first = *self.first_ms.get_or_insert(at_ms);
        if self.full {
            return;
        }
        let event = event.filter(|name| !name.is_empty()).unwrap_or("message");
        let data = elide_text(data);
        let size = event.len() + data.len();
        if self.frames.len() >= MAX_FRAMES + FRAME_COPY_MARGIN_FRAMES
            || self.bytes + size > MAX_FRAME_BYTES + FRAME_COPY_MARGIN_BYTES
        {
            self.full = true;
            return;
        }
        self.bytes += size;
        self.frames.push(CapturedFrame {
            index: u32::try_from(self.frames.len() + 1).unwrap_or(u32::MAX),
            t_ms: at_ms.saturating_sub(first),
            event: event.to_owned(),
            data: data.into_owned(),
        });
    }

    /// The copied frames (up to the caps plus margin) and the footer counts.
    #[must_use]
    pub fn finish(self) -> CapturedFrames {
        let truncated = self.total > self.frames.len() as u64;
        CapturedFrames {
            frames: self.frames,
            total_frames: self.total,
            truncated,
        }
    }
}
