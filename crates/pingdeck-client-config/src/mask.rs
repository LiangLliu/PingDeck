//! Masking of gateway tokens in previews: prefix, bullets and the last four characters
//! (`pd_cc_••••9d2f`, `pdk_c_••••9d2f`, legacy `pdk_••••9d2f`).

use std::{borrow::Cow, ops::RangeInclusive, sync::LazyLock};

use regex::Regex;

use pingdeck_base::secrets::{REDACTED, redact_secrets};

/// The token written for a gateway that needs none; it is not a secret, so previews show it.
pub(super) const OPEN_GATEWAY_TOKEN: &str = "pingdeck";

/// Random part of the stand-in token of a client that does not exist yet ([`pending_token`]).
const PENDING_BODY: &str = "PENDINGPENDINGPENDINGPENDINGXXXX";
/// What a preview shows for a token that is not gateway-shaped.
const GENERIC_MASK: &str = "<gateway token>";
/// Fewest random characters a token must carry to be masked as a token of ours.
const MIN_BODY: usize = 8;
/// Fewest random characters of a stray gateway-shaped token found inside a file.
const MIN_STRAY_BODY: usize = 16;
/// Length of the client tag in `pd_<tag>_`.
const TAG_LEN: RangeInclusive<usize> = 2..=4;

/// The stand-in token for a preview made before the client exists: it masks as
/// `pd_<tag>_••••••••`. `tag` is the client's 2-4 character tag (`cc` for Claude Code); anything
/// else becomes `xx`. It is for previews only and must never be applied.
#[must_use]
pub fn pending_token(tag: &str) -> String {
    format!("pd_{}_{PENDING_BODY}", if is_tag(tag) { tag } else { "xx" })
}

