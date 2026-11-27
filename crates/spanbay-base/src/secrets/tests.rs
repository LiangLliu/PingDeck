use super::redact::*;
use super::*;
use std::collections::HashMap;

/// An in-memory store for tests of code that only needs the trait.
#[derive(Default)]
struct MapStore(std::sync::Mutex<HashMap<String, String>>);

impl SecretStore for MapStore {
    fn get(&self, secret_ref: &str) -> Result<Option<String>, AppError> {
        Ok(self.0.lock().unwrap().get(secret_ref).cloned())
    }
    fn put(&self, secret_ref: &str, plaintext: &str) -> Result<SecretHint, AppError> {
        self.0
            .lock()
            .unwrap()
            .insert(secret_ref.to_owned(), plaintext.to_owned());
        self.hint(secret_ref)
    }
    fn delete(&self, secret_ref: &str) -> Result<bool, AppError> {
        Ok(self.0.lock().unwrap().remove(secret_ref).is_some())
    }
    fn hint(&self, secret_ref: &str) -> Result<SecretHint, AppError> {
        let value = self.get(secret_ref)?;
        Ok(SecretHint {
            secret_ref: secret_ref.to_owned(),
            has_secret: value.is_some(),
            hint: value.as_deref().map(mask_secret_hint).unwrap_or_default(),
            updated_at_ms: 0,
        })
    }
    fn active_redaction_needles(&self) -> Vec<String> {
        self.0.lock().unwrap().values().cloned().collect()
    }
}

#[test]
fn hints_never_reveal_the_middle_of_a_secret() {
    assert_eq!(mask_secret_hint(""), "");
    assert_eq!(mask_secret_hint("  "), "");
    assert_eq!(mask_secret_hint("short"), "••••");
    assert_eq!(mask_secret_hint("sk-abcdefghijklmnop9f2a"), "…9f2a");
    assert_eq!(
        mask_secret_hint("sk-abcdefghijklmnopqrstuvwx9f2a"),
        "sk-…9f2a"
    );
    assert_eq!(mask_secret_hint("sk-abcdefgh"), "••••");
    // Twelve characters: never more than the last four.
    assert_eq!(mask_secret_hint("abcdefghijkl"), "…ijkl");
}

#[test]
fn redaction_covers_raw_encoded_and_escaped_forms() {
    let secret = "sk-live/abc+123\"q".to_owned();
    let encoded: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
    let text = format!(
        "raw {secret} url ?key={encoded} json {{\"k\":\"sk-live/abc+123\\\"q\"}} keep sk-live"
    );
    let out = redact_secrets(&text, &[secret]);
    assert!(!out.contains("abc"), "{out}");
    assert!(out.contains("keep sk-live"));
    assert_eq!(out.matches(REDACTED).count(), 3, "{out}");
}

#[test]
fn short_needles_are_ignored_and_credential_headers_are_always_masked() {
    let out = redact_secrets("hello abc world", &["abc".to_owned()]);
    assert_eq!(out, "hello abc world");
    let headers = "Authorization: Bearer top-secret-token\r\nx-api-key: k-123\r\nAccept: */*\r\n\
                       Cookie: sid=abc; theme=dark";
    let out = redact_secrets(headers, &[]);
    assert!(!out.contains("top-secret-token") && !out.contains("k-123"));
    assert!(!out.contains("sid=abc"));
    assert!(out.contains("Accept: */*"));
    let json = r#"{"headers":{"Authorization":"Bearer abc.def","x-goog-api-key":"g-9"}}"#;
    let out = redact_secrets(json, &[]);
    assert!(!out.contains("abc.def") && !out.contains("g-9"), "{out}");
    assert!(
        serde_json::from_str::<serde_json::Value>(&out).is_ok(),
        "{out}"
    );
}

#[test]
fn longer_secrets_win_over_the_shorter_one_they_contain() {
    let out = redact_secrets(
        "a sk-abcdef-extra b",
        &["sk-abcdef".to_owned(), "sk-abcdef-extra".to_owned()],
    );
    assert_eq!(out, format!("a {REDACTED} b"));
}

