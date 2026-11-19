//! Token keys, the token rows, PKCE values and the loopback redirect.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::*;
use pingdeck_base::secrets::{SecretHint, SecretStore, redact_secrets};

/// The token cache only needs get/put/delete: an in-memory map is enough here, and it keeps
/// this crate free of the store (which owns the SQLite implementation).
#[derive(Default)]
struct MapStore(std::sync::Mutex<std::collections::HashMap<String, String>>);

impl SecretStore for MapStore {
    fn get(&self, secret_ref: &str) -> Result<Option<String>, pingdeck_base::error::AppError> {
        Ok(self.0.lock().expect("store lock").get(secret_ref).cloned())
    }
    fn put(
        &self,
        secret_ref: &str,
        plaintext: &str,
    ) -> Result<SecretHint, pingdeck_base::error::AppError> {
        self.0
            .lock()
            .expect("store lock")
            .insert(secret_ref.to_owned(), plaintext.to_owned());
        Ok(SecretHint {
            secret_ref: secret_ref.to_owned(),
            has_secret: true,
            hint: pingdeck_base::secrets::mask_secret_hint(plaintext),
            updated_at_ms: pingdeck_base::time::now_ms(),
        })
    }
    fn delete(&self, secret_ref: &str) -> Result<bool, pingdeck_base::error::AppError> {
        Ok(self
            .0
            .lock()
            .expect("store lock")
            .remove(secret_ref)
            .is_some())
    }
    fn hint(&self, secret_ref: &str) -> Result<SecretHint, pingdeck_base::error::AppError> {
        let map = self.0.lock().expect("store lock");
        Ok(match map.get(secret_ref) {
            Some(value) => SecretHint {
                secret_ref: secret_ref.to_owned(),
                has_secret: true,
                hint: pingdeck_base::secrets::mask_secret_hint(value),
                updated_at_ms: 0,
            },
            None => SecretHint {
                secret_ref: secret_ref.to_owned(),
                has_secret: false,
                hint: String::new(),
                updated_at_ms: 0,
            },
        })
    }
    fn active_redaction_needles(&self) -> Vec<String> {
        self.0
            .lock()
            .expect("store lock")
            .values()
            .filter(|value| value.chars().count() >= pingdeck_base::secrets::MIN_REDACTION_CHARS)
            .cloned()
            .collect()
    }
}

fn secrets() -> Arc<dyn SecretStore> {
    Arc::new(MapStore::default())
}

fn grant(access: &str, refresh: Option<&str>, expires_in: Option<u64>) -> TokenGrant {
    TokenGrant {
        access_token: access.into(),
        token_type: "Bearer".into(),
        scope: "read".into(),
        expires_in,
        refresh_token: refresh.map(Into::into),
    }
}

#[test]
fn a_key_follows_the_workspace_grant_and_resolved_configuration() {
    let key =
        |ws, grant, url, client, scope, user| token_key(ws, grant, url, client, scope, user, &[]);
    let base = key(
        "ws",
        OAuth2GrantType::ClientCredentials,
        "https://a/t",
        "c",
        "s",
        "",
    );
    assert_eq!(
        base,
        key(
            "ws",
            OAuth2GrantType::ClientCredentials,
            "https://a/t",
            "c",
            "s",
            ""
        )
    );
    assert!(base.starts_with("oauth.ws."));
    for other in [
        key(
            "ws2",
            OAuth2GrantType::ClientCredentials,
            "https://a/t",
            "c",
            "s",
            "",
        ),
        key("ws", OAuth2GrantType::Password, "https://a/t", "c", "s", ""),
        key(
            "ws",
            OAuth2GrantType::ClientCredentials,
            "https://b/t",
            "c",
            "s",
            "",
        ),
        key(
            "ws",
            OAuth2GrantType::ClientCredentials,
            "https://a/t",
            "d",
            "s",
            "",
        ),
        key(
            "ws",
            OAuth2GrantType::ClientCredentials,
            "https://a/t",
            "c",
            "t",
            "",
        ),
        key(
            "ws",
            OAuth2GrantType::ClientCredentials,
            "https://a/t",
            "c",
            "s",
            "u",
        ),
    ] {
        assert_ne!(base, other);
    }
}

