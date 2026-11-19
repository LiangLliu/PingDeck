//! Text from outside the app (web pages, search hits, MCP results, response bodies, sub-agent
//! reports) on its way into a model prompt.
//!
//! Single responsibility: make hidden characters visible and mark where external data starts and
//! ends. [`sanitize_untrusted`] removes control characters and writes format characters
//! (bidirectional controls, zero-width, tag characters) as `⟦U+XXXX⟧`, so an instruction hidden
//! from a reader is not hidden from the model either. [`envelope_tags`] makes the
//! `<external_data>` tags that wrap such text; their random id is one a page cannot guess, so it
//! cannot close the block early.
//! The character class is the one the approval card uses (`chat/approvals/Visible.tsx`);
//! `visibleParity.test.ts` compares the two sources.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;

use pingdeck_base::crypto::hex_lower;
use pingdeck_base::id::random_bytes;

/// The characters treated as hidden, except [`KEPT`]: `\p{Cc}` is removed, the rest is written as
/// `⟦U+XXXX⟧`. Same class as `HIDDEN` in `src/features/chat/approvals/Visible.tsx`.
pub const HIDDEN_CLASS: &str = r"[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]";

/// Characters of [`HIDDEN_CLASS`] that stay as they are.
pub const KEPT: [char; 2] = ['\n', '\t'];

#[allow(clippy::expect_used)] // a constant pattern; the parity test compiles it
static HIDDEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"[{HIDDEN_CLASS}--[\n\t]]")).expect("HIDDEN_CLASS is a valid class")
});

/// `text` with control characters other than `\n` and `\t` removed and format, line and
/// paragraph separator characters written as `⟦U+XXXX⟧`. Borrows when nothing changes.
#[must_use]
pub fn sanitize_untrusted(text: &str) -> Cow<'_, str> {
    HIDDEN.replace_all(text, |caps: &regex::Captures<'_>| {
        let c = caps[0].chars().next().unwrap_or_default();
        if c.is_control() {
            String::new()
        } else {
            format!("⟦U+{:04X}⟧", u32::from(c))
        }
    })
}

/// The opening and closing tags of an envelope for data from `source`, with a fresh 8-digit id.
#[must_use]
pub fn envelope_tags(source: &str) -> (String, String) {
    let id = hex_lower(&random_bytes::<4>());
    let source: String = source
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
        .collect();
    (
        format!("<external_data source=\"{source}\" id=\"{id}\">"),
        format!("</external_data id=\"{id}\">"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_characters_are_removed_or_made_visible() {
        assert!(matches!(
            sanitize_untrusted("plain\ttext\n"),
            Cow::Borrowed(_)
        ));
        let cases = [
            ("a\u{0}b\u{1b}[31mc\r\n", "ab[31mc\n"),
            ("pay\u{202e}lixe", "pay⟦U+202E⟧lixe"),
            ("x\u{200b}y\u{feff}", "x⟦U+200B⟧y⟦U+FEFF⟧"),
            ("tag\u{e0041}\u{e007f}", "tag⟦U+E0041⟧⟦U+E007F⟧"),
            ("line\u{2028}para\u{2029}", "line⟦U+2028⟧para⟦U+2029⟧"),
            ("é 中文 🙂", "é 中文 🙂"),
        ];
        for (input, expected) in cases {
            assert_eq!(sanitize_untrusted(input), expected, "{input:?}");
        }
    }
}