/// Whether `tag` is a client tag: 2-4 characters of `[a-z0-9]`.
fn is_tag(tag: &str) -> bool {
    TAG_LEN.contains(&tag.len())
        && tag
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// Length of the visible prefix (`pd_cc_`, `pdk_c_`, `pdk_`) that starts `text`, when there is
/// one.
///
/// Shapes: `pd_<tag>_` with a 2-4 character `[a-z0-9]` tag, and the legacy `pdk_c_` and `pdk_`.
fn prefix_len(text: &str) -> Option<usize> {
    if text.starts_with("pdk_c_") {
        return Some(6);
    }
    if text.starts_with("pdk_") {
        return Some(4);
    }
    let rest = text.strip_prefix("pd_")?;
    let tag = rest
        .bytes()
        .take_while(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        .count();
    // The tag ends at the first byte outside `[a-z0-9]`, which must be the `_`.
    (TAG_LEN.contains(&tag) && rest[tag..].starts_with('_')).then_some(3 + tag + 1)
}

/// Splits a gateway token into its visible prefix and its random body.
fn split_token(token: &str) -> Option<(&str, &str)> {
    prefix_len(token).map(|n| token.split_at(n))
}

/// What a preview shows in place of a gateway token: prefix, bullets and the last four
/// characters (`pd_cc_••••9d2f`), all bullets for a [`pending_token`], or `<gateway token>` for a
/// token of any other shape.
pub(super) fn mask_token(token: &str) -> String {
    match split_token(token) {
        Some((prefix, body)) if body == PENDING_BODY => format!("{prefix}••••••••"),
        Some((prefix, body)) if body.len() >= MIN_BODY && body.is_ascii() => {
            format!("{prefix}••••{}", &body[body.len() - 4..])
        }
        _ => GENERIC_MASK.to_owned(),
    }
}

/// Length of the gateway-shaped token that starts `tail`, when there is one.
fn stray_token_len(tail: &str) -> Option<usize> {
    let prefix_len = prefix_len(tail)?;
    let body = tail[prefix_len..]
        .bytes()
        .take_while(u8::is_ascii_alphanumeric)
        .count();
    (body >= MIN_STRAY_BODY).then_some(prefix_len + body)
}

/// Byte ranges of the gateway-shaped tokens (`pd_<tag>_` or `pdk_` plus 16 or more letters and
/// digits) in `text`. A candidate that follows a word character (`xpd_...`) is not a token.
fn token_spans(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(rel) = text[from..].find("pd") {
        let at = from + rel;
        let boundary = text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        match boundary.then(|| stray_token_len(&text[at..])).flatten() {
            Some(len) => {
                spans.push(at..at + len);
                from = at + len;
            }
            None => from = at + 2,
        }
    }
    spans
}

/// The gateway-shaped tokens found in `text`, such as the ones a backup still holds.
pub(super) fn gateway_tokens(text: &str) -> Vec<&str> {
    token_spans(text)
        .into_iter()
        .map(|span| &text[span])
        .collect()
}

/// A credential key (`*_API_KEY`, `*_TOKEN`, `apiKey`, `authorization`, `experimental_bearer_token`,
/// `secret`, `password`, in JSON, TOML or `.env` form) and its value: group 1 is the key and the
/// separator, group 2 the value, quoted or bare. A key that merely contains a word of the list
/// (`max_tokens`) is not one.
///
/// `Option` because `expect_used` is denied: the pattern is a constant, and the masking tests
/// fail if it ever stops compiling (masking would silently skip credential keys).
static CREDENTIAL_VALUE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(["']?[a-z0-9_.-]*(?:api_?key|token|secret|password|authorization)["']?[ \t]*[:=][ \t]*)("(?:[^"\\\r\n]|\\.)*"|'[^'\r\n]*'|[^\s,}\]"'{\[]+)"#,
    )
    .ok()
});

/// `text` with the value of every credential key replaced by [`REDACTED`], keeping its quotes.
/// Empty values, the open gateway token and values that are already masked stay.
fn mask_credential_values(text: &str) -> Cow<'_, str> {
    let Some(pattern) = CREDENTIAL_VALUE.as_ref() else {
        return Cow::Borrowed(text);
    };
    pattern.replace_all(text, |found: &regex::Captures<'_>| {
        let value = &found[2];
        let quote = value.chars().next().filter(|c| matches!(c, '"' | '\''));
        let inner = quote.map_or(value, |_| &value[1..value.len() - 1]);
        if inner.is_empty() || inner == OPEN_GATEWAY_TOKEN || inner.contains(REDACTED) {
            return found[0].to_owned();
        }
        match quote {
            Some(q) => format!("{}{q}{REDACTED}{q}", &found[1]),
            None => format!("{}{REDACTED}", &found[1]),
        }
    })
}

/// One stretch of text between our own tokens: credential keys and the patterns of
/// [`redact_secrets`] (provider keys, `Bearer` values, JWTs, URL secrets) are masked. In front of
/// a token (`before_token`), a mask that swallowed the empty value ahead of it is dropped again.
fn scrub(piece: &str, before_token: bool) -> String {
    let mut out = redact_secrets(&mask_credential_values(piece), &[]);
    if before_token && !piece.ends_with(REDACTED) && out.ends_with(REDACTED) {
        // `REDACTED` is a whole UTF-8 suffix, so the cut lands on a char boundary.
        out.truncate(out.len() - REDACTED.len());
    }
    out
}

