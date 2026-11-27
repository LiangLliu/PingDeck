//! Content sniffing: what an attached file really is, decided by its bytes.
//!
//! Magic-byte detection (PNG, JPEG, GIF, WebP, PDF), image dimensions read straight from the
//! headers (no decoder crate), and the text decision with a media type picked from the file
//! extension. The UI's claimed type never decides Image or Pdf. Everything else (executables,
//! HEIC, TIFF, BMP, SVG) is refused: there is no "binary" attachment.

use spanbay_model::agent::AttachmentKind;

/// What [`sniff`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sniffed {
    /// MIME type, e.g. `image/png` or `text/x-rust`.
    pub media_type: &'static str,
    pub kind: AttachmentKind,
    /// Pixel width of an image whose header parsed.
    pub width: Option<u32>,
    /// Pixel height of an image whose header parsed.
    pub height: Option<u32>,
}

/// Extension -> text media type; any other text file is `text/plain`.
const TEXT_TYPES: &[(&str, &str)] = &[
    ("md", "text/markdown"),
    ("markdown", "text/markdown"),
    ("txt", "text/plain"),
    ("log", "text/plain"),
    ("ini", "text/plain"),
    ("csv", "text/csv"),
    ("tsv", "text/tab-separated-values"),
    ("json", "application/json"),
    ("yaml", "application/yaml"),
    ("yml", "application/yaml"),
    ("toml", "application/toml"),
    ("xml", "application/xml"),
    ("cs", "text/x-csharp"),
    ("rb", "text/x-ruby"),
    ("php", "text/x-php"),
    ("swift", "text/x-swift"),
    ("kt", "text/x-kotlin"),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("css", "text/css"),
    ("js", "text/javascript"),
    ("mjs", "text/javascript"),
    ("cjs", "text/javascript"),
    ("ts", "text/typescript"),
    ("tsx", "text/tsx"),
    ("jsx", "text/jsx"),
    ("rs", "text/x-rust"),
    ("py", "text/x-python"),
    ("go", "text/x-go"),
    ("java", "text/x-java"),
    ("c", "text/x-c"),
    ("h", "text/x-c"),
    ("cpp", "text/x-c++"),
    ("cc", "text/x-c++"),
    ("hpp", "text/x-c++"),
    ("sh", "text/x-shellscript"),
    ("sql", "application/sql"),
];

const DEFAULT_TEXT: &str = "text/plain";

/// Decides what `bytes` are; `None` for anything that is not a PNG, JPEG, GIF, WebP, PDF or UTF-8
/// text without a NUL byte, and for `.svg` files (markup a decoder would have to run). `file_name`
/// picks the text subtype by extension; `hint` (the UI's claimed type) only breaks the tie when the
/// extension says nothing, and only among the text types this module knows.
#[must_use]
pub fn sniff(bytes: &[u8], file_name: &str, hint: &str) -> Option<Sniffed> {
    let image = |media_type, (width, height): (Option<u32>, Option<u32>)| {
        Some(Sniffed {
            media_type,
            kind: AttachmentKind::Image,
            width,
            height,
        })
    };
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return image("image/png", png_size(bytes));
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return image("image/jpeg", jpeg_size(bytes));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return image("image/gif", gif_size(bytes));
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return image("image/webp", webp_size(bytes));
    }
    let plain = |kind, media_type| {
        Some(Sniffed {
            media_type,
            kind,
            width: None,
            height: None,
        })
    };
    if bytes.starts_with(b"%PDF-") {
        return plain(AttachmentKind::Pdf, "application/pdf");
    }
    if is_text(bytes) && !extension(file_name).is_some_and(|ext| ext.eq_ignore_ascii_case("svg")) {
        return plain(AttachmentKind::Text, text_type(file_name, hint));
    }
    None
}

/// Valid UTF-8 with no NUL byte. A NUL anywhere means binary even when the rest decodes.
fn is_text(bytes: &[u8]) -> bool {
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

/// What follows the last `.` of `file_name`, if it has one.
fn extension(file_name: &str) -> Option<&str> {
    file_name.rsplit_once('.').map(|(_, extension)| extension)
}

fn text_type(file_name: &str, hint: &str) -> &'static str {
    if let Some(extension) = extension(file_name)
        && let Some((_, media_type)) = TEXT_TYPES
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(extension))
    {
        return media_type;
    }
    // The hint may name a parameterised type (`text/plain;charset=utf-8`); compare the essence.
    let hint = hint.split(';').next().unwrap_or_default().trim();
    TEXT_TYPES
        .iter()
        .map(|(_, media_type)| *media_type)
        .find(|media_type| media_type.eq_ignore_ascii_case(hint))
        .unwrap_or(DEFAULT_TEXT)
}

