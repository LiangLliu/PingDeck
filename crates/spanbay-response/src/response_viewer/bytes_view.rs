//! Byte-oriented body views: the Hex dump and the Base64 encoding.
//!
//! Single responsibility: render the first bytes of a captured body (as raw bytes, not through a
//! lossy text decode) the way the workbench's Hex and Base64 views show them.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};

use super::FormattedBodyDto;

/// Bytes shown by the Hex view.
pub(super) const MAX_HEX_BYTES: usize = 64 * 1024;
/// Bytes encoded by the Base64 view.
pub(super) const MAX_BASE64_BYTES: usize = 2 * 1024 * 1024;

/// Classic 16-byte-per-row hex + ASCII dump of the first [`MAX_HEX_BYTES`] bytes of `body`.
pub(super) fn hex_view(body: &[u8]) -> FormattedBodyDto {
    let limited = body.len() > MAX_HEX_BYTES;
    let shown = &body[..body.len().min(MAX_HEX_BYTES)];
    let mut text = String::with_capacity(shown.len() * 4 + 64);
    for (row, chunk) in shown.chunks(16).enumerate() {
        if row > 0 {
            text.push('\n');
        }
        text.push_str(&format!("{:08x}  ", row * 16));
        let hex: Vec<String> = chunk.iter().map(|byte| format!("{byte:02x}")).collect();
        text.push_str(&format!("{:<47}  |", hex.join(" ")));
        text.extend(chunk.iter().map(|byte| {
            if (32..127).contains(byte) {
                char::from(*byte)
            } else {
                '.'
            }
        }));
        text.push('|');
    }
    view(
        text,
        if limited {
            "Hex · first 64 KiB"
        } else {
            "Hex dump"
        },
        limited.then_some(MAX_HEX_BYTES),
    )
}

/// Base64 of the first [`MAX_BASE64_BYTES`] bytes of `body`.
pub(super) fn base64_view(body: &[u8]) -> FormattedBodyDto {
    let limited = body.len() > MAX_BASE64_BYTES;
    let shown = &body[..body.len().min(MAX_BASE64_BYTES)];
    view(
        BASE64_STANDARD.encode(shown),
        if limited {
            "Base64 · first 2 MiB"
        } else {
            "Base64 encoded"
        },
        limited.then_some(MAX_BASE64_BYTES),
    )
}

fn view(text: String, mode: &str, truncated_at_bytes: Option<usize>) -> FormattedBodyDto {
    FormattedBodyDto {
        text,
        mode: mode.to_owned(),
        is_valid_json: false,
        truncated_at_bytes,
        too_large: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_rows_have_offset_hex_and_ascii_columns() {
        let view = hex_view(b"Hello, SpanBay!\x00\xff");
        assert_eq!(
            view.text,
            "00000000  48 65 6c 6c 6f 2c 20 53 70 61 6e 42 61 79 21 00  |Hello, SpanBay!.|\n\
             00000010  ff                                               |.|"
        );
        assert_eq!(view.mode, "Hex dump");
        assert_eq!(view.truncated_at_bytes, None);
    }

    #[test]
    fn views_are_cut_at_their_limits_and_say_so() {
        let hex = hex_view(&vec![b'a'; MAX_HEX_BYTES + 1]);
        assert_eq!(hex.mode, "Hex · first 64 KiB");
        assert_eq!(hex.truncated_at_bytes, Some(MAX_HEX_BYTES));
        assert_eq!(hex.text.lines().count(), MAX_HEX_BYTES / 16);

        let base64 = base64_view(&vec![0_u8; MAX_BASE64_BYTES + 1]);
        assert_eq!(base64.mode, "Base64 · first 2 MiB");
        assert_eq!(base64.text.len(), MAX_BASE64_BYTES.div_ceil(3) * 4);
        assert_eq!(base64_view(b"abc").text, "YWJj");
    }
}
