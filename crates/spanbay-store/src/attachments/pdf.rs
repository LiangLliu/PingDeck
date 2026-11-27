//! Best-effort text extraction from PDFs, for models that cannot read a PDF natively.
//!
//! Scans raw bytes for content streams (plain or `FlateDecode`), runs the text-showing operators
//! (`Tj`, `TJ`, `'`, `"` plus positioning operators that imply a line or word break; see
//! `content`) and returns the text.
//!
//! Hostile input is bounded: at most [`MAX_STREAMS`] streams, each ending within
//! [`MAX_STREAM_IN`] bytes of its `stream` keyword and inflated to at most [`MAX_STREAM_OUT`], and
//! at most [`MAX_TEXT_BYTES`] of output. The [`MAX_INFLATED_TOTAL`] budget is checked after each
//! stream is inflated, so inflating stops once it is reached and the real ceiling is about
//! `MAX_INFLATED_TOTAL + MAX_STREAM_OUT` (72 MiB).

mod content;
#[cfg(test)]
mod tests;

use std::borrow::Cow;
use std::io::Read;

use flate2::read::ZlibDecoder;
use memchr::memmem;

use content::show_text;

/// Most bytes of extracted text; the output is cut at a character boundary.
const MAX_TEXT_BYTES: usize = 1024 * 1024;
/// Most `stream` keyword hits examined, whether or not they turn out to start a stream.
const MAX_STREAMS: usize = 10_000;
/// How far past a `stream` keyword, in bytes, its `endstream` is looked for. A search window, not
/// a decode cap: a stream that does not end within it makes the whole file unreadable.
const MAX_STREAM_IN: usize = 16 * 1024 * 1024;
/// Most bytes one `FlateDecode` stream inflates to; the rest of it is never read.
const MAX_STREAM_OUT: u64 = 8 * 1024 * 1024;
/// Inflate budget for the whole file, in bytes, checked after each stream.
const MAX_INFLATED_TOTAL: u64 = 64 * 1024 * 1024;
/// How many bytes before a `stream` keyword are scanned for its dictionary.
const DICT_LOOKBACK: usize = 8 * 1024;

/// Dictionary markers (whitespace removed, see [`dictionary`]) of streams that never hold page
/// text. Object streams are handled before this list is consulted.
const SKIPPED_STREAMS: &[&[u8]] = &[
    b"/Subtype/Image",
    b"/Type/XRef",
    b"/Type/Metadata",
    b"/Type/EmbeddedFile",
];

/// Filters this module cannot decode: outside an object stream, a stream naming one makes the
/// whole file unreadable.
const OTHER_FILTERS: &[&[u8]] = &[
    b"ASCII85Decode",
    b"ASCIIHexDecode",
    b"LZWDecode",
    b"RunLengthDecode",
    b"DCTDecode",
    b"JPXDecode",
    b"CCITTFaxDecode",
    b"JBIG2Decode",
    b"Crypt",
];

/// Extracts the text of `pdf`'s content streams, or `None` when it has none or cannot be read as
/// a whole (scanned, encrypted, not a PDF, truncated, unsupported filter, or glyph-id font).
#[must_use]
pub fn extract_pdf_text(pdf: &[u8]) -> Option<String> {
    if !pdf.starts_with(b"%PDF-") || contains(pdf, b"/Encrypt") || composite_fonts(pdf) {
        return None;
    }
    let mut text = String::new();
    let mut inflated_total = 0_u64;
    let mut at = 0;
    for _ in 0..MAX_STREAMS {
        let Some(keyword) = find(pdf, b"stream", at) else {
            break;
        };
        let after = keyword + b"stream".len();
        at = after;
        let Some(data_start) = follows_delimiter(pdf, keyword)
            .then(|| data_offset(pdf, after))
            .flatten()
        else {
            continue;
        };
        // A stream with no `endstream` within `MAX_STREAM_IN` means a truncated or hostile file:
        // give up on all of it, the text collected so far included.
        let limit = pdf.len().min(data_start.saturating_add(MAX_STREAM_IN));
        let window = pdf.get(data_start..limit)?;
        let length = find(window, b"endstream", 0)?;
        at = data_start + length + b"endstream".len();
        let dict = dictionary(pdf, keyword);
        match handle_stream(&dict, &window[..length], &mut inflated_total, &mut text) {
            Next::Continue => {}
            Next::Stop => break,
            Next::Unreadable => return None,
        }
    }
    let text = tidy(&text);
    (!text.is_empty()).then_some(text)
}

/// What [`extract_pdf_text`] does after one stream.
enum Next {
    /// Look for the next stream.
    Continue,
    /// A budget is spent: return the text collected so far.
    Stop,
    /// Give up on the whole file.
    Unreadable,
}

