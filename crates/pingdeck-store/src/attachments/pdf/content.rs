//! The content-stream layer: tokenises one decoded stream and runs its text operators.
//!
//! Only the operators that put text on the page or move to a new line or word are interpreted;
//! everything else is skipped. Strings are decoded as UTF-16BE (with a BOM) or WinAnsi.

use memchr::{memchr_iter, memchr2};

use super::MAX_TEXT_BYTES;

/// Most operands kept for one operator; further ones are dropped silently.
const MAX_OPERANDS: usize = 65_536;
/// A `TJ` displacement, in thousandths of a text space unit, below which (more negative) the gap
/// is shown as a word space.
const WORD_GAP: f64 = -200.0;
/// Bytes that end a token besides whitespace.
const DELIMITERS: &[u8] = b"()<>[]{}/%";

enum Operand {
    Text(Vec<u8>),
    Number(f64),
}

/// Keeps `operand` unless the operator already has [`MAX_OPERANDS`].
fn push_operand(operands: &mut Vec<Operand>, operand: Operand) {
    if operands.len() < MAX_OPERANDS {
        operands.push(operand);
    }
}

/// Runs the text operators of one content stream, appending to `out` until it holds
/// [`MAX_TEXT_BYTES`]. Returns false when a string cannot be decoded (see [`decode`]), which
/// makes the whole file unreadable.
pub(super) fn show_text(content: &[u8], out: &mut String) -> bool {
    let mut operands: Vec<Operand> = Vec::new();
    let mut at = 0;
    while at < content.len() && out.len() < MAX_TEXT_BYTES {
        match content[at] {
            b'(' => {
                let (bytes, next) = literal_string(content, at);
                push_operand(&mut operands, Operand::Text(bytes));
                at = next;
            }
            b'<' if content.get(at + 1) == Some(&b'<') => at += 2,
            b'<' => {
                let (bytes, next) = hex_string(content, at);
                push_operand(&mut operands, Operand::Text(bytes));
                at = next;
            }
            b'%' => {
                at = memchr2(b'\r', b'\n', &content[at..]).map_or(content.len(), |end| at + end);
            }
            b'/' => at = token_end(content, at + 1),
            b'>' | b'[' | b']' | b'{' | b'}' | b')' | 0 => at += 1,
            b if b.is_ascii_whitespace() => at += 1,
            _ => {
                let end = token_end(content, at);
                let token = &content[at..end];
                if let Some(value) = number(token) {
                    push_operand(&mut operands, Operand::Number(value));
                } else {
                    if !apply(token, &operands, out) {
                        return false;
                    }
                    operands.clear();
                }
                at = end.max(at + 1);
            }
        }
    }
    true
}

fn token_end(content: &[u8], from: usize) -> usize {
    from + content[from..]
        .iter()
        .position(|byte| byte.is_ascii_whitespace() || DELIMITERS.contains(byte))
        .unwrap_or(content.len() - from)
}

fn number(token: &[u8]) -> Option<f64> {
    token
        .first()
        .is_some_and(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.'))
        .then(|| std::str::from_utf8(token).ok()?.parse().ok())
        .flatten()
}

/// Runs `operator` on its `operands`. Returns false when a string cannot be decoded.
fn apply(operator: &[u8], operands: &[Operand], out: &mut String) -> bool {
    let last_text = || {
        operands.iter().rev().find_map(|op| match op {
            Operand::Text(bytes) => Some(bytes.as_slice()),
            Operand::Number(_) => None,
        })
    };
    match operator {
        b"Tj" => return last_text().is_none_or(|bytes| decode(bytes, out)),
        b"'" | b"\"" => {
            line_break(out);
            return last_text().is_none_or(|bytes| decode(bytes, out));
        }
        b"TJ" => {
            for operand in operands {
                match operand {
                    Operand::Text(bytes) if !decode(bytes, out) => return false,
                    Operand::Number(gap) if *gap < WORD_GAP => space(out),
                    _ => {}
                }
            }
        }
        b"Td" | b"TD" => match operands.last() {
            Some(Operand::Number(dy)) if *dy != 0.0 => line_break(out),
            _ => space(out),
        },
        b"T*" | b"Tm" | b"ET" => line_break(out),
        _ => {}
    }
    true
}

fn line_break(out: &mut String) {
    out.truncate(out.trim_end_matches(' ').len());
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn space(out: &mut String) {
    if !out.is_empty() && !out.ends_with(char::is_whitespace) {
        out.push(' ');
    }
}

/// The bytes of the `(…)` string opening at `start` and the offset just past it (or the end of
/// `content` when it is not closed). Balanced parentheses nest without escaping.
fn literal_string(content: &[u8], start: usize) -> (Vec<u8>, usize) {
    let mut bytes = Vec::new();
    let mut depth = 0_usize;
    let mut at = start;
    while let Some(&byte) = content.get(at) {
        at += 1;
        match byte {
            b'(' => {
                depth += 1;
                if depth > 1 {
                    bytes.push(byte);
                }
            }
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return (bytes, at);
                }
                bytes.push(byte);
            }
            b'\\' => bytes.extend(escape(content, &mut at)),
            _ => bytes.push(byte),
        }
    }
    (bytes, at)
}

