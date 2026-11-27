//! The redaction rules one by one: headers (R1), URL query and user info (R2), JSON keys (R3),
//! the text patterns and needles (R4-R9), URL paths (R10), plus a seeded property test.

use serde_json::{Value, json};

use super::{PD_TOKEN, Rng, SK_KEY, bounded, pairs, random_secret, raw};
use crate::llm::capture::redact::{mask_value, redact_url};
use crate::llm::capture::{RedactedHeader, redact_exchange, redact_exchange_hiding, redact_text};
use spanbay_base::secrets::REDACTED;

fn header_of(headers: &[RedactedHeader], name: &str) -> RedactedHeader {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .cloned()
        .unwrap_or_else(|| panic!("no header {name}"))
}

fn redact_headers(list: &[(&str, &str)]) -> Vec<RedactedHeader> {
    let mut exchange = raw();
    exchange.client_req_headers = pairs(list);
    redact_exchange(exchange, &[]).0.client_req_headers
}

#[test]
fn r1_every_listed_header_is_redacted_case_insensitively() {
    let names = [
        "Authorization",
        "PROXY-AUTHORIZATION",
        "X-Api-Key",
        "x-goog-api-key",
        "Api-Key",
        "X-Auth-Token",
        "x-amz-security-token",
        "Cookie",
        "Set-Cookie",
    ];
    for name in names {
        let headers = redact_headers(&[(name, "very-private-value-123456")]);
        let header = header_of(&headers, name);
        assert!(header.redacted, "{name}");
        assert!(
            !header.value.contains("private-value"),
            "{name}: {}",
            header.value
        );
    }
}

#[test]
fn r1_bearer_values_keep_the_scheme_a_prefix_and_the_last_four() {
    let headers = redact_headers(&[("Authorization", &format!("Bearer {SK_KEY}"))]);
    assert_eq!(headers[0].value, "Bearer sk-…2345");
    assert!(headers[0].redacted);
}

#[test]
fn r1_short_values_become_the_marker() {
    let headers = redact_headers(&[("x-api-key", "short"), ("Authorization", "Bearer tiny")]);
    assert_eq!(headers[0].value, REDACTED);
    assert_eq!(headers[1].value, format!("Bearer {REDACTED}"));
}

#[test]
fn r1_cookies_and_basic_credentials_are_hidden_entirely() {
    let headers = redact_headers(&[
        ("set-cookie", "session=abcdef123456; Path=/"),
        ("Authorization", "Basic dXNlcjpwYXNzd29yZDEyMzQ1"),
    ]);
    assert_eq!(headers[0].value, REDACTED);
    assert_eq!(headers[1].value, REDACTED);
}

#[test]
fn headers_without_secrets_are_untouched_and_untagged() {
    let headers = redact_headers(&[("content-type", "application/json"), ("retry-after", "2")]);
    assert!(headers.iter().all(|h| !h.redacted));
    assert_eq!(headers[1].value, "2");
}

#[test]
fn an_empty_credential_header_is_not_tagged() {
    let headers = redact_headers(&[("authorization", "")]);
    assert!(!headers[0].redacted);
}

#[test]
fn r1_compound_credential_names_are_redacted_but_rate_limit_counters_are_not() {
    let headers = redact_headers(&[
        ("X-Relay-Token", "very-private-value-123456"),
        ("x-client-secret", "very-private-value-123456"),
        ("x-ratelimit-remaining-tokens", "49999"),
    ]);
    for name in ["X-Relay-Token", "x-client-secret"] {
        let header = header_of(&headers, name);
        assert!(header.redacted, "{name}");
        assert!(!header.value.contains("private-value"), "{name}");
    }
    let counter = header_of(&headers, "x-ratelimit-remaining-tokens");
    assert!(!counter.redacted && counter.value == "49999");
}

#[test]
fn a_hidden_header_name_masks_a_value_below_the_needle_floor() {
    let mut exchange = raw();
    exchange.upstream_req_headers = pairs(&[("X-Org", "o-1"), ("x-trace", "t-1")]);
    let hidden = vec!["x-org".to_owned()];
    let stored = redact_exchange_hiding(exchange, &[], &hidden).0;
    let org = header_of(&stored.upstream_req_headers, "X-Org");
    assert!(org.redacted && org.value == REDACTED, "{org:?}");
    assert!(!header_of(&stored.upstream_req_headers, "x-trace").redacted);
}

