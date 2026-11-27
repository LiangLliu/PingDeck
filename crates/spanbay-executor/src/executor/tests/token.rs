//! The OAuth 2.0 token request: it carries credentials, so it never follows a redirect, reads a
//! bounded body and connects only where its guard allows.

use std::net::IpAddr;
use std::sync::Arc;

use super::support::TestServer;
use crate::executor::types::{AuxiliaryNetwork, ExecutionError, ResolveGuard};
use crate::executor::{OAuthClient, token_request};
use spanbay_model::OAuth2ClientAuth;

const FORM: &[(&str, &str)] = &[("grant_type", "client_credentials")];
const CLIENT: OAuthClient<'static> = OAuthClient {
    auth: OAuth2ClientAuth::Body,
    id: "app",
    secret: "hunter2",
};

#[tokio::test]
async fn token_request_refuses_redirects_oversized_bodies_and_guarded_hosts() {
    use super::support::TestServerAction;
    let target = TestServer::spawn("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}");
    let over_body = "a".repeat(256 * 1024 + 1);
    let ok_body = r#"{"access_token":"tok","expires_in":60}"#;
    let idp = TestServer::spawn_sequence(vec![
        TestServerAction::Respond(format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: {}/steal\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            target.base_url
        )),
        TestServerAction::Respond(format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{over_body}",
            over_body.len()
        )),
        TestServerAction::Respond(format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{ok_body}",
            ok_body.len()
        )),
    ]);
    let token_url = format!("{}/token", idp.base_url);

    let redir_err = token_request(
        &AuxiliaryNetwork::default(),
        &ResolveGuard::default(),
        &token_url,
        FORM,
        CLIENT,
    )
    .await
    .err()
    .unwrap();
    assert!(
        matches!(&redir_err, ExecutionError::OAuth2Token { message } if message.contains("redirect"))
    );
    assert!(target.received_request_rx.try_recv().is_err());

    let over_err = token_request(
        &AuxiliaryNetwork::default(),
        &ResolveGuard::default(),
        &token_url,
        FORM,
        CLIENT,
    )
    .await
    .err()
    .unwrap();
    assert!(
        matches!(&over_err, ExecutionError::OAuth2Token { message } if message.contains("256 KiB"))
    );

    let grant = token_request(
        &AuxiliaryNetwork::default(),
        &ResolveGuard::default(),
        &token_url,
        FORM,
        CLIENT,
    )
    .await
    .unwrap();
    assert_eq!(
        (grant.access_token.as_str(), grant.expires_in),
        ("tok", Some(60))
    );

    let meta_lit = token_request(
        &AuxiliaryNetwork::default(),
        &ResolveGuard::default(),
        "http://169.254.169.254/latest/token",
        FORM,
        CLIENT,
    )
    .await
    .err()
    .unwrap();
    assert!(
        matches!(&meta_lit, ExecutionError::GuardRefused { host } if host == "169.254.169.254")
    );

    let meta_dns = ResolveGuard {
        lookup: Some(Arc::new(|_| {
            vec!["169.254.169.254".parse::<IpAddr>().unwrap()]
        })),
        ..ResolveGuard::default()
    };
    let dns_err = token_request(
        &AuxiliaryNetwork::default(),
        &meta_dns,
        "http://idp.test:9/token",
        FORM,
        CLIENT,
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(&dns_err, ExecutionError::GuardRefused { host } if host == "idp.test"));

    let strict = ResolveGuard::new(Some(Arc::new(Vec::new())));
    let strict_err = token_request(
        &AuxiliaryNetwork::default(),
        &strict,
        &token_url,
        FORM,
        CLIENT,
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(strict_err, ExecutionError::GuardRefused { .. }));
}

/// The client credentials go either in the form body or in a Basic header whose parts are
/// form-encoded first (RFC 6749 §2.3.1); the Basic form leaves them out of the body.
#[tokio::test]
async fn token_request_sends_client_credentials_as_form_fields_or_basic_header() {
    let grant = r#"{"access_token":"tok"}"#;
    let reply = || {
        super::support::TestServerAction::Respond(format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{grant}",
            grant.len()
        ))
    };
    let idp = TestServer::spawn_sequence(vec![reply(), reply()]);
    let token_url = format!("{}/token", idp.base_url);
    for auth in [OAuth2ClientAuth::Body, OAuth2ClientAuth::Basic] {
        let client = OAuthClient {
            auth,
            id: "my app",
            secret: "s:cr&t",
        };
        token_request(
            &AuxiliaryNetwork::default(),
            &ResolveGuard::default(),
            &token_url,
            FORM,
            client,
        )
        .await
        .unwrap_or_else(|_| panic!("{auth:?} grant"));
    }
    let requests = idp.received_request_bytes_many(2);
    let body = String::from_utf8_lossy(&requests[0]);
    assert!(
        body.ends_with("grant_type=client_credentials&client_id=my+app&client_secret=s%3Acr%26t")
    );
    assert!(!body.to_ascii_lowercase().contains("authorization:"));
    let basic = String::from_utf8_lossy(&requests[1]);
    // base64("my+app:s%3Acr%26t")
    assert!(
        basic.contains("authorization: Basic bXkrYXBwOnMlM0FjciUyNnQ=\r\n"),
        "{basic}"
    );
    assert!(basic.ends_with("\r\n\r\ngrant_type=client_credentials"));
}