#[test]
fn placeholders_resolve_and_a_missing_secret_is_an_error() {
    let store = MapStore::default();
    store.put("mcp.s1.env.TOKEN", "t-1").unwrap();
    let text = "--token={{secret:mcp.s1.env.TOKEN}} x {{secret:mcp.s1.env.TOKEN}} {{open";
    assert_eq!(
        resolve_placeholders(&store, text).unwrap(),
        "--token=t-1 x t-1 {{open"
    );
    let error = resolve_placeholders(&store, "{{secret:nope}}").unwrap_err();
    assert!(error.message().contains("nope"));
    assert_eq!(
        SecretRef::mcp_env("s1", "TOKEN").placeholder(),
        "{{secret:mcp.s1.env.TOKEN}}"
    );
    assert_eq!(SecretRef::llm_key("k1").as_str(), "llm.key.k1");
}

/// Runs every `(input, expected)` row through [`redact_secrets`]; `<R>` stands for
/// [`REDACTED`]. Redacting the output again must change nothing.
fn check(rows: &[(&str, &str)]) {
    for (input, expected) in rows {
        let expected = expected.replace("<R>", REDACTED);
        let once = redact_secrets(input, &[]);
        assert_eq!(once, expected, "input: {input}");
        assert_eq!(redact_secrets(&once, &[]), once, "not idempotent: {input}");
    }
}

#[test]
fn camel_case_keys_pem_blocks_and_more_token_shapes_are_masked() {
    let gsk = format!("gsk_{}", "aB3".repeat(7));
    let xai = format!("xai-{}", "Zz9".repeat(7));
    let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\nIBAAKCAQEA\n-----END RSA PRIVATE KEY-----";
    let cut = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBg\nkqhkiG9w0BAQEFAASC";
    let cert = "-----BEGIN CERTIFICATE-----\nMIIDdz\n-----END CERTIFICATE-----";
    check(&[
        (
            r#"{"apiKey":"k1","clientSecret":"c1","refreshToken":"r1","accessToken":"a1"}"#,
            r#"{"apiKey":"<R>","clientSecret":"<R>","refreshToken":"<R>","accessToken":"<R>"}"#,
        ),
        (
            r#"{"authorization": "Bearer x.y", "password": "p w", "n": 1}"#,
            r#"{"authorization": "<R>", "password": "<R>", "n": 1}"#,
        ),
        (gsk.as_str(), "<R>"),
        ("gsk_short1234567890123", "gsk_short1234567890123"),
        (xai.as_str(), "<R>"),
        ("xai-short", "xai-short"),
        (format!("a {pem} b").as_str(), "a <R> b"),
        (
            format!("a {pem}\nmid\n{pem} b").as_str(),
            "a <R>\nmid\n<R> b",
        ),
        (format!("log: {cut}").as_str(), "log: <R>"),
        (cert, cert),
    ]);
    // Already-clean text is returned unchanged.
    let clean = "nothing to hide here: just text, 12345 and -----";
    assert_eq!(redact_secrets(clean, &[]), clean);
}

#[test]
fn credential_headers_are_masked_and_other_headers_kept() {
    check(&[
        ("Authorization: Bearer abc.def", "Authorization: <R>"),
        ("authorization: Basic dXNlcjpwYXNz", "authorization: <R>"),
        (
            "Proxy-Authorization: Basic dXNlcjpwYXNz",
            "Proxy-Authorization: <R>",
        ),
        ("x-api-key: k-1", "x-api-key: <R>"),
        ("X-Goog-Api-Key: g-9", "X-Goog-Api-Key: <R>"),
        ("api-key: az-1", "api-key: <R>"),
        ("Cookie: sid=abc; theme=dark", "Cookie: <R>"),
        ("Set-Cookie: sid=1; HttpOnly", "Set-Cookie: <R>"),
        ("X-Auth-Token: tok-123", "X-Auth-Token: <R>"),
        ("x-auth-token=tok-123", "x-auth-token=<R>"),
        (
            "anthropic-version: 2023-06-01",
            "anthropic-version: 2023-06-01",
        ),
        (
            "Accept: */*\r\nContent-Type: application/json",
            "Accept: */*\r\nContent-Type: application/json",
        ),
    ]);
}