/// `text` for the screen: `token` and every other gateway-shaped token (such as the one an
/// earlier apply left in the file) become their mask, and every other secret the file holds (the
/// user's own provider keys, see [`scrub`]) becomes [`REDACTED`]. The open gateway token is
/// shown as it is.
pub(super) fn mask_secrets(text: &str, token: &str) -> String {
    let mut spans = token_spans(text);
    if !token.is_empty() {
        spans.extend(
            text.match_indices(token)
                .map(|(at, found)| at..at + found.len()),
        );
    }
    spans.sort_by_key(|span| (span.start, std::cmp::Reverse(span.end)));
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for span in spans {
        if span.start < last {
            continue;
        }
        out.push_str(&scrub(&text[last..span.start], true));
        let found = &text[span.clone()];
        if found == OPEN_GATEWAY_TOKEN {
            out.push_str(found);
        } else {
            out.push_str(&mask_token(found));
        }
        last = span.end;
    }
    out.push_str(&scrub(&text[last..], false));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY: &str = "pdk_00112233445566778899aabbccddeeff00112233445566ff";
    const CURRENT: &str = "pd_cc_a1b2c3d4e5f60718293a4b5c6d7e9d2f";
    const CURRENT_SHOWN: &str = "pd_cc_••••9d2f";

    #[test]
    fn the_current_shape_masks_to_prefix_bullets_and_last_four() {
        assert_eq!(mask_token(CURRENT), "pd_cc_••••9d2f");
        assert_eq!(mask_token("pd_zd1_abcdefgh12345678"), "pd_zd1_••••5678");
    }

    #[test]
    fn the_legacy_shapes_use_the_same_bullet_form() {
        assert_eq!(mask_token(LEGACY), "pdk_••••66ff");
        assert_eq!(
            mask_token("pdk_c_0123456789abcdef0123456789abcdef0123456789abcdef"),
            "pdk_c_••••cdef"
        );
    }

    #[test]
    fn a_token_not_created_yet_masks_to_all_bullets() {
        assert_eq!(mask_token(&pending_token("cc")), "pd_cc_••••••••");
        assert_eq!(mask_token(&pending_token("zd")), "pd_zd_••••••••");
        assert_eq!(mask_token(&pending_token("Bad Tag")), "pd_xx_••••••••");
        assert_eq!(
            mask_secrets(&format!("k={}", pending_token("cc")), ""),
            "k=pd_cc_••••••••"
        );
    }

    #[test]
    fn odd_tokens_fall_back_to_the_generic_mask() {
        for odd in [
            "pd-token",
            "pdk_short",
            "pd_c_abcdefgh12345678",
            "pd_toolong_abcdefgh12345678",
            "pd_CC_abcdefgh12345678",
            "pd_cc_short",
            "",
        ] {
            assert_eq!(mask_token(odd), "<gateway token>", "{odd}");
        }
    }

    #[test]
    fn the_given_token_and_stray_tokens_of_both_shapes_are_masked() {
        let text = format!("a={CURRENT} b={LEGACY} c=pd_cx_{}", "Z".repeat(20));
        assert_eq!(
            mask_secrets(&text, CURRENT),
            "a=pd_cc_••••9d2f b=pdk_••••66ff c=pd_cx_••••ZZZZ"
        );
        assert_eq!(
            mask_secrets("a pd-token b", "pd-token"),
            "a <gateway token> b"
        );
    }

    #[test]
    fn a_stray_pdk_c_token_is_found_and_masked() {
        let token = "pdk_c_0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(gateway_tokens(&format!("k={token};")), [token]);
        assert_eq!(
            mask_secrets(&format!("k={token};"), ""),
            "k=pdk_c_••••cdef;"
        );
    }

    #[test]
    fn short_look_alikes_and_embedded_words_are_left_alone() {
        let embedded = format!("xpd_cc_{}", "A".repeat(20));
        for same in [
            "pdk_x",
            "pd_cc_short",
            "spend pd and pdf",
            embedded.as_str(),
        ] {
            assert_eq!(mask_secrets(same, "other"), same);
        }
    }

    #[test]
    fn a_token_at_the_start_or_after_punctuation_is_masked() {
        assert_eq!(mask_secrets(CURRENT, ""), "pd_cc_••••9d2f");
        assert_eq!(
            mask_secrets(&format!("\"{CURRENT}\""), ""),
            "\"pd_cc_••••9d2f\""
        );
    }

    #[test]
    fn gateway_tokens_lists_every_token_shape_once() {
        let text = format!(
            "a {CURRENT} b {LEGACY} c xpd_cc_{} d pd_c_{}",
            "A".repeat(20),
            "B".repeat(20)
        );
        assert_eq!(gateway_tokens(&text), [CURRENT, LEGACY]);
        assert!(gateway_tokens("nothing here").is_empty());
    }

    #[test]
    fn multi_byte_text_ahead_of_a_token_keeps_the_boundary_rule() {
        // A separator ahead of the token is a boundary, whatever precedes it.
        assert_eq!(
            mask_secrets(&format!("键={CURRENT}"), ""),
            "键=pd_cc_••••9d2f"
        );
        assert_eq!(
            mask_secrets(&format!("键 {CURRENT}"), ""),
            "键 pd_cc_••••9d2f"
        );
        // A letter of any script glued to it is not: the stray scan leaves it, the given token
        // is still masked.
        let glued = format!("中{CURRENT}");
        assert_eq!(gateway_tokens(&glued), Vec::<&str>::new());
        assert_eq!(mask_secrets(&glued, CURRENT), "中pd_cc_••••9d2f");
        // A credential key with a multi-byte name ahead of the token: the key mask is dropped
        // again in front of the token.
        assert_eq!(
            mask_secrets(&format!("密钥_token={CURRENT}"), ""),
            "密钥_token=pd_cc_••••9d2f"
        );
    }

    #[test]
    fn an_empty_token_never_matches_everywhere() {
        assert_eq!(mask_secrets("abc", ""), "abc");
    }

    #[test]
    fn credential_values_are_masked_whatever_their_shape_and_quoting() {
        let masked = |text: &str| mask_secrets(text, "");
        assert_eq!(
            masked("ANTHROPIC_AUTH_TOKEN=opaque123"),
            format!("ANTHROPIC_AUTH_TOKEN={REDACTED}")
        );
        assert_eq!(
            masked("api_key = 'opaque123'"),
            format!("api_key = '{REDACTED}'")
        );
        assert_eq!(
            masked("{\"apiKey\": \"opaque123\", \"x\": 1}"),
            format!("{{\"apiKey\": \"{REDACTED}\", \"x\": 1}}")
        );
        assert_eq!(
            masked("experimental_bearer_token = \"opaque123\""),
            format!("experimental_bearer_token = \"{REDACTED}\"")
        );
    }

    #[test]
    fn only_credential_keys_are_masked_and_masking_is_idempotent() {
        for same in [
            "{\"max_tokens\": 4096}",
            "max_output_tokens = 4096",
            "KEY=\nNEXT=1",
            "{\"apiKey\": \"\"}",
            "{\"authorization\": {\"x\": 1}}",
        ] {
            assert_eq!(mask_secrets(same, ""), same);
        }
        let once = mask_secrets("A_TOKEN=opaque123 B=1", "");
        assert_eq!(mask_secrets(&once, ""), once);
    }

    #[test]
    fn the_open_gateway_token_stays_visible_next_to_other_secrets() {
        let text = "{\"apiKey\": \"pingdeck\", \"other_token\": \"opaque123\"}";
        assert_eq!(
            mask_secrets(text, OPEN_GATEWAY_TOKEN),
            format!("{{\"apiKey\": \"pingdeck\", \"other_token\": \"{REDACTED}\"}}")
        );
    }

    #[test]
    fn a_gateway_token_under_a_credential_key_shows_its_mask_alone() {
        let text = format!(
            "{{\"apiKey\": \"{CURRENT}\"}}\nGEMINI_API_KEY={CURRENT}\ntoken={}\n",
            pending_token("cc")
        );
        assert_eq!(
            mask_secrets(&text, CURRENT),
            format!(
                "{{\"apiKey\": \"{CURRENT_SHOWN}\"}}\nGEMINI_API_KEY={CURRENT_SHOWN}\ntoken=pd_cc_••••••••\n"
            )
        );
    }

    #[test]
    fn pd_look_alikes_that_are_not_ours_are_masked_as_secrets() {
        let text = format!("x = pd_toolong_{}", "A".repeat(20));
        assert_eq!(mask_secrets(&text, "other"), format!("x = {REDACTED}"));
    }
}