/// Collects the text of one stream (dictionary `dict`, raw bytes `data`) into `text`.
fn handle_stream(dict: &[u8], data: &[u8], inflated_total: &mut u64, text: &mut String) -> Next {
    if contains(dict, b"/Type/ObjStm") {
        // Object streams hold no page text. They are inflated only to spot a composite font
        // declared inside; an unsupported filter is ignored here, and what inflates still counts
        // against the total budget.
        if let Content::Bytes(objects) = decode_stream(dict, data, inflated_total)
            && composite_fonts(&objects)
        {
            return Next::Unreadable;
        }
        return if *inflated_total >= MAX_INFLATED_TOTAL {
            Next::Stop
        } else {
            Next::Continue
        };
    }
    if SKIPPED_STREAMS.iter().any(|marker| contains(dict, marker)) {
        return Next::Continue;
    }
    match decode_stream(dict, data, inflated_total) {
        Content::Skip => {}
        Content::Unsupported => return Next::Unreadable,
        Content::Bytes(content) if contains(&content, b"BT") => {
            if !show_text(&content, text) {
                return Next::Unreadable;
            }
            text.push_str("\n\n");
        }
        Content::Bytes(_) => {}
    }
    if text.len() >= MAX_TEXT_BYTES || *inflated_total >= MAX_INFLATED_TOTAL {
        Next::Stop
    } else {
        Next::Continue
    }
}

/// Whether the byte before `keyword` ends a token (`>` or whitespace), so the `stream` found
/// there is a keyword and not the tail of a word such as `endstream`.
fn follows_delimiter(pdf: &[u8], keyword: usize) -> bool {
    keyword
        .checked_sub(1)
        .and_then(|before| pdf.get(before))
        .is_some_and(|byte| matches!(byte, b'>' | b' ' | b'\t' | b'\r' | b'\n'))
}

/// Whether `bytes` declare a composite (`Type0` / Identity-encoded) font, whose strings are glyph
/// ids this module cannot map to text.
fn composite_fonts(bytes: &[u8]) -> bool {
    [&b"/Type0"[..], b"/Identity-H", b"/Identity-V"]
        .iter()
        .any(|name| contains(bytes, name))
}

/// Where the data of a stream starts: after the end of line that must follow its keyword.
fn data_offset(pdf: &[u8], after: usize) -> Option<usize> {
    match pdf.get(after..after + 2)? {
        [b'\r', b'\n'] => Some(after + 2),
        [b'\n', _] | [b'\r', _] => Some(after + 1),
        _ => None,
    }
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    memmem::find(haystack.get(from..)?, needle).map(|pos| pos + from)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    memmem::find(haystack, needle).is_some()
}

/// The dictionary in front of the `stream` keyword at `keyword`: the bytes since the last `obj`
/// within [`DICT_LOOKBACK`], whitespace removed so that `/Type /XObject` matches markers written
/// as `/Type/XObject`.
fn dictionary(pdf: &[u8], keyword: usize) -> Vec<u8> {
    let window = pdf
        .get(keyword.saturating_sub(DICT_LOOKBACK)..keyword)
        .unwrap_or_default();
    let start = memmem::rfind(window, b"obj").map_or(0, |at| at + 3);
    window[start..]
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect()
}

/// A stream's decoded bytes, or why there are none.
enum Content<'a> {
    /// Not text (an unknown filter, or nothing inflated): keep scanning.
    Skip,
    /// An undecodable filter or corrupt data: give up on the whole file.
    Unsupported,
    /// The content, borrowed from the file when it has no filter.
    Bytes(Cow<'a, [u8]>),
}

/// Decodes one stream, adding what it inflates to `inflated_total`.
fn decode_stream<'a>(dict: &[u8], data: &'a [u8], inflated_total: &mut u64) -> Content<'a> {
    if !contains(dict, b"/Filter") {
        return Content::Bytes(Cow::Borrowed(data));
    }
    if OTHER_FILTERS.iter().any(|name| contains(dict, name)) {
        return Content::Unsupported;
    }
    if !contains(dict, b"/FlateDecode") {
        return Content::Skip;
    }
    let mut inflated = Vec::new();
    let read = ZlibDecoder::new(data)
        .take(MAX_STREAM_OUT)
        .read_to_end(&mut inflated);
    *inflated_total += inflated.len() as u64;
    if read.is_err() {
        return Content::Unsupported;
    }
    if inflated.is_empty() {
        Content::Skip
    } else {
        Content::Bytes(Cow::Owned(inflated))
    }
}

/// Trims trailing spaces, collapses runs of blank lines and caps the text at [`MAX_TEXT_BYTES`].
fn tidy(text: &str) -> String {
    let mut tidy = String::new();
    let mut blank = true;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            if !blank {
                tidy.push('\n');
            }
            blank = true;
        } else {
            tidy.push_str(line);
            tidy.push('\n');
            blank = false;
        }
    }
    tidy.truncate(tidy.floor_char_boundary(MAX_TEXT_BYTES));
    tidy.trim().to_owned()
}