#[test]
fn credential_query_parameters_are_masked() {
    check(&[
        (
            "https://x.test/y?key=abc123&b=1",
            "https://x.test/y?key=<R>&b=1",
        ),
        (
            "https://x.test/?a=1&api_key=xx#frag",
            "https://x.test/?a=1&api_key=<R>#frag",
        ),
        ("?apikey=1", "?apikey=<R>"),
        ("?access_token=1&x=2", "?access_token=<R>&x=2"),
        ("?x=1&token=t", "?x=1&token=<R>"),
        ("?secret=s", "?secret=<R>"),
        ("?signature=s&sig=t", "?signature=<R>&sig=<R>"),
        ("?KEY=upper", "?KEY=<R>"),
        (
            "?monkey=1&keyword=abc&sigma=2&page=2",
            "?monkey=1&keyword=abc&sigma=2&page=2",
        ),
    ]);
}

#[test]
fn form_and_json_credential_keys_are_masked() {
    check(&[
        ("password=hunter2&user=bob", "password=<R>&user=bob"),
        (
            "user=bob&client_secret=abc&grant_type=x",
            "user=bob&client_secret=<R>&grant_type=x",
        ),
        (
            "refresh_token=r1&access_token=a1",
            "refresh_token=<R>&access_token=<R>",
        ),
        (
            "api_key=k secret=s token=t",
            "api_key=<R> secret=<R> token=<R>",
        ),
        (
            "max_tokens=5&token_type=x&passwords=1",
            "max_tokens=5&token_type=x&passwords=1",
        ),
        (
            r#"{"password":"p@ss\"w","user":"bob"}"#,
            r#"{"password":"<R>","user":"bob"}"#,
        ),
        (
            r#"{"client_secret": "abc", "x": 1}"#,
            r#"{"client_secret": "<R>", "x": 1}"#,
        ),
        (
            r#"{"refresh_token":"r","access_token":"a","api_key":"k","secret":"s","token":"t"}"#,
            r#"{"refresh_token":"<R>","access_token":"<R>","api_key":"<R>","secret":"<R>","token":"<R>"}"#,
        ),
        (
            r#"{"max_tokens": 5, "token_type": "bearer"}"#,
            r#"{"max_tokens": 5, "token_type": "bearer"}"#,
        ),
    ]);
    let json = r#"{"password":"x\"y","Authorization":"Bearer y","n":1}"#;
    let out = redact_secrets(json, &[]);
    assert!(
        serde_json::from_str::<serde_json::Value>(&out).is_ok(),
        "{out}"
    );
}

