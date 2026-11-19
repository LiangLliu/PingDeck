//! Server-sent events: an incremental parser for upstream streams and a frame writer for ours.
//!
//! Single responsibility: turn raw response bytes into [`SseEvent`]s and back. The parser is
//! hand-written and tolerant: it accepts `\n`, `\r\n` and `\r` line ends, ignores comment lines,
//! joins multi-line `data:` fields with `\n`, strips one leading space after the colon, skips a
//! leading UTF-8 BOM and keeps a multi-byte character that a network chunk split.
//!
//! It is hand-written because it needs hard per-line and per-event limits (8 MiB each) reported
//! in-band as an [`OVERFLOW_EVENT`], which no SSE crate offers (and none is in `Cargo.lock`).

use serde_json::Value;

/// One dispatched server-sent event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field, when the frame had one.
    pub event: Option<String>,
    /// The `data:` lines joined with `\n`.
    pub data: String,
    /// The `id:` field, when present.
    pub id: Option<String>,
}

/// The longest single line the parser buffers.
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
/// The most `data:` bytes one event may accumulate.
const MAX_DATA_BYTES: usize = 8 * 1024 * 1024;
/// A line buffer that grew beyond this is freed after its line instead of kept for the next one,
/// so one huge line does not pin its memory for the rest of the stream.
const LINE_REUSE_LIMIT: usize = 64 * 1024;

/// The `event` name of the synthetic event emitted when a limit is exceeded; its `data` is the
/// reason. Stream decoders turn it into a decode error.
pub const OVERFLOW_EVENT: &str = "pingdeck-overflow";

/// Incremental parser: feed network chunks, get complete events.
#[derive(Debug, Default)]
pub struct SseParser {
    /// Bytes of the line being read (no terminator yet); never longer than `MAX_LINE_BYTES`.
    line: Vec<u8>,
    /// A `\r` just ended a line, so a directly following `\n` belongs to it.
    after_cr: bool,
    /// The BOM check has been done.
    started: bool,
    /// A limit was exceeded; all further input is ignored.
    overflowed: bool,
    /// Bytes buffered in `data` (each line counts its joining `\n`).
    data_len: usize,
    event: Option<String>,
    /// The `data:` lines so far, joined with `\n`.
    data: String,
    /// At least one `data:` line arrived (it may have been empty).
    has_data: bool,
    id: Option<String>,
}