#[test]
fn a_stored_token_reports_its_facts_and_never_its_text() {
    let secrets = secrets();
    let token = "access-token-value-ABCD";
    let meta = store_grant(
        secrets.as_ref(),
        "oauth.ws.k",
        &grant(token, Some("refresh-token-value-1"), Some(3600)),
        1_000,
    )
    .expect("stored");
    assert_eq!(meta.last4, "ABCD");
    assert_eq!(meta.expires_at_ms, Some(3_601_000));
    assert!(
        !serde_json::to_string(&meta)
            .expect("json")
            .contains("access-token")
    );
    assert_eq!(
        load_meta(secrets.as_ref(), "oauth.ws.k").expect("read"),
        Some(meta)
    );
    assert!(has_refresh(secrets.as_ref(), "oauth.ws.k").expect("read"));
    // Redaction knows every row without being told.
    let leaked = format!("sent {token} and refresh-token-value-1");
    let clean = redact_secrets(&leaked, &secrets.active_redaction_needles());
    assert!(!clean.contains(token) && !clean.contains("refresh-token-value-1"));
}

#[test]
fn a_short_token_is_masked_and_a_missing_lifetime_has_no_expiry() {
    let secrets = secrets();
    let meta = store_grant(secrets.as_ref(), "k", &grant("short", None, None), 5).expect("stored");
    assert_eq!(meta.last4, "••••");
    assert_eq!(meta.expires_at_ms, None);
    assert!(!has_refresh(secrets.as_ref(), "k").expect("read"));
}

#[test]
fn a_new_grant_without_a_refresh_token_keeps_the_stored_one() {
    let secrets = secrets();
    store_grant(
        secrets.as_ref(),
        "k",
        &grant("access-one-0000", Some("refresh-one-0000"), None),
        0,
    )
    .expect("stored");
    store_grant(
        secrets.as_ref(),
        "k",
        &grant("access-two-0000", None, None),
        0,
    )
    .expect("stored");
    assert_eq!(
        access_token(secrets.as_ref(), "k")
            .expect("read")
            .as_deref(),
        Some("access-two-0000")
    );
    assert_eq!(
        refresh_token(secrets.as_ref(), "k")
            .expect("read")
            .as_deref(),
        Some("refresh-one-0000")
    );
}

#[test]
fn clear_removes_every_row() {
    let secrets = secrets();
    store_grant(
        secrets.as_ref(),
        "k",
        &grant("access-one-0000", Some("refresh-one-0000"), None),
        0,
    )
    .expect("stored");
    clear(secrets.as_ref(), "k").expect("cleared");
    assert_eq!(load_meta(secrets.as_ref(), "k").expect("read"), None);
    assert_eq!(access_token(secrets.as_ref(), "k").expect("read"), None);
    assert_eq!(refresh_token(secrets.as_ref(), "k").expect("read"), None);
}

#[test]
fn the_executor_sees_only_unexpired_tokens() {
    let secrets = secrets();
    let source = SecretTokens(Arc::clone(&secrets));
    let now = pingdeck_base::time::now_ms();
    store_grant(
        secrets.as_ref(),
        "live",
        &grant("live-token-0000", None, Some(3600)),
        now,
    )
    .expect("stored");
    store_grant(
        secrets.as_ref(),
        "dead",
        &grant("dead-token-0000", None, Some(10)),
        now,
    )
    .expect("stored");
    store_grant(
        secrets.as_ref(),
        "forever",
        &grant("forever-token-00", None, None),
        now,
    )
    .expect("stored");
    // `dead` ends within the 30 s skew: treated as expired.
    assert_eq!(
        source.valid_access_token("live").as_deref(),
        Some("live-token-0000")
    );
    assert_eq!(source.valid_access_token("dead"), None);
    assert_eq!(
        source.valid_access_token("forever").as_deref(),
        Some("forever-token-00")
    );
    assert_eq!(source.valid_access_token("absent"), None);
}

/// A refused token is marked expired only while it is still the stored one (a token another send
/// refreshed meanwhile stays); its refresh token is kept for the next send.
#[test]
fn invalidating_marks_only_the_refused_token_expired() {
    let secrets = secrets();
    let source = SecretTokens(Arc::clone(&secrets));
    let now = pingdeck_base::time::now_ms();
    store_grant(
        secrets.as_ref(),
        "k",
        &grant("current-token-000", Some("refresh-1"), Some(3600)),
        now,
    )
    .expect("stored");
    source.invalidate("k", "older-token-0000");
    assert_eq!(
        source.valid_access_token("k").as_deref(),
        Some("current-token-000")
    );
    source.invalidate("k", "current-token-000");
    assert_eq!(source.valid_access_token("k"), None);
    assert_eq!(source.refresh_token("k").as_deref(), Some("refresh-1"));
    assert!(source.store_grant("k", &grant("next-token-00000", None, Some(3600))));
    assert_eq!(
        source.valid_access_token("k").as_deref(),
        Some("next-token-00000")
    );
}

