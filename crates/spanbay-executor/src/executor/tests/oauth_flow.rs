//! The OAuth 2.0 token of a send: a 401 drops the token and retries once with a new one, an
//! expired card token is refreshed with its refresh token, concurrent sends share one fetch, and
//! the extra parameters reach the token request without replacing the grant's own fields.

use std::sync::{Arc, Mutex};

use super::support::{TestServer, TestServerAction};
use crate::executor::{
    ExecutorOptions, OAuthTokenSource, StoredOAuthTokens, TokenGrant, execute_request,
};
use spanbay_model::{AuthConfig, KeyValue, OAuth2GrantType, Request};

fn respond(status: &str, body: &str) -> TestServerAction {
    TestServerAction::Respond(format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ))
}

fn grant(token: &str) -> TestServerAction {
    respond(
        "200 OK",
        &format!(r#"{{"access_token":"{token}","expires_in":3600,"refresh_token":"r2"}}"#),
    )
}

fn row(key: &str, value: &str, enabled: bool) -> KeyValue {
    KeyValue {
        key: key.to_owned(),
        value: value.to_owned(),
        enabled,
        description: None,
    }
}

/// A request to `api` whose OAuth 2.0 token comes from `idp`; `client_id` keeps the in-memory
/// token cache of one test apart from another's.
fn oauth_request(api: &str, idp: &str, grant: OAuth2GrantType, client_id: &str) -> Request {
    let mut request =
        Request::new("req_oauth", "ws_oauth", "OAuth", format!("{api}/data")).unwrap();
    request.auth = AuthConfig::OAuth2 {
        grant_type: grant,
        access_token_url: format!("{idp}/token"),
        client_id: client_id.to_owned(),
        client_secret_ref: "secret".into(),
        username: String::new(),
        password_ref: String::new(),
        scope: String::new(),
        authorization_url: String::new(),
        redirect_port: None,
        client_auth: Default::default(),
        extra_params: vec![
            row("audience", "https://api.example", true),
            row("resource", "ignored", false),
            row("client_id", "evil", true),
        ],
    };
    request
}

fn bearer(request: &str) -> Option<&str> {
    request
        .lines()
        .find_map(|line| line.strip_prefix("authorization: bearer "))
        .map(str::trim)
}

#[tokio::test]
async fn a_401_drops_the_fetched_token_and_retries_once_with_a_new_one() {
    let idp = TestServer::spawn_sequence(vec![grant("tok-a"), grant("tok-b")]);
    let api = TestServer::spawn_sequence(vec![
        respond("401 Unauthorized", "{}"),
        respond("200 OK", "{}"),
    ]);
    let request = oauth_request(
        &api.base_url,
        &idp.base_url,
        OAuth2GrantType::ClientCredentials,
        "client-401",
    );
    let result = execute_request(&request, None, ExecutorOptions::default())
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);
    let sent = api.received_requests(2);
    assert_eq!(
        (bearer(&sent[0]), bearer(&sent[1])),
        (Some("tok-a"), Some("tok-b"))
    );
    let token_requests = idp.received_requests(2);
    let body = token_requests[0]
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or_default();
    // The enabled extra parameter is sent; a disabled one and one that names a grant field are not.
    assert!(
        body.contains("audience=https%3a%2f%2fapi.example"),
        "{body}"
    );
    assert!(
        !body.contains("resource=") && !body.contains("evil"),
        "{body}"
    );
    assert_eq!(body.matches("client_id=").count(), 1, "{body}");
}

/// The card's store: one stored token, a refresh token, and what the executor did with them.
#[derive(Default)]
struct CardStore {
    valid: Mutex<Option<String>>,
    refresh: Mutex<Option<String>>,
    stored: Mutex<Vec<String>>,
    invalidated: Mutex<Vec<String>>,
    sent: Mutex<Vec<String>>,
}

impl StoredOAuthTokens for CardStore {
    fn valid_access_token(&self, _key: &str) -> Option<String> {
        self.valid.lock().unwrap().clone()
    }
    fn refresh_token(&self, _key: &str) -> Option<String> {
        self.refresh.lock().unwrap().clone()
    }
    fn store_grant(&self, _key: &str, grant: &TokenGrant) -> bool {
        self.stored.lock().unwrap().push(grant.access_token.clone());
        *self.valid.lock().unwrap() = Some(grant.access_token.clone());
        true
    }
    fn invalidate(&self, _key: &str, token: &str) {
        self.invalidated.lock().unwrap().push(token.to_owned());
        let mut valid = self.valid.lock().unwrap();
        if valid.as_deref() == Some(token) {
            *valid = None;
        }
    }
    fn sent(&self, token: &str) {
        self.sent.lock().unwrap().push(token.to_owned());
    }
}

fn card_options(store: &Arc<CardStore>) -> ExecutorOptions {
    ExecutorOptions {
        oauth_tokens: OAuthTokenSource::new(Arc::clone(store) as Arc<dyn StoredOAuthTokens>),
        ..ExecutorOptions::default()
    }
}

