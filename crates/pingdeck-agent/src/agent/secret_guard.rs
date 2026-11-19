//! Keeping secret values out of what the model controls.
//!
//! Single responsibility: recognise a resolved secret value in text the model wrote (a `fetch_url`
//! URL, a `web_search` query) and widen the redaction needles for model-bound results to the
//! forms a value takes on the wire. A value of [`MIN_SECRET_CHARS`] or more is found raw,
//! URL-encoded (form and percent style, either hex case), JSON-escaped and base64 (standard and
//! URL-safe, at any of the three byte alignments, so a value inside a longer base64 text is found
//! too). The tools call [`refusal_if_secret`] before any network use, in every permission mode;
//! `app::agent_service::run` passes the needles through [`expand_needles`] before redaction.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};

/// What the tool returns when its input carries a secret value.
pub const SECRET_REFUSAL: &str = "Blocked: the input contains a secret value.";

/// Shortest value treated as a secret: a shorter one would match ordinary text.
pub const MIN_SECRET_CHARS: usize = 4;

/// `%xx` escapes of `text` with their hex digits in upper case, so `%2f` and `%2F` compare equal.
fn upper_percent(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        let escape = bytes[index] == b'%'
            && bytes.get(index + 1).is_some_and(u8::is_ascii_hexdigit)
            && bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit);
        if escape {
            out.push('%');
            out.push(char::from(bytes[index + 1].to_ascii_uppercase()));
            out.push(char::from(bytes[index + 2].to_ascii_uppercase()));
            index += 3;
        } else {
            // `index` sits on a char boundary: escapes are ASCII and skipped whole.
            let ch = text[index..].chars().next().unwrap_or('\u{fffd}');
            out.push(ch);
            index += ch.len_utf8();
        }
    }
    out
}

/// The base64 text a value contributes to any longer base64 text it is embedded in: the encoding
/// at byte alignment `shift` (0 to 2), without the characters that also depend on the bytes next
/// to the value. Empty when the value is too short to leave a stable middle.
fn base64_core(value: &[u8], shift: usize, encode: impl Fn(&[u8]) -> String) -> String {
    let mut padded = vec![0_u8; shift];
    padded.extend_from_slice(value);
    let full = encode(&padded);
    // Characters holding bits of the `shift` bytes in front are skipped; so is the last, partly
    // filled one (it also carries bits of whatever follows).
    let skip = (shift * 8).div_ceil(6);
    let end = padded.len() * 8 / 6;
    full.get(skip..end).unwrap_or_default().to_owned()
}

/// Every form `secret` may take: raw, URL-encoded (form and percent), JSON-escaped, base64.
#[must_use]
pub fn variants(secret: &str) -> Vec<String> {
    if secret.chars().count() < MIN_SECRET_CHARS {
        return Vec::new();
    }
    let form: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
    let percent = form.replace('+', "%20");
    let json = serde_json::to_string(secret)
        .map(|json| json.trim_matches('"').to_owned())
        .unwrap_or_default();
    let mut forms = vec![secret.to_owned(), form, percent, json];
    for shift in 0..3 {
        forms.push(base64_core(secret.as_bytes(), shift, |bytes| {
            STANDARD_NO_PAD.encode(bytes)
        }));
        forms.push(base64_core(secret.as_bytes(), shift, |bytes| {
            URL_SAFE_NO_PAD.encode(bytes)
        }));
    }
    forms.retain(|form| form.chars().count() >= MIN_SECRET_CHARS);
    forms.sort();
    forms.dedup();
    forms
}

/// `needles` plus the base64 forms of each; `redact_secrets` adds the URL and JSON forms itself.
#[must_use]
pub fn expand_needles(needles: Vec<String>) -> Vec<String> {
    let mut all = Vec::with_capacity(needles.len() * 4);
    for needle in needles {
        for shift in 0..3 {
            for form in [
                base64_core(needle.as_bytes(), shift, |bytes| {
                    STANDARD_NO_PAD.encode(bytes)
                }),
                base64_core(needle.as_bytes(), shift, |bytes| {
                    URL_SAFE_NO_PAD.encode(bytes)
                }),
            ] {
                if needle.chars().count() >= MIN_SECRET_CHARS
                    && form.chars().count() >= MIN_SECRET_CHARS
                {
                    all.push(form);
                }
            }
        }
        all.push(needle);
    }
    all.sort();
    all.dedup();
    all
}

/// Whether `text` holds any of `needles` in any of the forms of [`variants`].
#[must_use]
pub fn contains_secret(text: &str, needles: &[String]) -> bool {
    let haystack = upper_percent(text);
    needles.iter().any(|needle| {
        variants(needle)
            .iter()
            .any(|form| haystack.contains(&upper_percent(form)))
    })
}

/// [`SECRET_REFUSAL`] when `text` carries a secret value, else `None`. Never echoes the value.
#[must_use]
pub fn refusal_if_secret(text: &str, needles: &[String]) -> Option<&'static str> {
    contains_secret(text, needles).then_some(SECRET_REFUSAL)
}

/// Shortest tail of an unfinished line a [`StreamRedactor`] holds back, in bytes.
const MIN_HOLD: usize = 1024;
/// How far an unfinished line may outgrow the hold-back window before a piece is released.
const MAX_HOLD_SLACK: usize = 8 * 1024;

/// Redacts a text that arrives in arbitrary chunks (a command's output): a secret split across two
/// chunks is still found, because text is released only up to the last line break (`\n` or `\r`)
/// and the unfinished line is held back until more arrives or [`StreamRedactor::finish`] ends the
/// stream.
///
/// shortcut: a line with no break that grows past the longest needle (x3 for its URL form, at
/// least 1 KiB) plus 8 KiB is released in pieces, so a pattern-based match (a very long header
/// value) that starts before a cut is masked only up to it.
#[derive(Debug, Default)]
pub struct StreamRedactor {
    carry: String,
}