#[test]
fn other_header_values_still_go_through_the_text_rules_and_needles() {
    let exchange = {
        let mut exchange = raw();
        exchange.upstream_resp_headers = pairs(&[
            ("x-echo", "relay-9f8e7d6c5b4a"),
            ("x-note", &format!("see {SK_KEY}")),
        ]);
        redact_exchange(exchange, &["relay-9f8e7d6c5b4a".to_owned()]).0
    };
    let echo = header_of(&exchange.upstream_resp_headers, "x-echo");
    assert!(echo.redacted && echo.value == REDACTED);
    let note = header_of(&exchange.upstream_resp_headers, "x-note");
    assert!(note.redacted && !note.value.contains("sk-"));
}

#[test]
fn r2_query_values_and_user_info_are_masked() {
    let mut exchange = raw();
    exchange.upstream_url = "https://user:hunter2@generativelanguage.googleapis.com/v1beta/models/g:streamGenerateContent?alt=sse&key=AIzaSyA-abcdefghijklmnopqrstuvwxyz01234&sig=short&model=gemini#frag".to_owned();
    let (stored, count) = redact_exchange(exchange, &[]);
    let url = &stored.upstream_url;
    assert!(!url.contains("hunter2") && !url.contains("user:"), "{url}");
    assert!(
        url.starts_with(&format!("https://{REDACTED}@generativelanguage")),
        "{url}"
    );
    assert!(
        url.contains("alt=sse") && url.contains("model=gemini"),
        "{url}"
    );
    assert!(url.contains("key=AIz…1234"), "{url}");
    assert!(url.contains(&format!("sig={REDACTED}")), "{url}");
    assert!(url.ends_with("#frag"));
    assert_eq!(count, 3);
}

#[test]
fn r2_covers_every_listed_query_key() {
    for key in [
        "key",
        "api_key",
        "apikey",
        "token",
        "access_token",
        "signature",
        "sig",
        "KEY",
    ] {
        let mut exchange = raw();
        exchange.upstream_url = format!("https://h.example/v1?{key}=0123456789abcdefXYZ&x=1");
        let stored = redact_exchange(exchange, &[]).0.upstream_url;
        assert!(!stored.contains("0123456789abcdef"), "{key}: {stored}");
        assert!(stored.contains("x=1"), "{key}: {stored}");
    }
}

#[test]
fn a_fragment_after_a_query_goes_through_the_text_rules() {
    // Regression guard (the fragment after a query was stored as is).
    let (url, count) = redact_url(&format!("https://h/p?a=1#access_token={SK_KEY}"), &[]);
    assert!(!url.contains(SK_KEY), "{url}");
    assert_eq!(url, format!("https://h/p?a=1#access_token={REDACTED}"));
    assert_eq!(count, 1);
    // The same fragment without a query was already redacted.
    let (url, count) = redact_url(&format!("https://h/p#access_token={SK_KEY}"), &[]);
    assert!(!url.contains(SK_KEY), "{url}");
    assert_eq!(count, 1);
    let (url, count) = redact_url("https://h/p?a=1#frag", &[]);
    assert_eq!((url.as_str(), count), ("https://h/p?a=1#frag", 0));
}

#[test]
fn r10_elides_a_token_in_a_url_path_in_text_and_in_the_url_field() {
    let url = "https://relay.acme.dev/t/AbCdEfGhIjKlMnOpQrStUvWx/v1/models?key=1";
    let (text, count) = redact_text(&format!("upstream said: GET {url} failed"), &[]);
    assert!(text.contains("/t/…/v1/models"), "{text}");
    assert!(!text.contains("AbCdEfGhIjKl"));
    assert!(count >= 1);
    let (stored, count) = redact_url(url, &[]);
    assert_eq!(
        stored,
        format!("https://relay.acme.dev/t/…/v1/models?key={REDACTED}")
    );
    assert_eq!(count, 2);
    let (bot, _) = redact_url(
        "https://api.telegram.org/bot123456:ABCDEFGHIJKLMNOPQRSTUVWXYZ0123/getMe",
        &[],
    );
    assert_eq!(bot, "https://api.telegram.org/…/getMe");
}