impl SseParser {
    /// A parser with no buffered input.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Consumes `chunk` and returns every event it completed.
    ///
    /// A line or event larger than the limits yields one [`OVERFLOW_EVENT`] and the parser then
    /// ignores the rest of the stream.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        let mut remaining = chunk;
        if !self.started {
            self.skip_bom(&mut remaining);
        }
        let mut events = Vec::new();
        while !remaining.is_empty() && !self.overflowed {
            if self.after_cr {
                self.after_cr = false;
                if let Some(rest) = remaining.strip_prefix(b"\n") {
                    remaining = rest;
                    if remaining.is_empty() {
                        break;
                    }
                }
            }
            // `line` never exceeds the limit, so this cannot underflow.
            let room = MAX_LINE_BYTES - self.line.len();
            match memchr::memchr2(b'\n', b'\r', remaining) {
                Some(pos) => {
                    if pos > room {
                        self.overflow(&mut events, "an SSE line exceeds the 8 MiB limit");
                        break;
                    }
                    self.line.extend_from_slice(&remaining[..pos]);
                    remaining = self.consume_terminator(remaining, pos);
                    self.end_line(&mut events);
                }
                None => {
                    if remaining.len() > room {
                        self.overflow(&mut events, "an SSE line exceeds the 8 MiB limit");
                    } else {
                        self.line.extend_from_slice(remaining);
                    }
                    break;
                }
            }
        }
        events
    }

    /// Ends the stream: a final unterminated line and a pending event are dispatched.
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if self.overflowed {
            return events;
        }
        if !self.line.is_empty() {
            self.end_line(&mut events);
        }
        if !self.overflowed {
            self.dispatch(&mut events);
        }
        events
    }

    /// Skips a leading UTF-8 BOM, which may arrive split across chunks; its bytes are held in
    /// `line` until the check is done. A partial BOM followed by other bytes stays in the line.
    fn skip_bom(&mut self, remaining: &mut &[u8]) {
        const BOM: &[u8] = b"\xEF\xBB\xBF";
        while let Some((&byte, rest)) = remaining.split_first() {
            if byte != BOM[self.line.len()] {
                self.started = true;
                return;
            }
            self.line.push(byte);
            *remaining = rest;
            if self.line.len() == BOM.len() {
                self.line.clear();
                self.started = true;
                return;
            }
        }
    }

    /// Returns the input after the line terminator at `pos` (`\n`, `\r\n` or `\r`). A `\r` not
    /// followed by `\n` in this chunk sets `after_cr`, so a `\n` opening the next one is skipped.
    fn consume_terminator<'a>(&mut self, remaining: &'a [u8], pos: usize) -> &'a [u8] {
        if remaining[pos] == b'\r' {
            if remaining.get(pos + 1) == Some(&b'\n') {
                return &remaining[pos + 2..];
            }
            self.after_cr = true;
        }
        &remaining[pos + 1..]
    }

    /// Handles the complete line in `line`, then keeps the buffer for the next line unless it
    /// grew large or the line overflowed.
    fn end_line(&mut self, events: &mut Vec<SseEvent>) {
        let mut line = std::mem::take(&mut self.line);
        self.read_line(&line, events);
        if !self.overflowed && line.capacity() <= LINE_REUSE_LIMIT {
            line.clear();
            self.line = line;
        }
    }

    fn read_line(&mut self, line: &[u8], events: &mut Vec<SseEvent>) {
        if line.is_empty() {
            self.dispatch(events);
            return;
        }
        if line.first() == Some(&b':') {
            return;
        }
        let text = String::from_utf8_lossy(line);
        let (field, value) = match text.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (text.as_ref(), ""),
        };
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => {
                self.data_len += value.len() + 1;
                if self.data_len > MAX_DATA_BYTES {
                    self.overflow(events, "an SSE event's data exceeds the 8 MiB limit");
                    return;
                }
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "id" => self.id = Some(value.to_owned()),
            _ => {}
        }
    }

    /// Dispatches the pending event at a blank line. Unlike the WHATWG algorithm, a frame with
    /// only an `event:` field is dispatched (with empty data); decoders ignore empty data. A
    /// frame with neither field dispatches nothing and forgets its `id`.
    fn dispatch(&mut self, events: &mut Vec<SseEvent>) {
        self.data_len = 0;
        if !self.has_data && self.event.is_none() {
            self.id = None;
            return;
        }
        events.push(SseEvent {
            event: self.event.take(),
            data: std::mem::take(&mut self.data),
            id: self.id.take(),
        });
        self.has_data = false;
    }

    /// Drops all buffered input, ignores the rest of the stream and reports `reason` as an
    /// [`OVERFLOW_EVENT`].
    fn overflow(&mut self, events: &mut Vec<SseEvent>, reason: &str) {
        self.line = Vec::new();
        self.data = String::new();
        self.has_data = false;
        self.event = None;
        self.id = None;
        self.data_len = 0;
        self.overflowed = true;
        events.push(SseEvent {
            event: Some(OVERFLOW_EVENT.to_owned()),
            data: reason.to_owned(),
            id: None,
        });
    }
}

/// Renders one outgoing frame: an optional `event:` line, the `data:` line(s) and a blank line.
///
/// `data` is split on `\n` only, so it must not contain `\r`; callers pass compact JSON or
/// `[DONE]`. `event` must not contain a line break.
#[must_use]
pub fn frame(event: Option<&str>, data: &str) -> Vec<u8> {
    let mut out = String::with_capacity(data.len() + 32);
    if let Some(event) = event {
        out.push_str("event: ");
        out.push_str(event);
        out.push('\n');
    }
    for line in data.split('\n') {
        out.push_str("data: ");
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    out.into_bytes()
}

/// Appends one outgoing frame whose data is `data` as compact JSON, without building the JSON
/// text first.
///
/// The bytes equal `frame(event, &data.to_string())`: compact serde_json output escapes control
/// characters, so it holds no raw `\n` or `\r` and is a single `data:` line. `event` must not
/// contain a line break.
pub(crate) fn write_json_frame(out: &mut Vec<u8>, event: Option<&str>, data: &Value) {
    let start = out.len();
    if let Some(event) = event {
        out.extend_from_slice(b"event: ");
        out.extend_from_slice(event.as_bytes());
        out.push(b'\n');
    }
    out.extend_from_slice(b"data: ");
    if serde_json::to_writer(&mut *out, data).is_err() {
        // Not reachable: serializing a `Value` into a `Vec` cannot fail. Drop the partial frame.
        out.truncate(start);
        return;
    }
    out.extend_from_slice(b"\n\n");
}

/// [`write_json_frame`] into a new buffer.
#[must_use]
pub(crate) fn frame_json(event: Option<&str>, data: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_json_frame(&mut out, event, data);
    out
}

#[cfg(test)]
mod tests;