impl StreamRedactor {
    /// Adds `text`; returns the redacted text that can be released now (possibly empty).
    pub fn push(&mut self, text: &str, needles: &[String]) -> String {
        self.carry.push_str(text);
        let hold = needles
            .iter()
            .map(|needle| needle.len() * 3)
            .max()
            .unwrap_or(0)
            .max(MIN_HOLD);
        let mut cut = self.carry.rfind(['\n', '\r']).map_or(0, |at| at + 1);
        if self.carry.len() - cut > hold + MAX_HOLD_SLACK {
            cut = self.carry.len() - hold;
            while !self.carry.is_char_boundary(cut) {
                cut -= 1;
            }
        }
        let released = pingdeck_base::secrets::redact_secrets(&self.carry[..cut], needles);
        self.carry.drain(..cut);
        released
    }

    /// Ends the stream: the held-back remainder, redacted.
    pub fn finish(&mut self, needles: &[String]) -> String {
        let rest = pingdeck_base::secrets::redact_secrets(&self.carry, needles);
        self.carry.clear();
        rest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn needles() -> Vec<String> {
        vec!["tok_Live/4+x9=Zq".to_owned(), "ab".to_owned()]
    }

    #[test]
    fn a_raw_url_encoded_json_escaped_and_base64_value_is_found() {
        let secret = "tok_Live/4+x9=Zq";
        let encoded: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
        for text in [
            format!("https://x.test/?k={secret}"),
            format!("https://x.test/?k={encoded}"),
            format!(
                "https://x.test/?k={}",
                secret
                    .replace('/', "%2f")
                    .replace('+', "%2b")
                    .replace('=', "%3d")
            ),
            format!("Basic {}", STANDARD_NO_PAD.encode(secret)),
            format!("Basic {}", STANDARD_NO_PAD.encode(format!("user:{secret}"))),
            format!("Basic {}", STANDARD_NO_PAD.encode(format!("us:{secret}"))),
            format!("Basic {}", STANDARD_NO_PAD.encode(format!("u:{secret}!"))),
            format!("Basic {}", URL_SAFE_NO_PAD.encode(format!("{secret}:pw"))),
        ] {
            assert!(contains_secret(&text, &needles()), "{text}");
        }
        let quoted = vec!["pa\"ss\\word".to_owned()];
        assert!(contains_secret(r#"{"p":"pa\"ss\\word"}"#, &quoted));
    }

    #[test]
    fn ordinary_text_and_short_values_are_not_secrets() {
        assert!(!contains_secret("https://x.test/?q=weather", &needles()));
        assert!(!contains_secret("a b ab abab", &needles()));
        assert_eq!(refusal_if_secret("hello", &needles()), None);
    }

    #[test]
    fn the_refusal_never_echoes_the_value() {
        let text = format!("https://x.test/?k={}", needles()[0]);
        let refusal = refusal_if_secret(&text, &needles());
        assert_eq!(refusal, Some(SECRET_REFUSAL));
        assert!(!SECRET_REFUSAL.contains("tok_"));
    }

    #[test]
    fn expanded_needles_redact_base64_forms_too() {
        let secret = "tok_Live/4+x9=Zq";
        let expanded = expand_needles(vec![secret.to_owned()]);
        let text = format!("auth {}", STANDARD_NO_PAD.encode(format!("user:{secret}")));
        let masked = pingdeck_base::secrets::redact_secrets(&text, &expanded);
        assert!(!masked.contains(&STANDARD_NO_PAD.encode(secret)));
        assert!(
            masked.contains(pingdeck_base::secrets::REDACTED),
            "{masked}"
        );
    }

    /// A needle split over two chunks, at every possible cut, is redacted like the whole text.
    #[test]
    fn a_stream_redactor_matches_a_secret_split_across_chunks() {
        let needles = expand_needles(vec!["tok_Live/4+x9=Zq".to_owned()]);
        let text =
            "start\nusing tok_Live/4+x9=Zq here\r\nprogress tok_Live/4+x9=Zq\nend tok_Live/4+x9=Zq";
        let whole = pingdeck_base::secrets::redact_secrets(text, &needles);
        for cut in (0..text.len()).filter(|cut| text.is_char_boundary(*cut)) {
            let mut stream = StreamRedactor::default();
            let mut out = stream.push(&text[..cut], &needles);
            out.push_str(&stream.push(&text[cut..], &needles));
            out.push_str(&stream.finish(&needles));
            assert_eq!(out, whole, "cut at {cut}");
            assert!(!out.contains("tok_Live"), "cut at {cut}: {out}");
        }
    }

    #[test]
    fn a_stream_redactor_holds_back_only_a_bounded_unfinished_line() {
        let needles = vec!["tok_Live/4+x9=Zq".to_owned()];
        let mut stream = StreamRedactor::default();
        assert_eq!(stream.push("done line\npartial", &needles), "done line\n");
        assert_eq!(stream.finish(&needles), "partial");
        // A line without any break is released in pieces once it outgrows the hold-back window,
        // keeping the tail that could still be the start of a secret.
        let long = "x".repeat(MIN_HOLD + MAX_HOLD_SLACK + 100);
        let released = stream.push(&long, &needles);
        assert!(!released.is_empty() && released.len() < long.len());
        assert_eq!(released.len() + stream.finish(&needles).len(), long.len());
        // Multi-byte characters are never cut.
        let wide = "é".repeat(MIN_HOLD + MAX_HOLD_SLACK);
        let mut stream = StreamRedactor::default();
        let part = stream.push(&wide, &needles);
        assert_eq!(part.len() + stream.finish(&needles).len(), wide.len());
    }
}