#[test]
fn r10_leaves_ordinary_paths_alone() {
    let (url, count) = redact_url("https://api.deepseek.com/v1/chat/completions", &[]);
    assert_eq!(url, "https://api.deepseek.com/v1/chat/completions");
    assert_eq!(count, 0);
    let (gemini, _) = redact_url(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse",
        &[],
    );
    assert!(
        gemini.contains("models/gemini-2.5-pro:streamGenerateContent?alt=sse"),
        "{gemini}"
    );
}

#[test]
fn the_upstream_url_of_an_exchange_gets_r10() {
    let mut exchange = raw();
    exchange.upstream_url =
        "https://relay.acme.dev/t/AbCdEfGhIjKlMnOpQrStUvWx/v1/chat/completions".to_owned();
    let stored = redact_exchange(exchange, &[]).0;
    assert_eq!(
        stored.upstream_url,
        "https://relay.acme.dev/t/…/v1/chat/completions"
    );
    assert_eq!(stored.redacted_count, 1);
}

#[test]
fn r3_json_keys_are_masked_at_any_depth_and_max_tokens_is_not() {
    let text = r#"{"api_key":"abc123456","apiKey":"x1","authorization":"Basic zz","access_token":"t","refresh_token":"rt","client_secret":"cs","password":"pw","secret":"s","token":"tk","max_tokens":10,"nested":{"deep":[{"token":"deep-token-value"}]}}"#;
    let (out, count) = redact_text(text, &[]);
    assert_eq!(count, 10);
    let value: Value = serde_json::from_str(&out).unwrap();
    for key in [
        "api_key",
        "apiKey",
        "authorization",
        "access_token",
        "refresh_token",
        "client_secret",
        "password",
        "secret",
        "token",
    ] {
        assert_eq!(value[key], REDACTED, "{key}");
    }
    assert_eq!(value["max_tokens"], 10);
    assert_eq!(value["nested"]["deep"][0]["token"], REDACTED);
}

#[test]
fn r3_leaves_numeric_and_already_masked_values_consistent() {
    let (out, count) = redact_text(r#"{"token":"••••••","secret":12345,"other":1}"#, &[]);
    let value: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["token"], REDACTED);
    assert_eq!(value["secret"], REDACTED);
    assert_eq!(value["other"], 1);
    assert_eq!(count, 1);
}

#[test]
fn r3_reaches_json_inside_a_json_string() {
    let text =
        json!({"arguments": "{\"password\":\"hunter2-value\",\"city\":\"Oslo\"}"}).to_string();
    let (out, count) = redact_text(&text, &[]);
    assert!(!out.contains("hunter2"), "{out}");
    assert!(out.contains("Oslo"));
    assert_eq!(count, 1);
    assert!(serde_json::from_str::<Value>(&out).is_ok());
}