#[test]
fn bearer_values_and_token_shapes_are_masked() {
    let aiza = format!("AIza{}", "A".repeat(35));
    let pat = format!("github_pat_{}", "A1_".repeat(17));
    let pd = format!("pd_gw_{}", "a1B2".repeat(8));
    let pdk = format!("pdk_{}", "A_".repeat(10));
    let ghp30 = format!("ghp_{}", "a".repeat(36));
    let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghijk_lmn";
    let rows: Vec<(String, String)> = vec![
        (
            "token is Bearer abcDEF123+/= ok".into(),
            "token is Bearer <R> ok".into(),
        ),
        ("bearer of bad news".into(), "bearer of bad news".into()),
        ("sk-ant-api03-ABCDEFGHIJKLMNOP.".into(), "<R>.".into()),
        ("a sk-short b".into(), "a sk-short b".into()),
        (
            "risk-assessment-framework-v2".into(),
            "risk-assessment-framework-v2".into(),
        ),
        (format!("k={aiza}!"), "k=<R>!".into()),
        (
            format!("k=AIza{}", "A".repeat(34)),
            format!("k=AIza{}", "A".repeat(34)),
        ),
        (ghp30.clone(), "<R>".into()),
        (
            format!("ghp_{}", "a".repeat(35)),
            format!("ghp_{}", "a".repeat(35)),
        ),
        (
            format!("gho_{0} ghu_{0} ghs_{0} ghr_{0}", "b".repeat(36)),
            "<R> <R> <R> <R>".into(),
        ),
        (
            format!("ghx_{}", "a".repeat(36)),
            format!("ghx_{}", "a".repeat(36)),
        ),
        (pat, "<R>".into()),
        ("github_pat_short".into(), "github_pat_short".into()),
        (
            "xoxb-1234567890-abcdef xoxp-1234567890 xoxz-1234567890".into(),
            "<R> <R> xoxz-1234567890".into(),
        ),
        ("AKIAIOSFODNN7EXAMPLE".into(), "<R>".into()),
        (
            "AKIA123 akiaiosfodnn7example".into(),
            "AKIA123 akiaiosfodnn7example".into(),
        ),
        (
            format!("pd_ab_{0} pd_a1b2_{0}", "A".repeat(16)),
            "<R> <R>".into(),
        ),
        (format!("x {pd}"), "x <R>".into()),
        (
            // R5 matches the prefix followed by 16 token characters, whatever the tag; a
            // longer word that merely ends in `pd_…` is not a token.
            format!("pd_x_{0} pd_toolong_{0} xpd_gw_{0}", "A".repeat(16)),
            format!("<R> <R> xpd_gw_{0}", "A".repeat(16)),
        ),
        (pdk, "<R>".into()),
        ("pdk_short".into(), "pdk_short".into()),
        (format!("t={jwt};"), "t=<R>;".into()),
        (
            "eyJhbGciOiJIUzI1NiJ9.short".into(),
            "eyJhbGciOiJIUzI1NiJ9.short".into(),
        ),
    ];
    for (input, expected) in &rows {
        let expected = expected.replace("<R>", REDACTED);
        let once = redact_secrets(input, &[]);
        assert_eq!(once, expected, "input: {input}");
        assert_eq!(redact_secrets(&once, &[]), once, "not idempotent: {input}");
    }
}

#[test]
fn ordinary_prose_and_code_are_untouched() {
    let prose = "The quick brown fox: jumps over the lazy dog. Use `cargo test` (key insight: \
                     tokens are cheap). See https://example.com/docs?page=2&sort=asc#top and \
                     {\"name\":\"spanbay\",\"count\":3}. Authors: A, B. anthropic-version: 2023-06-01";
    assert_eq!(redact_secrets(prose, &[]), prose);
}

#[test]
fn needles_are_masked_in_raw_url_and_json_forms_inside_patterns_too() {
    let secret = "p@ss w\"rd\\1".to_owned();
    let encoded: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
    let text = format!(
        "raw {secret} | url x?note={encoded} | json {{\"note\":{}}}",
        serde_json::to_string(&secret).unwrap()
    );
    let out = redact_secrets(&text, &[secret]);
    assert_eq!(out.matches(REDACTED).count(), 3, "{out}");
    assert!(!out.contains("ss w") && !out.contains("p%40ss"), "{out}");
}

#[test]
fn redacting_a_megabyte_is_fast_and_complete() {
    let line = "Lorem ipsum dolor sit amet, consectetur adipiscing elit; sed do eiusmod tempor \
                    incididunt ut labore et dolore magna aliqua. 0123456789 abcdefghijklmnopqrstuvwxyz\n";
    let mut text = String::with_capacity(1 << 20);
    let mut n = 0_usize;
    while text.len() < 1 << 20 {
        text.push_str(line);
        if n.is_multiple_of(500) {
            text.push_str(
                "Authorization: Bearer abcdef.ghijkl\n?token=tk123&x=1 sk-abcdefghijklmnopqrstuv\n",
            );
            text.push_str(
                "Cookie: sid=\"qq1\"; other=\"zz2\"\nDB_PASSWORD=\"pw 3\"\nx-relay-token: rt4\n",
            );
        }
        n += 1;
    }
    let started = std::time::Instant::now();
    let out = redact_secrets(&text, &["needle-value-123".to_owned()]);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    assert!(
        !out.contains("abcdef.ghijkl") && !out.contains("tk123") && !out.contains("sk-abcdefghij")
    );
    assert!(
        !out.contains("qq1")
            && !out.contains("zz2")
            && !out.contains("pw 3")
            && !out.contains("rt4")
    );
    assert!(out.contains("Lorem ipsum"));
}