/// Decodes the escape whose backslash ends just before `*at`, moving `at` past it. `None` for a
/// line continuation (a backslash before an end of line) and, without moving `at`, for a
/// backslash at the end of `content`.
fn escape(content: &[u8], at: &mut usize) -> Option<u8> {
    let &escaped = content.get(*at)?;
    *at += 1;
    match escaped {
        b'n' => Some(b'\n'),
        b'r' => Some(b'\r'),
        b't' => Some(b'\t'),
        b'b' => Some(0x08),
        b'f' => Some(0x0C),
        b'0'..=b'7' => {
            // At most three octal digits; high-order overflow is ignored, as the PDF spec says.
            let mut value = u32::from(escaped - b'0');
            for _ in 0..2 {
                let Some(&digit @ b'0'..=b'7') = content.get(*at) else {
                    break;
                };
                value = value * 8 + u32::from(digit - b'0');
                *at += 1;
            }
            Some((value & 0xFF) as u8)
        }
        b'\r' => {
            if content.get(*at) == Some(&b'\n') {
                *at += 1;
            }
            None
        }
        b'\n' => None,
        other => Some(other),
    }
}

/// The bytes of the `<…>` string opening at `start` and the offset just past it. Non-hex bytes
/// are skipped and an odd last digit is padded with zero.
fn hex_string(content: &[u8], start: usize) -> (Vec<u8>, usize) {
    let mut bytes = Vec::new();
    let mut high: Option<u8> = None;
    let mut at = start + 1;
    while let Some(&byte) = content.get(at) {
        at += 1;
        if byte == b'>' {
            break;
        }
        let Some(digit) = char::from(byte)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
        else {
            continue;
        };
        if let Some(h) = high.take() {
            bytes.push(h << 4 | digit);
        } else {
            high = Some(digit);
        }
    }
    if let Some(h) = high {
        bytes.push(h << 4);
    }
    (bytes, at)
}

/// Appends the text of one string: UTF-16BE after a BOM, WinAnsi otherwise. Returns false when
/// at least half the bytes are NUL: two-byte glyph ids without a BOM, which cannot be mapped to
/// text, so the whole file is given up.
fn decode(bytes: &[u8], out: &mut String) -> bool {
    if let Some(utf16) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units = utf16
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair));
        for ch in char::decode_utf16(units) {
            push_char(ch.unwrap_or(char::REPLACEMENT_CHARACTER), out);
        }
        return true;
    }
    if memchr_iter(0, bytes).count() * 2 >= bytes.len().max(1) {
        return false;
    }
    for &byte in bytes {
        if let Some(ch) = winansi(byte) {
            push_char(ch, out);
        }
    }
    true
}

/// Appends `character`, a tab as a word space; other control characters are dropped.
fn push_char(character: char, out: &mut String) {
    if character == '\t' {
        space(out);
    } else if !character.is_control() {
        out.push(character);
    }
}

/// A WinAnsi (Windows-1252) byte as Unicode: the typographic marks mapped here, nothing for the
/// other bytes in 0x81-0x9F, and Latin-1 for the rest.
fn winansi(byte: u8) -> Option<char> {
    match byte {
        0x80 => Some('€'),
        0x85 => Some('…'),
        0x91 => Some('‘'),
        0x92 => Some('’'),
        0x93 => Some('“'),
        0x94 => Some('”'),
        0x95 => Some('•'),
        0x96 => Some('–'),
        0x97 => Some('—'),
        0x99 => Some('™'),
        0x81..=0x9F => None,
        _ => Some(char::from(byte)),
    }
}