#[test]
fn r3_key_names_inside_values_are_not_keys() {
    let (out, count) = redact_text(r#"{"note":"the password: is fine","token_count":3}"#, &[]);
    assert_eq!(count, 0);
    assert!(out.contains("token_count"));
}

/// What one [`redact_text`] case expects of the count.
#[derive(Debug, Clone, Copy)]
enum Count {
    /// Not checked.
    Any,
    AtLeastOne,
    Exactly(u32),
}

/// One [`redact_text`] case: the input, the needles, the strings that must be gone, the strings
/// that must be kept, the whole expected output when it is pinned, and the count.
struct TextCase {
    text: String,
    needles: Vec<String>,
    gone: Vec<String>,
    kept: Vec<&'static str>,
    output: Option<String>,
    count: Count,
}

/// `secret` between a context word and a tail must be removed and counted.
fn embedded(context: &str, secret: &str) -> TextCase {
    TextCase {
        text: format!("{context} {secret} tail"),
        needles: Vec::new(),
        gone: vec![secret.to_owned()],
        kept: Vec::new(),
        output: None,
        count: Count::AtLeastOne,
    }
}

fn text_cases() -> Vec<TextCase> {
    let mut cases = Vec::new();
    // R4: sk- keys, including Anthropic and project ones.
    for secret in [
        SK_KEY,
        "sk-ant-api03-abcdefghijklmnopqrstuvwx",
        "sk-proj-abcdefghijklmnopqrstuvwxyz_01-23",
    ] {
        cases.push(embedded("key", secret));
    }
    // R4 does not flag ordinary words.
    cases.push(TextCase {
        text: "a risk-assessment-of-the-whole-project and sk-short".to_owned(),
        needles: Vec::new(),
        gone: Vec::new(),
        kept: vec!["risk-assessment"],
        output: None,
        count: Count::Exactly(0),
    });
    // R5: gateway tokens.
    for secret in [PD_TOKEN, "pdk_abcdefghijklmnop1234"] {
        cases.push(embedded("token", secret));
    }
    // R6: provider and cloud keys.
    for secret in [
        "AIzaSyA-abcdefghijklmnopqrstuvwxyz01234".to_owned(),
        format!("gsk_{}", "aB3".repeat(8)),
        format!("xai-{}", "Zz9".repeat(8)),
        format!("ghp_{}", "a1B2".repeat(9)),
        format!("github_pat_{}", "a1B2_".repeat(11)),
        "AKIAABCDEFGHIJKLMNOP".to_owned(),
    ] {
        cases.push(embedded("credential", &secret));
    }
    // R7: Bearer values and JWTs.
    cases.push(embedded("header", "Bearer abcdefghijklmnop.qrstuv-wxyz=="));
    cases.push(embedded(
        "jwt",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r",
    ));
    // R8: PEM private keys, whole or cut.
    let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA\nabcdefghijkl\n-----END RSA PRIVATE KEY-----";
    cases.push(TextCase {
        text: format!("before\n{pem}\nafter"),
        needles: Vec::new(),
        gone: vec!["MIIEow".to_owned()],
        kept: vec!["before", "after"],
        output: None,
        count: Count::Exactly(1),
    });
    cases.push(TextCase {
        text: "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBg\nkqhkiG9w".to_owned(),
        needles: Vec::new(),
        gone: vec!["MIIEvQ".to_owned()],
        kept: Vec::new(),
        output: None,
        count: Count::Any,
    });
    // R9: a needle in its URL-encoded form (next to a JSON-escaped one).
    let needle = "relay key/with+chars 99".to_owned();
    let encoded: String = url::form_urlencoded::byte_serialize(needle.as_bytes()).collect();
    cases.push(TextCase {
        text: format!("q={encoded} and {}", needle.replace('/', "\\/")),
        needles: vec![needle],
        gone: vec![encoded],
        kept: Vec::new(),
        output: None,
        count: Count::Any,
    });
    // R9: one needle floor of eight, in raw, URL-encoded and JSON-escaped form.
    let needle = "ab\"cd+ef/g".to_owned();
    let encoded: String = url::form_urlencoded::byte_serialize(needle.as_bytes()).collect();
    let escaped = serde_json::to_string(&needle)
        .unwrap()
        .trim_matches('"')
        .to_owned();
    cases.push(TextCase {
        text: format!("raw {needle} enc {encoded} json {escaped}"),
        needles: vec![needle.clone()],
        gone: vec![needle, encoded, escaped],
        kept: Vec::new(),
        output: None,
        count: Count::Any,
    });
    // A needle of 7 characters is below the floor of 8 and is kept.
    cases.push(TextCase {
        text: "an abcdefg word".to_owned(),
        needles: vec!["abcdefg".to_owned()],
        gone: Vec::new(),
        kept: Vec::new(),
        output: Some("an abcdefg word".to_owned()),
        count: Count::Exactly(0),
    });
    cases.push(TextCase {
        text: "an abcdefgh word".to_owned(),
        needles: vec!["abcdefgh".to_owned()],
        gone: Vec::new(),
        kept: Vec::new(),
        output: Some(format!("an {REDACTED} word")),
        count: Count::Any,
    });
    cases
}

#[test]
fn redact_text_removes_and_counts_each_pattern() {
    for case in text_cases() {
        let (out, count) = redact_text(&case.text, &case.needles);
        for secret in &case.gone {
            assert!(!out.contains(secret.as_str()), "{secret} survived in {out}");
        }
        for word in &case.kept {
            assert!(out.contains(word), "{word} was lost in {out}");
        }
        if let Some(expected) = &case.output {
            assert_eq!(&out, expected);
        }
        match case.count {
            Count::Any => {}
            Count::AtLeastOne => assert!(count >= 1, "{}", case.text),
            Count::Exactly(n) => assert_eq!(count, n, "{}", case.text),
        }
    }
}

#[test]
fn r9_a_relay_key_without_a_known_prefix_is_removed_from_an_error_body() {
    let mut exchange = raw();
    exchange.upstream_status = Some(401);
    exchange.upstream_resp_body = Some(bounded(
        r#"{"error":{"message":"Incorrect API key provided: relay-9f8e7d6c5b4a."}}"#,
    ));
    let (stored, count) = redact_exchange(exchange, &["relay-9f8e7d6c5b4a".to_owned()]);
    let body = stored.upstream_resp_body.unwrap().text.unwrap();
    assert!(!body.contains("relay-9f8e7d6c5b4a"), "{body}");
    assert!(body.contains(REDACTED));
    assert_eq!(count, 1);
}

#[test]
fn mask_value_keeps_prefix_and_last_four_from_twenty_four_characters() {
    assert_eq!(mask_value(SK_KEY), "sk-…2345");
    // Regression guard (was "123…9012": 7 of 12 characters shown).
    assert_eq!(mask_value("123456789012"), "…9012");
    assert_eq!(mask_value("12345678901"), REDACTED);
    assert_eq!(mask_value(""), REDACTED);
}

#[test]
fn mask_value_shows_only_the_last_four_of_a_secret_under_twenty_four_characters() {
    // Regression for capture B1: the tiers of `mask_secret_hint`, with the marker below 12.
    assert_eq!(mask_value("abcdefghijklmnopqrstuvw"), "…tuvw");
    assert_eq!(mask_value("abcdefghijklmnopqrstuvwx"), "abc…uvwx");
    assert_eq!(mask_value("  abcdefghijkl  "), "…ijkl");
    let headers = redact_headers(&[("x-api-key", "relay-0123456789")]);
    assert_eq!(headers[0].value, "…6789");
    let (url, _) = redact_url("https://h.example/v1?key=0123456789abcdef", &[]);
    assert_eq!(url, "https://h.example/v1?key=…cdef");
}

#[test]
fn redaction_is_idempotent() {
    let text = format!(
        r#"{{"password":"hunter2-long","text":"{SK_KEY} Bearer abcdefghijklmnopqrstu x"}}"#
    );
    let (once, _) = redact_text(&text, &[]);
    let (twice, count) = redact_text(&once, &[]);
    assert_eq!(once, twice);
    assert_eq!(count, 0);
}

#[test]
fn property_no_embedded_secret_survives() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let words = [
        "the", "model", "said", "hello", "world", "error", "request", "failed", "ok",
    ];
    for round in 0..400 {
        let needle = format!("relay-{}", rng.chars("0123456789abcdef", 12));
        let mut text = String::new();
        let mut secrets = Vec::new();
        for _ in 0..1 + rng.below(3) {
            for _ in 0..rng.below(6) {
                text.push_str(words[rng.below(words.len())]);
                text.push(' ');
            }
            let secret = random_secret(&mut rng, &needle, true);
            match rng.below(3) {
                0 => text.push_str(&format!("{secret} ")),
                1 => text.push_str(&format!(r#"{{"note":"{secret}"}} "#)),
                _ => text.push_str(&format!("Authorization: {secret}\n")),
            }
            secrets.push(secret);
        }
        let (out, count) = redact_text(&text, std::slice::from_ref(&needle));
        for secret in &secrets {
            // The Bearer scheme word itself is not secret; the value after it is.
            let value = secret.strip_prefix("Bearer ").unwrap_or(secret);
            assert!(
                !out.contains(value),
                "round {round}: {value} survived in {out}"
            );
        }
        assert!(count >= 1, "round {round}");
    }
}