#[test]
fn opaque_url_path_segments_are_elided() {
    let long = "A1b2C3d4E5f6G7h8I9j0K1l2"; // 24 characters
    assert_eq!(long.len(), MIN_OPAQUE_SEGMENT_CHARS);
    let short = &long[..23];
    let rows: Vec<(String, String)> = vec![
        (
            format!("https://api.x.test/v1/{long}/balance"),
            "https://api.x.test/v1/…/balance".into(),
        ),
        (
            format!("https://api.x.test/{long}"),
            "https://api.x.test/…".into(),
        ),
        (
            format!("https://api.x.test/{long}/"),
            "https://api.x.test/…/".into(),
        ),
        (
            format!("GET http://h/a/{long}?page=2#top ok"),
            "GET http://h/a/…?page=2#top ok".into(),
        ),
        (
            format!("see (https://h/{long}). and https://h/{long}."),
            "see (https://h/…). and https://h/….".into(),
        ),
        (
            format!("https://h/{long}/x/{long}_-z"),
            "https://h/…/x/…".into(),
        ),
        (
            format!("https://api.x.test/{short}/v1"),
            format!("https://api.x.test/{short}/v1"),
        ),
        (
            format!("https://h/{long}.json"),
            format!("https://h/{long}.json"),
        ),
        (
            format!("https://h/docs?ref={long}"),
            format!("https://h/docs?ref={long}"),
        ),
        (
            format!("https://{long}.example.com/x"),
            format!("https://{long}.example.com/x"),
        ),
        (format!("https://{long}"), format!("https://{long}")),
        (
            format!("plain {long} words and /{long}/ paths"),
            format!("plain {long} words and /{long}/ paths"),
        ),
        (
            "https://example.com/docs/getting-started".into(),
            "https://example.com/docs/getting-started".into(),
        ),
    ];
    for (input, expected) in &rows {
        let once = redact_secrets(input, &[]);
        assert_eq!(&once, expected, "input: {input}");
        assert_eq!(redact_secrets(&once, &[]), once, "not idempotent: {input}");
    }
}

#[test]
fn credential_and_identity_keys_are_elided_in_json_and_the_rest_is_redacted() {
    let input = serde_json::json!({
        "apiKey": "k-1",
        "client-secret": 12345,
        "Refresh_Token": null,
        "Authorization": "Bearer abc.def",
        "email": "me@example.com",
        "user_id": 42,
        "userName": "bob",
        "AccountId": "acc-9",
        "nickname": true,
        "display_name": "Bob B.",
        "name": "kept name",
        "id": 7,
        "count": 3,
        "ok": false,
        "nothing": null,
        "note": "needle-value-123 at https://h/A1b2C3d4E5f6G7h8I9j0K1l2/x?token=zz",
        "nested": {"password": "p", "items": [{"email": "a@b.c", "plan": "pro"}, "x-api-key: kk", 1.5]},
        "token": {"value": "inner", "email": "e@x.y"},
        "list": ["a", {"secret": "s"}]
    });
    let out = redact_json(&input, &["needle-value-123".to_owned()]);
    let expected = serde_json::json!({
        "apiKey": "…",
        "client-secret": "…",
        "Refresh_Token": "…",
        "Authorization": "…",
        "email": "…",
        "user_id": "…",
        "userName": "…",
        "AccountId": "…",
        "nickname": "…",
        "display_name": "…",
        "name": "kept name",
        "id": 7,
        "count": 3,
        "ok": false,
        "nothing": null,
        "note": format!("{REDACTED} at https://h/…/x?token={REDACTED}"),
        "nested": {"password": "…", "items": [{"email": "…", "plan": "pro"}, format!("x-api-key: {REDACTED}"), 1.5]},
        "token": {"value": "inner", "email": "…"},
        "list": ["a", {"secret": "…"}]
    });
    assert_eq!(out, expected);
    assert_eq!(redact_json(&out, &[]), out, "not idempotent");
    assert_eq!(
        redact_json(&serde_json::json!("sk-abcdefghijklmnopqrstuv"), &[]),
        serde_json::json!(REDACTED)
    );
    assert_eq!(
        redact_json(&serde_json::json!(5), &[]),
        serde_json::json!(5)
    );
}