/// A 401 to a card token that cannot be renewed (no refresh token) leaves the token as it was:
/// the 401 stands and the next send still carries it.
#[tokio::test]
async fn a_401_keeps_a_card_token_that_has_no_refresh_token() {
    let api = TestServer::spawn_sequence(vec![
        respond("401 Unauthorized", "{}"),
        respond("200 OK", "{}"),
    ]);
    let store = Arc::new(CardStore::default());
    *store.valid.lock().unwrap() = Some("card".to_owned());
    let request = oauth_request(
        &api.base_url,
        "http://127.0.0.1:9",
        OAuth2GrantType::AuthorizationCodePkce,
        "client-keep",
    );
    let refused = execute_request(&request, None, card_options(&store))
        .await
        .unwrap();
    assert_eq!(refused.status_code, 401);
    assert!(store.invalidated.lock().unwrap().is_empty());
    assert_eq!(store.valid.lock().unwrap().as_deref(), Some("card"));
    let next = execute_request(&request, None, card_options(&store))
        .await
        .unwrap();
    assert_eq!(next.status_code, 200);
    let sent = api.received_requests(2);
    assert!(sent.iter().all(|request| bearer(request) == Some("card")));
}

/// Every token a send attaches is reported, also a fetched one that only the in-memory cache
/// keeps and the one a later send takes from that cache.
#[tokio::test]
async fn fetched_and_cached_tokens_are_reported_as_sent() {
    let idp = TestServer::spawn_sequence(vec![grant("tok-x"), grant("tok-y")]);
    let api = TestServer::spawn_sequence(vec![
        respond("401 Unauthorized", "{}"),
        respond("200 OK", "{}"),
        respond("200 OK", "{}"),
    ]);
    let request = oauth_request(
        &api.base_url,
        &idp.base_url,
        OAuth2GrantType::ClientCredentials,
        "client-report",
    );
    let first = Arc::new(CardStore::default());
    let result = execute_request(&request, None, card_options(&first))
        .await
        .unwrap();
    assert_eq!(result.status_code, 200);
    assert_eq!(*first.sent.lock().unwrap(), ["tok-x", "tok-y"]);
    let second = Arc::new(CardStore::default());
    execute_request(&request, None, card_options(&second))
        .await
        .unwrap();
    assert_eq!(*second.sent.lock().unwrap(), ["tok-y"]);
}

#[tokio::test]
async fn a_refused_card_token_is_refreshed_with_the_stored_refresh_token_and_stored() {
    let idp = TestServer::spawn_sequence(vec![grant("fresh")]);
    let api = TestServer::spawn_sequence(vec![
        respond("401 Unauthorized", "{}"),
        respond("200 OK", "{}"),
    ]);
    let store = Arc::new(CardStore::default());
    *store.valid.lock().unwrap() = Some("stale".to_owned());
    *store.refresh.lock().unwrap() = Some("r1".to_owned());
    let request = oauth_request(
        &api.base_url,
        &idp.base_url,
        OAuth2GrantType::AuthorizationCodePkce,
        "client-refresh",
    );
    let options = ExecutorOptions {
        oauth_tokens: OAuthTokenSource::new(Arc::clone(&store) as Arc<dyn StoredOAuthTokens>),
        ..ExecutorOptions::default()
    };
    let result = execute_request(&request, None, options).await.unwrap();
    assert_eq!(result.status_code, 200);
    let sent = api.received_requests(2);
    assert_eq!(
        (bearer(&sent[0]), bearer(&sent[1])),
        (Some("stale"), Some("fresh"))
    );
    assert_eq!(*store.invalidated.lock().unwrap(), ["stale"]);
    assert_eq!(*store.stored.lock().unwrap(), ["fresh"]);
    let refresh = idp.received_request();
    assert!(
        refresh.contains("grant_type=refresh_token&refresh_token=r1&audience="),
        "{refresh}"
    );

    // Without a refresh token the interactive grant has no way to a token: the send says so.
    let empty = Arc::new(CardStore::default());
    let options = ExecutorOptions {
        oauth_tokens: OAuthTokenSource::new(empty as Arc<dyn StoredOAuthTokens>),
        ..ExecutorOptions::default()
    };
    let error = execute_request(&request, None, options).await.unwrap_err();
    assert!(error.to_string().contains("token card"), "{error}");
}

#[tokio::test]
async fn concurrent_sends_of_one_configuration_share_one_token_request() {
    // One grant only: a second token request would find the server gone and fail its send.
    let idp = TestServer::spawn_sequence(vec![grant("shared")]);
    let api = TestServer::spawn_sequence(vec![respond("200 OK", "{}"), respond("200 OK", "{}")]);
    let request = oauth_request(
        &api.base_url,
        &idp.base_url,
        OAuth2GrantType::ClientCredentials,
        "client-flight",
    );
    let (first, second) = tokio::join!(
        execute_request(&request, None, ExecutorOptions::default()),
        execute_request(&request, None, ExecutorOptions::default()),
    );
    assert_eq!(
        (first.unwrap().status_code, second.unwrap().status_code),
        (200, 200)
    );
    let sent = api.received_requests(2);
    assert!(sent.iter().all(|request| bearer(request) == Some("shared")));
}