/// Extra parameters are part of the key (another audience is another token); none keeps it.
#[test]
fn extra_parameters_change_the_key_and_reserved_names_are_dropped() {
    let key = |extra: &[(String, String)]| {
        token_key(
            "ws",
            OAuth2GrantType::ClientCredentials,
            "https://a/t",
            "c",
            "s",
            "",
            extra,
        )
    };
    let audience = vec![("audience".to_owned(), "https://api".to_owned())];
    assert_ne!(key(&[]), key(&audience));
    let row = |key: &str, enabled: bool| pingdeck_model::KeyValue {
        key: key.to_owned(),
        value: "v".to_owned(),
        enabled,
        description: None,
    };
    let resolved = resolve_extra_params(
        &[
            row("audience", true),
            row("off", false),
            row(" Client_ID ", true),
            row("", true),
        ],
        |text| Ok::<_, ()>(text.to_owned()),
    )
    .expect("resolved");
    assert_eq!(resolved, [("audience".to_owned(), "v".to_owned())]);
}

#[test]
fn pkce_values_follow_rfc_7636() {
    let pkce = Pkce::generate().expect("random source");
    assert_eq!(pkce.verifier.len(), 43);
    assert_eq!(
        pkce.challenge,
        URL_SAFE_NO_PAD.encode(crypto::sha256(pkce.verifier.as_bytes()))
    );
    let again = Pkce::generate().expect("random source");
    assert_ne!(pkce.verifier, again.verifier);
    assert_ne!(pkce.state, again.state);
}

/// Sends one raw request to the loopback listener and returns the status line.
async fn get(port: u16, target: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    stream
        .write_all(format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
        .await
        .expect("write");
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await.expect("read");
    reply.lines().next().unwrap_or_default().to_owned()
}

#[tokio::test]
async fn the_redirect_listener_binds_loopback_only_and_returns_the_code_of_its_state() {
    let redirect = LoopbackRedirect::bind(None).await.expect("bound");
    let local = redirect.listener.local_addr().expect("address");
    assert!(local.ip().is_loopback());
    let port = local.port();
    assert_eq!(
        redirect.redirect_uri(),
        format!("http://127.0.0.1:{port}/callback")
    );
    let waiting = tokio::spawn(redirect.wait_for_code("good-state", Duration::from_secs(10), None));

    assert!(get(port, "/favicon.ico").await.contains("404"));
    assert!(
        get(port, "/callback?code=stolen&state=other")
            .await
            .contains("400")
    );
    assert!(
        get(port, "/callback?state=good-state")
            .await
            .contains("400")
    );
    assert!(
        get(port, "/callback?code=the-code&state=good-state")
            .await
            .contains("200")
    );
    assert_eq!(waiting.await.expect("task").expect("code"), "the-code");
}

#[tokio::test]
async fn a_refusal_from_the_server_is_an_error() {
    let redirect = LoopbackRedirect::bind(None).await.expect("bound");
    let port = redirect.listener.local_addr().expect("address").port();
    let waiting = tokio::spawn(redirect.wait_for_code("s", Duration::from_secs(10), None));
    get(
        port,
        "/callback?error=access_denied&error_description=No&state=s",
    )
    .await;
    let error = waiting.await.expect("task").expect_err("refused");
    assert!(error.message().contains("access_denied"));
}

#[tokio::test]
async fn an_idle_connection_does_not_stall_the_real_redirect() {
    let redirect = LoopbackRedirect::bind(None).await.expect("bound");
    let port = redirect.listener.local_addr().expect("address").port();
    let waiting = tokio::spawn(redirect.wait_for_code("s", Duration::from_secs(10), None));
    // A speculative preconnect that never sends its request.
    let mut idle = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    idle.write_all(b"G").await.expect("one byte");

    let status = tokio::time::timeout(
        Duration::from_secs(2),
        get(port, "/callback?code=c&state=s"),
    )
    .await
    .expect("answered while the idle connection is open");
    assert!(status.contains("200"));
    assert_eq!(waiting.await.expect("task").expect("code"), "c");
}

#[tokio::test]
async fn the_wait_ends_on_timeout_and_on_cancel() {
    let redirect = LoopbackRedirect::bind(None).await.expect("bound");
    let timed_out = redirect
        .wait_for_code("s", Duration::from_millis(40), None)
        .await
        .expect_err("timeout");
    assert!(timed_out.message().contains("in time"));

    let redirect = LoopbackRedirect::bind(None).await.expect("bound");
    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        trigger.cancel();
    });
    let cancelled = redirect
        .wait_for_code("s", Duration::from_secs(10), Some(&cancel))
        .await
        .expect_err("cancelled");
    assert!(cancelled.is_cancelled());
}

#[tokio::test]
async fn a_taken_port_is_reported() {
    let first = LoopbackRedirect::bind(None).await.expect("bound");
    let port = first.listener.local_addr().expect("address").port();
    assert!(LoopbackRedirect::bind(Some(port)).await.is_err());
}