#[test]
fn compound_and_camel_case_keys_are_credential_keys_and_look_alikes_are_not() {
    for key in [
        "password",
        "api_key",
        "apiKey",
        "client_secret",
        "refreshToken",
        "ACCESS_TOKEN",
        "db_password",
        "AWS_SECRET_ACCESS_KEY",
        "id_token",
        "session_token",
        "x-relay-token",
        "secretKey",
        "privateKeyId",
    ] {
        assert!(key_is_credential(key), "{key}");
    }
    for key in [
        "max_tokens",
        "token_type",
        "passwords",
        "x-ratelimit-remaining-tokens",
        "monkey",
        "id",
        "name",
    ] {
        assert!(!key_is_credential(key), "{key}");
    }
    assert!(is_secret_header_name("X-Relay-Token"));
    assert!(is_secret_header_name("Cookie"));
    assert!(is_secret_header_name("X-Amz-Security-Token"));
    assert!(!is_secret_header_name("x-request-id"));
    assert!(!is_secret_header_name("x-api-version"));
}

#[test]
fn compound_key_names_and_lower_case_bearer_are_masked_in_text() {
    let stripe = format!("sk_live_{}", "aB3dE5fG7hJ9kL1m");
    let npm = format!("npm_{}", "a1B2c3".repeat(6));
    let google = format!("ya29.{}", "a0AfB_byC".repeat(4));
    let gitlab = format!("glpat-{}", "xYz12345".repeat(3));
    check(&[
        ("DB_PASSWORD=hunter2hunter2", "DB_PASSWORD=<R>"),
        (
            "AWS_SECRET_ACCESS_KEY=abc/def+ghi",
            "AWS_SECRET_ACCESS_KEY=<R>",
        ),
        (r#"DB_PASSWORD="two words""#, r#"DB_PASSWORD="<R>""#),
        (r#"{"db_password":"p1"}"#, r#"{"db_password":"<R>"}"#),
        (r#"{"id_token":"abc"}"#, r#"{"id_token":"<R>"}"#),
        (
            r#"{"session_token":"abc","n":1}"#,
            r#"{"session_token":"<R>","n":1}"#,
        ),
        ("bearer abcdefghijklmnop", "bearer <R>"),
        ("BEARER abcdefghijklmnop", "BEARER <R>"),
        ("x-relay-token: abc123", "x-relay-token: <R>"),
        (
            "x-ratelimit-remaining-tokens: 99999",
            "x-ratelimit-remaining-tokens: 99999",
        ),
        (stripe.as_str(), "<R>"),
        (npm.as_str(), "<R>"),
        (google.as_str(), "<R>"),
        (gitlab.as_str(), "<R>"),
        (
            "npm_short sk_live_short ya29.short",
            "npm_short sk_live_short ya29.short",
        ),
    ]);
}

#[test]
fn a_quoted_value_does_not_end_a_header_line_redaction() {
    check(&[
        (
            r#"Cookie: sid="abc123secret"; other=zzzsecondcookie"#,
            "Cookie: <R>",
        ),
        (
            r#"Authorization: Digest username="bob", response="d3adbeef", cnonce="c0ffee""#,
            "Authorization: <R>",
        ),
        (
            "GET / HTTP/1.1\r\nCookie: a=\"b\"; c=d\r\nAccept: */*",
            "GET / HTTP/1.1\r\nCookie: <R>\r\nAccept: */*",
        ),
        (
            r#"{"cookie": "a=\"b\"; c=d", "model": "m"}"#,
            r#"{"cookie": "<R>", "model": "m"}"#,
        ),
        (
            r#"{"Authorization":"Digest u=\"bob\"","x":1}"#,
            r#"{"Authorization":"<R>","x":1}"#,
        ),
        (
            "{'authorization': 'Bearer abc', 'n': 1}",
            "{'authorization': '<R>', 'n': 1}",
        ),
    ]);
    let out = redact_secrets(r#"{"cookie":"a=\"b\"","n":1}"#, &[]);
    assert!(
        serde_json::from_str::<serde_json::Value>(&out).is_ok(),
        "{out}"
    );
}

#[test]
fn url_paths_can_be_redacted_on_their_own_and_telegram_bot_tokens_are_elided() {
    let long = "AbCdEfGhIjKlMnOpQrStUvWx";
    assert_eq!(
        redact_url_path(&format!("/t/{long}/v1/models")),
        "/t/…/v1/models"
    );
    assert_eq!(
        redact_url_path("/v1/chat/completions"),
        "/v1/chat/completions"
    );
    assert_eq!(redact_url_path(""), "");
    assert_eq!(redact_url_path("/"), "/");
    assert_eq!(redact_url_path(&format!("{long}.")), "….");
    let bot = "bot123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw";
    assert_eq!(redact_url_path(&format!("/{bot}/getMe")), "/…/getMe");
    for kept in [
        "/bot/getMe",
        "/botx:AAHdqTcvCH1vGWJxfSeofSAs0K5",
        "/bot123:short",
    ] {
        assert_eq!(redact_url_path(kept), kept);
    }
    let text = format!(
        "GET https://relay.acme.dev/t/{long}/v1/models?key=1 and https://api.telegram.org/{bot}/getMe"
    );
    let out = redact_secrets(&text, &[]);
    assert_eq!(
        out,
        format!(
            "GET https://relay.acme.dev/t/…/v1/models?key={REDACTED} and https://api.telegram.org/…/getMe"
        )
    );
    assert_eq!(redact_secrets(&out, &[]), out);
}

#[test]
fn needles_shorter_than_the_floor_are_ignored() {
    assert_eq!(MIN_REDACTION_CHARS, 8);
    assert_eq!(
        redact_secrets("a sk-1234 b", &["sk-1234".to_owned()]),
        "a sk-1234 b"
    );
    assert_eq!(
        redact_secrets("a abc-1234 b", &["abc-1234".to_owned()]),
        format!("a {REDACTED} b")
    );
}

mod table_tests {
    //! One positive and one false-positive-guard sample per rule of the redaction table (R1 to
    //! R10) and the properties the sinks rely on.

    use super::*;

    /// A deterministic pseudo-random token of `len` characters from `alphabet`.
    fn random_token(seed: &mut u64, len: usize, alphabet: &[u8]) -> String {
        (0..len)
            .map(|_| {
                *seed = seed
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                alphabet[usize::try_from(*seed >> 33).unwrap_or(0) % alphabet.len()] as char
            })
            .collect()
    }

    const TOKEN_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

    #[test]
    fn every_rule_has_a_positive_and_a_guard_sample() {
        let body = "x".repeat(40);
        let cases: [(&str, String, bool); 20] = [
            (
                "R1",
                "X-Amz-Security-Token: AQoDYXdzEJr1234".to_owned(),
                true,
            ),
            ("R1", "Accept: application/json".to_owned(), false),
            (
                "R2",
                "https://h.test/v1?api_key=abc123xyz&b=1".to_owned(),
                true,
            ),
            ("R2", "https://h.test/v1?page=2&b=1".to_owned(), false),
            (
                "R3",
                r#"{"refresh_token":"rt-abc-123456"}"#.to_owned(),
                true,
            ),
            ("R3", r#"{"max_tokens":4096}"#.to_owned(), false),
            ("R4", format!("sk-ant-{body}"), true),
            (
                "R4",
                "risk-assessment-of-the-whole-project-plan".to_owned(),
                false,
            ),
            (
                "R5",
                format!("{CLIENT_TOKEN_PREFIX}cc_{}", &body[..32]),
                true,
            ),
            ("R5", "the pd_ prefix alone".to_owned(), false),
            ("R6", format!("xoxb-{}", "1234567890-abc"), true),
            ("R6", "xoxb-short".to_owned(), false),
            ("R6", format!("AKIA{}", "ABCDEFGHIJKLMNOP"), true),
            (
                "R7",
                "eyJhbGciOiJIUzI1.eyJzdWIiOiIxMjM0.SflKxwRJSMeKKF2QT4".to_owned(),
                true,
            ),
            ("R7", "eyJ.only.two".to_owned(), false),
            (
                "R8",
                "-----BEGIN PRIVATE KEY-----\nMIIEvQ\n-----END PRIVATE KEY-----".to_owned(),
                true,
            ),
            (
                "R8",
                "-----BEGIN CERTIFICATE-----\nMIIEvQ\n-----END CERTIFICATE-----".to_owned(),
                false,
            ),
            (
                "R10",
                "https://relay.acme.dev/t/AbCdEfGhIjKlMnOpQrStUvWx/v1/models".to_owned(),
                true,
            ),
            (
                "R10",
                "https://relay.acme.dev/v1/chat/completions".to_owned(),
                false,
            ),
            ("R9", "echo relay-9f8e7d6c5b4a done".to_owned(), true),
        ];
        let needles = vec!["relay-9f8e7d6c5b4a".to_owned()];
        for (rule, text, secret) in cases {
            let out = redact_secrets(&text, &needles);
            assert_eq!(out != text, secret, "{rule}: {text} -> {out}");
        }
        let out = redact_secrets(
            "https://relay.acme.dev/t/AbCdEfGhIjKlMnOpQrStUvWx/v1/models?key=1",
            &[],
        );
        assert_eq!(out, "https://relay.acme.dev/t/…/v1/models?key=••••••");
        // R9: raw, URL-encoded and JSON-escaped forms, floor of 8 characters.
        let secret = "relay/9f8e+7d6c\"5b".to_owned();
        let encoded: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
        let text = format!("raw {secret} enc {encoded} short abc1234");
        let out = redact_secrets(&text, &[secret, "abc1234".to_owned()]);
        assert!(
            out.contains("abc1234") && !out.contains("relay") && !out.contains("9f8e"),
            "{out}"
        );
        // R5 reads the constant that mints the tokens.
        assert!(token_shapes().contains(&regex::escape(CLIENT_TOKEN_PREFIX)));
        assert_eq!(compile_patterns().len(), PATTERN_COUNT);
    }

    #[test]
    fn a_secret_after_the_cut_point_of_a_300_kib_body_and_random_secrets_never_survive() {
        let mut seed = 7_u64;
        let secret = format!("relay-{}", random_token(&mut seed, 24, TOKEN_CHARS));
        let mut body = "a ".repeat(150 * 1024);
        body.push_str(&secret);
        body.push_str(" tail");
        let out = redact_secrets(&body, std::slice::from_ref(&secret));
        // Redaction runs on the whole text, so cutting the result anywhere leaves no fragment.
        assert!(!out.contains(&secret[..12]));
        for _ in 0..200 {
            let key_len = 20 + usize::try_from(seed % 20).unwrap_or(0);
            let key = random_token(&mut seed, key_len, TOKEN_CHARS);
            let path_len = 24 + usize::try_from(seed % 10).unwrap_or(0);
            let path_token = random_token(&mut seed, path_len, TOKEN_CHARS);
            let text = format!(
                "error for https://h.test/t/{path_token}/v1: upstream echoed {key} and sk-{key}"
            );
            let out = redact_secrets(&text, std::slice::from_ref(&key));
            assert!(
                !out.contains(&key) && !out.contains(&path_token),
                "{text} -> {out}"
            );
        }
    }
}

#[test]
fn mask_secrets_masks_longer_values_first_and_skips_empty() {
    assert_eq!(
        mask_secrets(
            "token=abcd1234&other=abcd",
            &["abcd".into(), "abcd1234".into()]
        ),
        format!("token={REDACTED}&other={REDACTED}")
    );
    assert_eq!(mask_secrets("plain", &[String::new()]), "plain");
}