// Header field readers: the value at `at`, widened to `u32`; `None` when it runs past the end.

fn be16(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes(
        *bytes.get(at..)?.first_chunk()?,
    )))
}

fn le16(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes(
        *bytes.get(at..)?.first_chunk()?,
    )))
}

fn le24(bytes: &[u8], at: usize) -> Option<u32> {
    let [a, b, c] = *bytes.get(at..)?.first_chunk()?;
    Some(u32::from_le_bytes([a, b, c, 0]))
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(*bytes.get(at..)?.first_chunk()?))
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*bytes.get(at..)?.first_chunk()?))
}

/// Both dimensions, or neither: a zero dimension is a corrupt header, not an empty image.
fn nonzero_dimensions(width: Option<u32>, height: Option<u32>) -> (Option<u32>, Option<u32>) {
    match (width, height) {
        (Some(width), Some(height)) if width > 0 && height > 0 => (Some(width), Some(height)),
        _ => (None, None),
    }
}

/// PNG: the IHDR chunk comes first, width and height are big-endian `u32`.
fn png_size(bytes: &[u8]) -> (Option<u32>, Option<u32>) {
    if bytes.get(12..16) != Some(b"IHDR") {
        return (None, None);
    }
    nonzero_dimensions(be32(bytes, 16), be32(bytes, 20))
}

/// JPEG: walk the marker segments up to the first start-of-frame (SOF0-15 except DHT, JPG, DAC).
fn jpeg_size(bytes: &[u8]) -> (Option<u32>, Option<u32>) {
    let mut at = 2;
    while at + 4 <= bytes.len() {
        if bytes[at] != 0xFF {
            return (None, None);
        }
        let marker = bytes[at + 1];
        match marker {
            0xFF => at += 1, // fill byte
            // TEM, RST0-7 and SOI stand alone, with no length.
            0x01 | 0xD0..=0xD8 => at += 2,
            // EOI or SOS before any start-of-frame: no dimensions to find.
            0xD9 | 0xDA => return (None, None),
            _ => {
                let Some(length) = be16(bytes, at + 2) else {
                    return (None, None);
                };
                if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                    return nonzero_dimensions(be16(bytes, at + 7), be16(bytes, at + 5));
                }
                at += 2 + length as usize;
            }
        }
    }
    (None, None)
}

/// GIF: the logical screen descriptor, little-endian `u16` pair after the signature.
fn gif_size(bytes: &[u8]) -> (Option<u32>, Option<u32>) {
    nonzero_dimensions(le16(bytes, 6), le16(bytes, 8))
}

/// WebP: the first chunk is lossy `VP8 `, lossless `VP8L` or extended `VP8X`.
fn webp_size(bytes: &[u8]) -> (Option<u32>, Option<u32>) {
    match bytes.get(12..16) {
        // Frame tag (3 bytes), start code 9D 01 2A, then 14-bit width and height.
        Some(b"VP8 ") if bytes.get(23..26) == Some(&[0x9D, 0x01, 0x2A]) => nonzero_dimensions(
            le16(bytes, 26).map(|width| width & 0x3FFF),
            le16(bytes, 28).map(|height| height & 0x3FFF),
        ),
        // Signature 0x2F, then 14-bit (width-1) and 14-bit (height-1) in one little-endian word.
        Some(b"VP8L") if bytes.get(20) == Some(&0x2F) => {
            let Some(word) = le32(bytes, 21) else {
                return (None, None);
            };
            nonzero_dimensions(Some((word & 0x3FFF) + 1), Some(((word >> 14) & 0x3FFF) + 1))
        }
        // Canvas size: 24-bit (width-1) and (height-1) after the flags.
        Some(b"VP8X") => nonzero_dimensions(
            le24(bytes, 24).map(|v| v + 1),
            le24(bytes, 27).map(|v| v + 1),
        ),
        _ => (None, None),
    }
}
