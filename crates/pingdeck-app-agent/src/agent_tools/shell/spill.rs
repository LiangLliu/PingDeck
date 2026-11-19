//! What the model sees of a command's output, and reading kept full output back.
//!
//! Single responsibility: keep the head and the tail of a command's output in bounded memory (at
//! most [`TOOL_OUTPUT_FILE_BYTES`], so the runner can keep all of it on disk when it cuts the
//! result for the model), and read a kept file back in pages ([`read_tool_output`]) for
//! `read_output` and the UI's "Show full output".

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub(super) use crate::agent::limits::TOOL_OUTPUT_FILE_BYTES;
use crate::agent::tools::shell_meta::valid_call_id;
use crate::agent::tools::shell_meta::{ToolOutputChunk, incomplete_tail_len};
use crate::error::AppError;

/// Most bytes one [`read_tool_output`] call returns.
pub const MAX_CHUNK_BYTES: u32 = 64 * 1024;
/// Bytes of the end of a command's output kept when it outgrows the capture.
const TAIL_KEEP: usize = 256 * 1024;

/// The kept file of `call_id` below `dir`, or `None` when the id is not a safe file name.
fn spill_path(dir: &Path, call_id: &str) -> Option<PathBuf> {
    valid_call_id(call_id).then(|| dir.join(call_id))
}

/// Reads up to `limit` bytes (at most [`MAX_CHUNK_BYTES`]) of the spilled output of `call_id`
/// from byte `offset`. A page never ends inside a UTF-8 character unless it is the last.
///
/// # Errors
/// [`AppError::Validation`] for an unsafe call id, [`AppError::NotFound`] when nothing was
/// spilled for the call, [`AppError::Io`]-style errors for a failed read.
pub fn read_tool_output(
    dir: &Path,
    call_id: &str,
    offset: u64,
    limit: u32,
) -> Result<ToolOutputChunk, AppError> {
    let path = spill_path(dir, call_id)
        .ok_or_else(|| AppError::validation("the call id is not a valid output name"))?;
    let not_found = || AppError::not_found("no full output was kept for this call");
    // A planted symlink must not turn this into a read of an arbitrary file.
    let meta = fs::symlink_metadata(&path).map_err(|_| not_found())?;
    if !meta.is_file() {
        return Err(not_found());
    }
    let total = meta.len();
    let io_error = |error: std::io::Error| AppError::io(format!("cannot read the output: {error}"));
    let mut file = File::open(&path).map_err(io_error)?;
    file.seek(SeekFrom::Start(offset.min(total)))
        .map_err(io_error)?;
    let want = u64::from(limit.clamp(1, MAX_CHUNK_BYTES));
    let mut buf = Vec::new();
    file.take(want).read_to_end(&mut buf).map_err(io_error)?;
    let mut start = offset.min(total);
    // An offset inside a multi-byte character starts at the next character.
    let skip = if start > 0 {
        buf.iter()
            .take(3)
            .take_while(|byte| **byte & 0xC0 == 0x80)
            .count()
    } else {
        0
    };
    buf.drain(..skip);
    start += skip as u64;
    let reaches_end = start + buf.len() as u64 >= total;
    if !reaches_end {
        buf.truncate(buf.len() - incomplete_tail_len(&buf));
    }
    let next_offset = start + buf.len() as u64;
    Ok(ToolOutputChunk {
        text: String::from_utf8_lossy(&buf).into_owned(),
        offset: start,
        next_offset,
        total,
        eof: next_offset >= total,
    })
}

/// The line between the kept head and tail of an over-long output.
pub(super) fn marker(omitted: usize) -> String {
    format!("\n[output truncated: {omitted} bytes from the middle were not kept]\n")
}

/// `head`, a truncation marker for `omitted` bytes, and `tail`, each cut on a character boundary.
fn join_cut(head: &[u8], tail: &[u8], omitted: usize) -> String {
    let split_head = incomplete_tail_len(head);
    let head = &head[..head.len() - split_head];
    let split_tail = tail
        .iter()
        .take(3)
        .take_while(|byte| **byte & 0xC0 == 0x80)
        .count();
    let tail = &tail[split_tail..];
    let omitted = omitted + split_head + split_tail;
    format!(
        "{}{}{}",
        String::from_utf8_lossy(head),
        marker(omitted),
        String::from_utf8_lossy(tail)
    )
}

/// `text` when it fits in `cap` bytes, else its first and last halves around a truncation marker.
#[must_use]
pub fn cap_text(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_owned();
    }
    let head_len = cap / 2;
    let tail_len = cap - head_len;
    let bytes = text.as_bytes();
    let omitted = bytes.len() - head_len - tail_len;
    join_cut(
        &bytes[..head_len],
        &bytes[bytes.len() - tail_len..],
        omitted,
    )
}

/// Keeps the beginning and the end of what a command printed, in bounded memory.
#[derive(Debug)]
pub(super) struct Capture {
    /// Bytes of the beginning kept.
    head_max: usize,
    /// Bytes of the end kept.
    tail_max: usize,
    /// The first `head_max` bytes.
    head: Vec<u8>,
    /// The most recent bytes after the head (trimmed lazily to about `tail_max`).
    tail: Vec<u8>,
    /// Every byte seen.
    total: usize,
}

impl Capture {
    /// A capture whose rendering is at most about `cap` bytes: everything up to `cap`, beyond it
    /// the start and the last [`TAIL_KEEP`] bytes (at most half of `cap`).
    pub(super) fn new(cap: usize) -> Self {
        let tail_max = TAIL_KEEP.min(cap / 2);
        Self {
            head_max: cap - tail_max,
            tail_max,
            head: Vec::new(),
            tail: Vec::new(),
            total: 0,
        }
    }

    /// Records `text` (always whole UTF-8 characters).
    pub(super) fn push(&mut self, text: &str) {
        let bytes = text.as_bytes();
        self.total += bytes.len();
        let (head, tail) = bytes.split_at(bytes.len().min(self.head_max - self.head.len()));
        self.head.extend_from_slice(head);
        self.tail.extend_from_slice(tail);
        if self.tail.len() > 2 * self.tail_max {
            self.tail.drain(..self.tail.len() - self.tail_max);
        }
    }

    /// Whether output went past the capture and its middle was dropped.
    pub(super) fn dropped(&self) -> bool {
        self.total > self.head_max + self.tail_max
    }

    /// The text: everything up to the cap, otherwise head, marker and tail cut on character
    /// boundaries.
    pub(super) fn render(&self) -> String {
        if !self.dropped() {
            let mut all = self.head.clone();
            all.extend_from_slice(&self.tail);
            return String::from_utf8_lossy(&all).into_owned();
        }
        let tail = &self.tail[self.tail.len().saturating_sub(self.tail_max)..];
        let omitted = self.total - self.head.len() - tail.len();
        join_cut(&self.head, tail, omitted)
    }
}
