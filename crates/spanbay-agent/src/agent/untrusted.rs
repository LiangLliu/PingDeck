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

use spanbay_base::crypto::hex_lower;
use spanbay_base::id::random_bytes;

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

/// Parses an opening `<external_data source="..." id="<8-hex>">` tag line and returns its id.
fn parse_opening_envelope_tag(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("<external_data source=\"")?;
    let (source, rest) = rest.split_once("\" id=\"")?;
    if !source
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
    {
        return None;
    }
    let id = rest.strip_suffix("\">")?;
    if id.len() == 8 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Some(id)
    } else {
        None
    }
}

/// Strips the internal `<external_data source="..." id="...">` prompt-injection boundary tags
/// added by [`envelope_tags`] for human-facing UI display while preserving all payload text.
///
/// Handles complete envelopes, truncated prefixes (opening tag without closing tag), and
/// envelopes flanked by a leading `User note:` or trailing progress nudge. Never alters arbitrary
/// XML that does not match the exact 8-hex-digit envelope format.
#[must_use]
pub fn strip_external_envelope(text: &str) -> (Cow<'_, str>, bool) {
    let trimmed = text.trim_matches(['\r', '\n']);
    let first_line = trimmed.split('\n').next().unwrap_or("");
    if let Some(id) = parse_opening_envelope_tag(first_line.trim_end_matches('\r')) {
        let after_open = trimmed
            .get(first_line.len()..)
            .unwrap_or("")
            .strip_prefix('\n')
            .unwrap_or("");
        let close_tag = format!("</external_data id=\"{id}\">");
        if after_open == close_tag {
            return (Cow::Borrowed(""), true);
        }
        if let Some(inner) = after_open.strip_suffix(&close_tag) {
            let inner = inner.strip_suffix('\n').unwrap_or(inner);
            let inner = inner.strip_suffix('\r').unwrap_or(inner);
            return (Cow::Borrowed(inner), true);
        }
        let close_line = format!("\n{close_tag}");
        if let Some(pos) = after_open.find(&close_line) {
            let before = &after_open[..pos];
            let after = after_open[pos + close_line.len()..].trim_start_matches(['\r', '\n']);
            let joined = if after.is_empty() {
                before.to_owned()
            } else if before.is_empty() {
                after.to_owned()
            } else {
                format!("{before}\n{after}")
            };
            return (Cow::Owned(joined), true);
        }
        return (Cow::Borrowed(after_open), true);
    }

    // Secondary path: a leading `User note: ...` line before an enveloped body.
    if let Some(open_pos) = trimmed.find("\n<external_data source=\"") {
        let prefix = &trimmed[..open_pos];
        let tail = &trimmed[open_pos + 1..];
        let (stripped, was_enveloped) = strip_external_envelope(tail);
        if was_enveloped {
            let joined = if stripped.is_empty() {
                prefix.to_owned()
            } else {
                format!("{prefix}\n{stripped}")
            };
            return (Cow::Owned(joined), true);
        }
    }

    (Cow::Borrowed(text), false)
}

/// Strips a machine-readable `{"error":{"code":"..."}}` first line produced by
/// `spanbay_model::agent::coded_error_text`, returning `(Some(code), remaining_message)`.
#[must_use]
pub fn strip_coded_error_prefix(text: &str) -> (Option<&str>, &str) {
    let Some((first, rest)) = text.split_once('\n') else {
        return (None, text);
    };
    let line = first.trim_end_matches('\r');
    let Some(code) = line
        .strip_prefix("{\"error\":{\"code\":\"")
        .and_then(|s| s.strip_suffix("\"}}"))
    else {
        return (None, text);
    };
    if !code.is_empty()
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        (Some(code), rest)
    } else {
        (None, text)
    }
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

    #[test]
    fn strip_external_envelope_unwraps_generated_tags_without_touching_plain_xml() {
        let (open, close) = envelope_tags("read_file");
        let wrapped = format!("{open}\n     1\t[package]\n     2\tname = \"demo\"\n{close}");
        let (clean, isolated) = strip_external_envelope(&wrapped);
        assert!(isolated);
        assert_eq!(clean, "     1\t[package]\n     2\tname = \"demo\"");

        // Truncated output (opening tag only, e.g. 32 KiB spill page) still strips cleanly.
        let truncated = format!("{open}\n     1\tline one\n     2\tline two");
        let (clean_trunc, isolated_trunc) = strip_external_envelope(&truncated);
        assert!(isolated_trunc);
        assert_eq!(clean_trunc, "     1\tline one\n     2\tline two");

        // Envelope flanked by user note and trailing nudge.
        let flanked = format!("User note: check line 1\n{open}\nhello\n{close}\nNudge text");
        let (clean_flanked, isolated_flanked) = strip_external_envelope(&flanked);
        assert!(isolated_flanked);
        assert_eq!(clean_flanked, "User note: check line 1\nhello\nNudge text");

        // Arbitrary XML is never stripped.
        let user_xml = "<external_data>ordinary xml</external_data>";
        let (untouched, was_iso) = strip_external_envelope(user_xml);
        assert!(!was_iso);
        assert_eq!(untouched, user_xml);
    }

    #[test]
    fn strip_coded_error_prefix_extracts_machine_code() {
        let msg = "{\"error\":{\"code\":\"outside_folder\"}}\npath is outside the workspace";
        assert_eq!(
            strip_coded_error_prefix(msg),
            (Some("outside_folder"), "path is outside the workspace")
        );
        let normal = "{\"status\":200}\nbody";
        assert_eq!(strip_coded_error_prefix(normal), (None, normal));
    }
}
