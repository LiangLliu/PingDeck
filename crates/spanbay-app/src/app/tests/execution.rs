//! Request execution, collection runner, script, OAuth and body-file validation tests.

use std::{collections::BTreeMap, fs, path::PathBuf, time::Duration};

use super::support::*;
use crate::app::execution_service::{
    apply_script_variables, persist_script_state, response_cookie_entries, script_input,
    store_response_cookies,
};
use crate::app::{
    AppState, EnvironmentVariableInput, ExecuteDraftRequestInput, ExecuteRequestInput,
    ExecutionContext, ExecutionMeta, RunCollectionInput, SaveEnvironmentInput,
    cookie_jar_list_impl, environment_save_impl, execution_cancel_impl, execution_context,
    globals_get_impl, history_get_impl, request_body_pick_file_from_path_impl,
    request_body_validate_file_path_with_context_impl, request_execute_draft_impl,
    request_execute_impl, request_save_impl, response_body_fetch_impl, run_collection_impl,
    workspace_create_impl,
};
use crate::error::AppError;
use crate::workspace::{
    Assertion, AuthConfig, Collection, Environment, EnvironmentVariable,
    ExecutionHistoryResultKind, Folder, FolderId, HistoryEntryId, HttpMethod, KeyValue,
    NetworkOptions, OAuth2GrantType, ProtocolKind, Request, RequestBody,
    RequestBodyFileValidationStatus, RequestId, VariableResolutionSource, WorkspaceId,
};

const OK_RESPONSE: &str = "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok";
const OK_JSON: &str = "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"ok\":true}";

fn state_with_workspaces(ids: &[&str]) -> AppState {
    let state = AppState::open_in_memory().expect("state should open");
    for id in ids {
        workspace_create_impl(&state, (*id).into(), (*id).into()).unwrap();
    }
    state
}

fn save_request(state: &AppState, request: &Request) {
    state.store().save_request(request).unwrap();
}

fn var_input(key: &str, value: &str, is_secret: bool) -> EnvironmentVariableInput {
    EnvironmentVariableInput {
        key: key.into(),
        value: Some(value.into()),
        is_secret,
        ..Default::default()
    }
}

fn execute_input(
    workspace_id: &str,
    request_id: &str,
    environment_id: Option<&str>,
    confirmed: Option<bool>,
) -> ExecuteRequestInput {
    ExecuteRequestInput {
        workspace_id: workspace_id.into(),
        request_id: request_id.into(),
        environment_id: environment_id.map(str::to_owned),
        confirmed,
    }
}

fn run_input(workspace_id: &str, collection_id: &str) -> RunCollectionInput {
    RunCollectionInput {
        workspace_id: workspace_id.into(),
        collection_id: collection_id.into(),
        ..Default::default()
    }
}

fn run_pre_script(
    source: &str,
    request: &Request,
    before: &ExecutionContext,
) -> (ExecutionContext, crate::scripts::ScriptOutput) {
    let output = crate::scripts::run_scripts(script_input(
        crate::scripts::ScriptPhase::PreRequest,
        vec![crate::scripts::ScriptSource {
            label: "test".into(),
            source: source.into(),
        }],
        request,
        None,
        before,
        &ExecutionMeta::default(),
        None,
        crate::scripts::ScriptLimits::default(),
    ));
    assert!(output.error.is_none(), "{:?}", output.error);
    let mut after = before.clone();
    apply_script_variables(&mut after, &output);
    (after, output)
}

#[test]
fn script_changes_and_no_op_writes_merge_or_conflict_atomically() {
    let state = state_with_workspaces(&["ws_scripts"]);
    let mut collection = Collection::new("col_scripts", "ws_scripts", "Original").unwrap();
    collection.variables = vec![EnvironmentVariable::plain("old", "before").unwrap()];
    let mut environment = Environment::new("env_scripts", "ws_scripts", "Original").unwrap();
    environment.variables = vec![EnvironmentVariable::plain("old", "before").unwrap()];
    let mut request = Request::new(
        "req_scripts",
        "ws_scripts",
        "Request",
        "https://example.test",
    )
    .unwrap();
    request.collection_id = Some(collection.id.clone());
    {
        let store = state.store();
        store.save_collection(&collection).unwrap();
        store.save_environment(&environment).unwrap();
        store
            .save_global_variables(
                "ws_scripts",
                &[EnvironmentVariable::plain("old", "before").unwrap()],
            )
            .unwrap();
    }
    let before = {
        let store = state.store();
        execution_context(&store, &request, Some("env_scripts")).unwrap()
    };
    let (after, output) = run_pre_script(
        r#"
            pm.collectionVariables.set("script", "collection");
            pm.environment.unset("old");
            pm.globals.set("script", "global");
        "#,
        &request,
        &before,
    );
    {
        let store = state.store();
        collection.name = "Edited collection".into();
        collection
            .variables
            .push(EnvironmentVariable::plain("concurrent", "collection").unwrap());
        environment.name = "Edited environment".into();
        environment
            .variables
            .push(EnvironmentVariable::plain("concurrent", "environment").unwrap());
        store.save_collection(&collection).unwrap();
        store.save_environment(&environment).unwrap();
        store
            .save_global_variables(
                "ws_scripts",
                &[
                    EnvironmentVariable::plain("old", "before").unwrap(),
                    EnvironmentVariable::plain("concurrent", "global").unwrap(),
                ],
            )
            .unwrap();
    }
    persist_script_state(&state, &request, &before, &after, &output.mutations).unwrap();
    {
        let store = state.store();
        let saved_col = store
            .get_collection(&collection.workspace_id, &collection.id)
            .unwrap()
            .unwrap();
        let saved_env = store
            .get_environment(&environment.workspace_id, &environment.id)
            .unwrap()
            .unwrap();
        let globals = store.list_global_variables("ws_scripts").unwrap();
        assert_eq!(saved_col.name, "Edited collection");
        assert_eq!(saved_env.name, "Edited environment");
        assert!(saved_col.variables.iter().any(|v| v.key == "script"));
        assert!(saved_col.variables.iter().any(|v| v.key == "concurrent"));
        assert!(!saved_env.variables.iter().any(|v| v.key == "old"));
        assert!(saved_env.variables.iter().any(|v| v.key == "concurrent"));
        assert!(globals.iter().any(|v| v.key == "script"));
        assert!(globals.iter().any(|v| v.key == "concurrent"));
    }
    let mut conflicting = before.clone();
    conflicting.globals[0] = EnvironmentVariable::plain("old", "script").unwrap();
    let mut conflicting_mutations = crate::scripts::ScriptMutations::default();
    conflicting_mutations.globals.keys.insert("old".into());
    state
        .store()
        .save_global_variables(
            "ws_scripts",
            &[EnvironmentVariable::plain("old", "newer").unwrap()],
        )
        .unwrap();
    let conflict = persist_script_state(
        &state,
        &request,
        &before,
        &conflicting,
        &conflicting_mutations,
    )
    .unwrap_err();
    assert!(
        matches!(conflict, AppError::Conflict { .. })
            && conflict.message().contains("changed concurrently")
    );
    assert_eq!(
        globals_get_impl(&state, "ws_scripts".into())
            .unwrap()
            .variables[0]
            .value
            .as_deref(),
        Some("newer")
    );

    state
        .store()
        .delete_collection(&collection.workspace_id, &collection.id)
        .unwrap();
    persist_script_state(
        &state,
        &request,
        &before,
        &before,
        &crate::scripts::ScriptMutations::default(),
    )
    .unwrap();
    let deleted =
        persist_script_state(&state, &request, &before, &after, &output.mutations).unwrap_err();
    assert!(matches!(deleted, AppError::NotFound { .. }) && deleted.message().contains("deleted"));

    for scope in ["environment", "collectionVariables", "globals"] {
        for operation in ["set", "unset", "clear"] {
            let state = state_with_workspaces(&["ws"]);
            let mut col = Collection::new("col", "ws", "Collection").unwrap();
            let mut env = Environment::new("env", "ws", "Environment").unwrap();
            let initial = if operation == "set" {
                vec![EnvironmentVariable::plain("key", "original").unwrap()]
            } else {
                Vec::new()
            };
            let set_scope = |state: &AppState,
                             col: &mut Collection,
                             env: &mut Environment,
                             vars: &[EnvironmentVariable]| {
                let store = state.store();
                match scope {
                    "environment" => {
                        env.variables = vars.to_vec();
                        store.save_environment(env).unwrap();
                    }
                    "collectionVariables" => {
                        col.variables = vars.to_vec();
                        store.save_collection(col).unwrap();
                    }
                    _ => store.save_global_variables("ws", vars).unwrap(),
                }
            };
            {
                let store = state.store();
                store.save_collection(&col).unwrap();
                store.save_environment(&env).unwrap();
            }
            set_scope(&state, &mut col, &mut env, &initial);
            let req = Request::new("req", "ws", "Request", "https://example.test")
                .unwrap()
                .with_collection("col")
                .unwrap();
            let before = {
                let store = state.store();
                execution_context(&store, &req, Some("env")).unwrap()
            };
            let write = match operation {
                "set" => format!(
                    "pm.{scope}.set('key', 'temporary'); pm.{scope}.set('key', 'original');"
                ),
                "unset" => format!("pm.{scope}.unset('key');"),
                _ => format!("pm.{scope}.clear();"),
            };
            let other_scope = if scope == "globals" {
                "collectionVariables"
            } else {
                "globals"
            };
            let (after, out) = run_pre_script(
                &format!("{write} pm.{other_scope}.set('other', 'script');"),
                &req,
                &before,
            );
            let concurrent = vec![EnvironmentVariable::plain("key", "newer").unwrap()];
            set_scope(&state, &mut col, &mut env, &concurrent);
            let error =
                persist_script_state(&state, &req, &before, &after, &out.mutations).unwrap_err();
            assert!(
                matches!(error, AppError::Conflict { .. })
                    && error.message().contains("changed concurrently")
            );
            let store = state.store();
            let saved_col = store
                .get_collection(&col.workspace_id, &col.id)
                .unwrap()
                .unwrap();
            let saved_env = store
                .get_environment(&env.workspace_id, &env.id)
                .unwrap()
                .unwrap();
            let saved_globals = store.list_global_variables("ws").unwrap();
            let (saved, other) = match scope {
                "environment" => (&saved_env.variables, &saved_globals),
                "collectionVariables" => (&saved_col.variables, &saved_globals),
                _ => (&saved_globals, &saved_col.variables),
            };
            assert_eq!(saved, &concurrent);
            assert!(other.is_empty());
        }
    }
}

#[tokio::test]
async fn request_execute_impl_resolves_env_traces_redirects_and_executes_graphql_oauth_and_drafts()
{
    let server = TestServer::spawn_sequence(vec![
        TestServerAction::Respond(
            "HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\n\r\n".into(),
        ),
        TestServerAction::Respond(OK_JSON.into()),
        TestServerAction::Respond(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 30\r\n\r\n{\"data\":{\"viewer\":{\"id\":\"1\"}}}".into(),
        ),
        TestServerAction::Respond(OK_JSON.into()),
        TestServerAction::Respond(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 66\r\n\r\n{\"access_token\":\"tok_123\",\"token_type\":\"Bearer\",\"expires_in\":3600}".into(),
        ),
        TestServerAction::Respond(OK_JSON.into()),
    ]);
    let state = state_with_workspaces(&["ws_billing"]);
    environment_save_impl(
        &state,
        SaveEnvironmentInput {
            id: "env_local".into(),
            workspace_id: "ws_billing".into(),
            name: "Local".into(),
            variables: vec![
                var_input("baseUrl", &server.base_url, false),
                var_input("page", "2", false),
                var_input("token", "top-secret-token", true),
                var_input("customerId", "cus_123", false),
                var_input("operationName", "AccountSummary", false),
            ],
            ..Default::default()
        },
    )
    .unwrap();

    let mut health = Request::new(
        "req_history",
        "ws_billing",
        "History request",
        "{{baseUrl}}/start?page={{page}}",
    )
    .unwrap();
    health.query_params = vec![KeyValue {
        key: "source".into(),
        value: "spanbay".into(),
        enabled: true,
        description: None,
    }];
    health.headers = vec![KeyValue {
        key: "x-token".into(),
        value: "{{token}}".into(),
        enabled: true,
        description: None,
    }];
    save_request(&state, &health);

    let response = request_execute_impl(
        &state,
        execute_input("ws_billing", "req_history", Some("env_local"), None),
        None,
    )
    .await
    .unwrap();
    assert_eq!((response.status_code, response.attempt_count), (200, 1));
    assert_eq!(response.body_text, "{\"ok\":true}");

    let chain = &response.call_chain;
    let page = chain.variables.iter().find(|v| v.name == "page").unwrap();
    assert_eq!((page.resolved_value.as_str(), page.secret), ("2", false));
    let tok = chain.variables.iter().find(|v| v.name == "token").unwrap();
    assert!(tok.secret && tok.resolved_value == "••••••••");
    assert_eq!(tok.source, VariableResolutionSource::Environment);
    assert_eq!(
        (
            chain.auth.auth_type.as_str(),
            chain.auth.applied_to.as_str()
        ),
        ("none", "None")
    );
    assert_eq!(chain.transport.protocol, "HTTP/1.1");
    assert!(!chain.transport.proxy_bypassed && chain.transport.proxy_url.is_none());
    let hop = &chain.transport.redirect_hops[0];
    assert_eq!(
        (hop.hop, hop.status_code, hop.location.as_deref()),
        (1, 302, Some("/final"))
    );
    assert!(hop.url.contains("/start"));
    let sent = response.sent_request.expect("sent request is captured");
    assert!(sent.url.ends_with("/final"));
    assert!(
        sent.headers
            .iter()
            .any(|h| h.key.eq_ignore_ascii_case("x-token") && h.value == "••••••")
    );

    let history = history_list(&state, "ws_billing".into(), Some(10), None).unwrap();
    assert_eq!(history.len(), 1);
    assert!(history[0].id.as_str().starts_with("hist_req_history_"));
    assert_eq!(
        (
            history[0].request_id.as_ref().map(|id| id.as_str()),
            history[0].environment_id.as_ref().map(|id| id.as_str()),
            history[0].status_code,
            history[0].response_body_preview.as_str(),
            history[0].response_body_truncated,
        ),
        (
            Some("req_history"),
            Some("env_local"),
            200,
            "{\"ok\":true}",
            false
        )
    );
    let detail = history_get_impl(
        &state,
        "ws_billing".into(),
        history[0].id.as_str().to_owned(),
    )
    .unwrap();
    assert_eq!(detail.request_snapshot.unwrap().id.as_str(), "req_history");

    let mut gql = Request::new(
        "req_account_summary",
        "ws_billing",
        "Account summary",
        format!("{}/graphql", server.base_url),
    )
    .unwrap();
    gql.protocol = ProtocolKind::Graphql;
    gql.method = HttpMethod::Post;
    gql.body = RequestBody::Graphql {
        query: "query {{operationName}}($customerId: ID!) { account(id: $customerId) { id } }"
            .into(),
        variables_json: Some("{\"customerId\":\"{{customerId}}\"}".into()),
        operation_name: Some("{{operationName}}".into()),
    };
    save_request(&state, &gql);
    let gql_res = request_execute_impl(
        &state,
        execute_input("ws_billing", "req_account_summary", Some("env_local"), None),
        None,
    )
    .await
    .unwrap();
    assert_eq!(gql_res.body_text, "{\"data\":{\"viewer\":{\"id\":\"1\"}}}");

    let draft_res = request_execute_draft_impl(
        &state,
        ExecuteDraftRequestInput {
            request: draft_request_input(
                "req_draft",
                "ws_billing",
                format!("{}/draft", server.base_url),
            ),
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(draft_res.status_code, 200);
    // The response names the history row it wrote.
    assert!(draft_res.history_saved);
    let draft_row = history_get_impl(
        &state,
        "ws_billing".into(),
        draft_res.history_id.clone().unwrap(),
    )
    .unwrap();
    assert_eq!(
        (draft_row.request_name.as_str(), draft_row.status_code),
        ("Draft", 200)
    );
    // The Mock "Try it" drawer sends without a history row of its own.
    let recorded = || {
        history_list(&state, "ws_billing".into(), Some(100), None)
            .unwrap()
            .len()
    };
    let before = recorded();
    let try_server = TestServer::spawn(OK_RESPONSE);
    let unrecorded = request_execute_draft_impl(
        &state,
        ExecuteDraftRequestInput {
            request: draft_request_input(
                "req_try",
                "ws_billing",
                format!("{}/draft", try_server.base_url),
            ),
            save_to_history: Some(false),
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!((unrecorded.status_code, recorded()), (200, before));
    assert_eq!(
        (unrecorded.history_id, unrecorded.history_saved),
        (None, false)
    );
    assert!(
        state
            .store()
            .get_request(
                &WorkspaceId::new("ws_billing").unwrap(),
                &RequestId::new("req_draft").unwrap(),
            )
            .unwrap()
            .is_none()
    );

    let mut oauth_req = Request::new(
        "req_oauth",
        "ws_billing",
        "OAuth request",
        format!("{}/resource", server.base_url),
    )
    .unwrap();
    oauth_req.auth = AuthConfig::OAuth2 {
        grant_type: OAuth2GrantType::ClientCredentials,
        access_token_url: format!("{}/token", server.base_url),
        client_id: "client".into(),
        client_secret_ref: "secret".into(),
        username: String::new(),
        password_ref: String::new(),
        scope: "read".into(),
        authorization_url: String::new(),
        redirect_port: None,
        client_auth: Default::default(),
        extra_params: Vec::new(),
    };
    save_request(&state, &oauth_req);
    let oauth_res = request_execute_impl(
        &state,
        execute_input("ws_billing", "req_oauth", None, None),
        None,
    )
    .await
    .unwrap();
    assert_eq!(oauth_res.status_code, 200);

    let received = server.received_requests(6);
    assert!(received[0].starts_with("get /start?page=2&source=spanbay http/1.1"));
    assert!(received[2].starts_with("post /graphql http/1.1"));
    assert!(received[2].contains(
        "\"query\":\"query accountsummary($customerid: id!) { account(id: $customerid) { id } }\""
    ));
    assert!(received[2].contains("\"variables\":{\"customerid\":\"cus_123\"}"));
    assert!(received[2].contains("\"operationname\":\"accountsummary\""));
    assert!(received[3].starts_with("get /draft http/1.1"));
    assert!(received[4].contains("grant_type=client_credentials"));
    assert!(
        received[4].contains("client_id=client") && received[4].contains("client_secret=secret")
    );
    assert!(received[5].contains("authorization: bearer tok_123"));

    let mut ws_req = Request::new(
        "req_failed_history",
        "ws_billing",
        "Unsupported realtime stream",
        "http://127.0.0.1:9/stream",
    )
    .unwrap();
    ws_req.protocol = ProtocolKind::WebSocket;
    save_request(&state, &ws_req);
    let error = request_execute_impl(
        &state,
        execute_input("ws_billing", "req_failed_history", None, None),
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AppError::Unsupported { .. }));
    assert!(
        error
            .message()
            .contains("only HTTP REST and GraphQL can be sent")
    );

    let all_history = history_list(&state, "ws_billing".into(), Some(10), None).unwrap();
    assert_eq!(all_history.len(), 5);
    let failed = all_history
        .iter()
        .find(|entry| entry.request_id.as_ref() == Some(&ws_req.id))
        .unwrap();
    assert!(failed.id.as_str().starts_with("hist_req_failed_history_"));
    assert_eq!(failed.result_kind, ExecutionHistoryResultKind::Failure);
    assert_eq!(
        (failed.status_code, failed.status_text.as_str()),
        (0, "Failed")
    );
    assert_eq!(failed.error_kind.as_deref(), Some("UnsupportedProtocol"));
    assert_eq!(failed.error_message.as_deref(), Some(error.message()));
    assert_eq!(
        all_history
            .iter()
            .filter(|entry| entry.request_id.is_none())
            .count(),
        1
    );
}

#[tokio::test]
async fn request_execute_impl_persists_and_replays_cookies_and_externalizes_large_bodies() {
    static BIG_RESPONSE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let big = "é".repeat(1024 * 1024);
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{big}",
            big.len()
        )
    });
    let server = TestServer::spawn_sequence(vec![
        TestServerAction::Respond(
            "HTTP/1.1 200 OK\r\n\
             Set-Cookie: session=abc123; Path=/account; HttpOnly; SameSite=Lax\r\n\
             Set-Cookie: theme=dark; Domain=127.0.0.1; Path=/prefs\r\n\
             Set-Cookie: foreign=skip; Domain=example.com; Path=/prefs\r\n\
             Content-Length: 0\r\n\r\n"
                .into(),
        ),
        TestServerAction::Respond("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".into()),
        TestServerAction::Respond(BIG_RESPONSE.as_str().into()),
    ]);
    let state = state_with_workspaces(&["ws_billing"]);
    let req = Request::new(
        "req_cookie_jar",
        "ws_billing",
        "Cookie jar source",
        format!("{}/account/profile", server.base_url),
    )
    .unwrap();
    save_request(&state, &req);

    let response = request_execute_impl(
        &state,
        execute_input("ws_billing", "req_cookie_jar", None, None),
        None,
    )
    .await
    .unwrap();
    let jar = cookie_jar_list_impl(&state, "ws_billing".into()).unwrap();
    assert_eq!((response.cookies.len(), jar.len()), (2, 2));
    assert!(response.cookies.iter().all(|c| c.name != "foreign"));
    assert_eq!(
        (
            jar[0].name.as_str(),
            jar[0].domain.as_str(),
            jar[0].path.as_str(),
            jar[0].host_only,
            jar[0].source_request_id.as_ref().map(|id| id.as_str()),
        ),
        (
            "session",
            "127.0.0.1",
            "/account",
            true,
            Some("req_cookie_jar")
        )
    );
    assert_eq!((jar[1].name.as_str(), jar[1].host_only), ("theme", false));

    request_execute_impl(
        &state,
        execute_input("ws_billing", "req_cookie_jar", None, None),
        None,
    )
    .await
    .unwrap();

    let redirect_req = Request::new(
        "req_redirect_cookie",
        "ws_billing",
        "Redirect cookie",
        "https://app.example.com/home",
    )
    .unwrap();
    let mut redirect_res = response;
    redirect_res.url = "https://app.example.com/home".into();
    redirect_res.cookies = vec![crate::executor::ExecutionCookie {
        name: "redir_session".into(),
        value: "xyz".into(),
        domain: Some("auth.example.com".into()),
        path: Some("/".into()),
        expires: None,
        max_age: None,
        secure: true,
        http_only: true,
        same_site: Some("Lax".into()),
        host_only: true,
    }];
    let entries = response_cookie_entries(&redirect_req, &redirect_res);
    store_response_cookies(
        &state,
        &redirect_req.workspace_id,
        &redirect_req.id,
        entries,
    )
    .unwrap();
    let updated_jar = cookie_jar_list_impl(&state, "ws_billing".into()).unwrap();
    let redir = updated_jar
        .iter()
        .find(|c| c.name == "redir_session")
        .unwrap();
    assert_eq!(
        (redir.domain.as_str(), redir.host_only),
        ("auth.example.com", true)
    );

    let big = "é".repeat(1024 * 1024);
    let big_res = request_execute_impl(
        &state,
        execute_input("ws_billing", "req_cookie_jar", None, None),
        None,
    )
    .await
    .unwrap();
    let requests = server.received_requests(3);
    assert!(requests[1].contains("cookie: session=abc123"));
    assert_eq!(big_res.body_size_bytes, big.len());
    assert_eq!(big_res.body_text.len(), crate::bodies::BODY_PREVIEW_BYTES);
    assert!(big.starts_with(&big_res.body_text) && big_res.body_base64.is_none());
    let handle = big_res.body_handle.expect("a large body gets a handle");
    assert_eq!(
        response_body_fetch_impl(&state, &handle).unwrap().as_ref(),
        big.as_bytes()
    );
    assert!(matches!(
        response_body_fetch_impl(&state, "body_unknown"),
        Err(AppError::NotFound { .. })
    ));
}

#[tokio::test]
async fn rq25_disabled_cookie_jar_keeps_response_cookies_out_of_the_jar() {
    let server = TestServer::spawn_sequence(vec![
        TestServerAction::Respond(
            "HTTP/1.1 200 OK\r\nSet-Cookie: session=abc123; Path=/\r\nContent-Length: 0\r\n\r\n"
                .into(),
        ),
        TestServerAction::Respond("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".into()),
    ]);
    let state = state_with_workspaces(&["ws_rq25"]);
    let mut req = Request::new(
        "req_rq25",
        "ws_rq25",
        "No jar",
        format!("{}/a", server.base_url),
    )
    .unwrap();
    req.network_options.send_cookies = false;
    save_request(&state, &req);
    let response = request_execute_impl(
        &state,
        execute_input("ws_rq25", "req_rq25", None, None),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        response.cookies.len(),
        1,
        "the response still shows its cookie"
    );
    assert!(
        cookie_jar_list_impl(&state, "ws_rq25".into())
            .unwrap()
            .is_empty()
    );

    req.network_options.send_cookies = true;
    save_request(&state, &req);
    request_execute_impl(
        &state,
        execute_input("ws_rq25", "req_rq25", None, None),
        None,
    )
    .await
    .unwrap();
    assert!(!server.received_requests(2)[1].contains("session=abc123"));
}

#[test]
fn request_body_file_pick_and_validation_enforce_precedence_and_workspace_ownership() {
    let picked =
        request_body_pick_file_from_path_impl(Some(PathBuf::from("/tmp/receipt.pdf"))).unwrap();
    assert!(picked.opened && picked.path.as_deref() == Some("/tmp/receipt.pdf"));
    let state = state_with_workspaces(&["ws_scope", "ws_other"]);
    let path = temp_path("scoped-validation.txt");
    fs::write(&path, "content").unwrap();
    let path_value = path.to_string_lossy().into_owned();
    let mut collection = Collection::new("col_scope", "ws_scope", "Scope").unwrap();
    collection.variables =
        vec![EnvironmentVariable::plain("path", "/missing/collection-file").unwrap()];
    let mut request =
        Request::new("req_scope", "ws_scope", "Upload", "https://api.example/u").unwrap();
    request.collection_id = Some(collection.id.clone());
    let mut env_scope = Environment::new("env_scope", "ws_scope", "Local").unwrap();
    env_scope.variables = vec![EnvironmentVariable::plain("path", &path_value).unwrap()];
    let req_other =
        Request::new("req_other", "ws_other", "Other", "https://api.example/u").unwrap();
    let env_other = Environment::new("env_other", "ws_other", "Other").unwrap();
    {
        let store = state.store();
        store.save_collection(&collection).unwrap();
        store.save_request(&request).unwrap();
        store.save_environment(&env_scope).unwrap();
        store
            .save_global_variables(
                "ws_scope",
                &[EnvironmentVariable::plain("path", &path_value).unwrap()],
            )
            .unwrap();
        store.save_request(&req_other).unwrap();
        store.save_environment(&env_other).unwrap();
    }

    let validate = |ws: &str, req_id: &str, env_id: Option<&str>| {
        request_body_validate_file_path_with_context_impl(
            &state,
            ws.into(),
            req_id.into(),
            env_id.map(str::to_owned),
            "{{path}}".into(),
        )
    };
    assert_eq!(
        validate("ws_scope", "req_scope", None).unwrap().status,
        RequestBodyFileValidationStatus::Missing
    );
    assert_eq!(
        validate("ws_scope", "req_scope", Some("env_scope"))
            .unwrap()
            .status,
        RequestBodyFileValidationStatus::Ready
    );
    collection.variables[0].enabled = false;
    state.store().save_collection(&collection).unwrap();
    assert_eq!(
        validate("ws_scope", "req_scope", None).unwrap().status,
        RequestBodyFileValidationStatus::Ready
    );
    fs::remove_file(path).unwrap();

    let cross_env = validate("ws_scope", "req_scope", Some("env_other")).unwrap_err();
    assert!(matches!(cross_env, AppError::NotFound { .. }));
    assert!(cross_env.message().contains("env_other"));
    let cross_req = validate("ws_scope", "req_other", None).unwrap_err();
    assert!(matches!(cross_req, AppError::NotFound { .. }));
    assert!(cross_req.message().contains("req_other"));
}

fn save_scope_collection(state: &AppState, base_url: &str) {
    workspace_create_impl(state, "ws_scope".into(), "Scope".into()).unwrap();
    let collection = Collection::new("col_scope", "ws_scope", "Scope").unwrap();
    let folder_a = Folder::new("fold_a", "col_scope", "A").unwrap();
    let mut folder_b = Folder::new("fold_b", "col_scope", "B").unwrap();
    folder_b.parent_folder_id = Some(folder_a.id.clone());
    let store = state.store();
    store.save_collection(&collection).unwrap();
    store.save_folder(&folder_a).unwrap();
    store.save_folder(&folder_b).unwrap();
    for (position, (id, folder)) in [
        ("r1", None),
        ("r2", Some("fold_a")),
        ("r3", Some("fold_b")),
        ("r4", None),
    ]
    .into_iter()
    .enumerate()
    {
        let mut request = Request::new(id, "ws_scope", id, format!("{base_url}/{id}")).unwrap();
        request.collection_id = Some(collection.id.clone());
        request.folder_id = folder.map(|f| FolderId::new(f).unwrap());
        request.position = i64::try_from(position).unwrap();
        store.save_request(&request).unwrap();
    }
}

fn scope_input(folder_id: Option<&str>, request_ids: Option<&[&str]>) -> RunCollectionInput {
    let mut input = run_input("ws_scope", "col_scope");
    input.folder_id = folder_id.map(str::to_owned);
    input.request_ids = request_ids.map(|ids| ids.iter().map(|id| (*id).to_owned()).collect());
    input
}

fn mutate_scope_request(state: &AppState, id: &str, f: impl FnOnce(&mut Request)) {
    let store = state.store();
    let mut req = store
        .get_request(
            &WorkspaceId::new("ws_scope").unwrap(),
            &RequestId::new(id).unwrap(),
        )
        .unwrap()
        .unwrap();
    f(&mut req);
    store.save_request(&req).unwrap();
}

#[tokio::test]
async fn run_collection_impl_handles_subtree_assertions_workflow_streaming_secrets_and_guard() {
    let server = TestServer::spawn_sequence(vec![
        TestServerAction::Respond(OK_RESPONSE.into()),
        TestServerAction::Respond(OK_RESPONSE.into()),
        TestServerAction::Respond(OK_JSON.into()),
        TestServerAction::Respond(OK_JSON.into()),
        TestServerAction::Respond(OK_RESPONSE.into()),
        TestServerAction::Respond(OK_JSON.into()),
    ]);
    let base = server.base_url.clone();
    let state = AppState::open_in_memory().unwrap();
    save_scope_collection(&state, &base);

    let subtree = run_collection_impl(
        &state,
        scope_input(Some("fold_a"), None),
        None,
        &crate::events::NoEvents,
    )
    .await
    .unwrap();
    let ids: Vec<_> = subtree
        .items
        .iter()
        .map(|item| item.request_id.as_str())
        .collect();
    assert_eq!(ids, ["r2", "r3"]);
    for item in &subtree.items {
        assert_eq!(item.url, format!("{base}/{}", item.request_id));
        let id = HistoryEntryId::new(item.history_entry_id.as_deref().unwrap()).unwrap();
        let entry = state
            .store()
            .get_execution_history("ws_scope", &id)
            .unwrap()
            .unwrap();
        assert_eq!(
            entry.request_id.as_ref().map(|id| id.as_str()),
            Some(item.request_id.as_str())
        );
    }
    // The stored summary reopens the same run: its id is the history rows' run id.
    let stored = state
        .store()
        .get_collection_run("ws_scope", &subtree.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.folder_id.as_deref(), Some("fold_a"));
    assert_eq!((stored.planned, stored.total), (2, 2));
    let first = HistoryEntryId::new(subtree.items[0].history_entry_id.as_deref().unwrap()).unwrap();
    let entry = state
        .store()
        .get_execution_history("ws_scope", &first)
        .unwrap()
        .unwrap();
    assert_eq!(entry.run_id.as_deref(), Some(subtree.run_id.as_str()));

    mutate_scope_request(&state, "r1", |req| {
        req.assertions = vec![Assertion::StatusEquals { value: 200 }];
        req.test_script =
            r#"pm.execution.setNextRequest("r4"); pm.variables.set("carried", "yes");"#.into();
    });
    mutate_scope_request(&state, "r4", |req| {
        req.assertions = vec![Assertion::StatusEquals { value: 500 }];
        req.test_script = r#"pm.test("local carried", () => {
            pm.expect(pm.variables.get("carried")).to.equal("yes");
        });"#
            .into();
    });
    let mut flow_in = scope_input(None, None);
    flow_in.data = Some(vec![BTreeMap::from([("id".into(), "1".into())])]);
    let streamed = std::sync::Mutex::new(Vec::new());
    let sink = |event: crate::events::RunnerEvent| {
        streamed.lock().unwrap().push(event);
        true
    };
    let flow_res = run_collection_impl(&state, flow_in, None, &sink)
        .await
        .unwrap();
    assert_eq!(
        (flow_res.total, flow_res.passed, flow_res.failed),
        (2, 1, 1)
    );
    assert_eq!(
        flow_res
            .items
            .iter()
            .map(|item| item.request_id.as_str())
            .collect::<Vec<_>>(),
        vec!["r1", "r4"]
    );
    assert!(flow_res.items[0].passed && !flow_res.items[1].passed);
    // A `pm.variables` value set by one request is read by the next one of the same iteration.
    assert!(
        flow_res.items[1].script_tests.len() == 1 && flow_res.items[1].script_tests[0].passed,
        "{:?}",
        flow_res.items[1].script_tests
    );
    assert_eq!(flow_res.items[1].data_row, Some(0));
    assert_eq!(
        flow_res.items[1].data_preview,
        [("id".to_owned(), "1".to_owned())]
    );
    let streamed = streamed.into_inner().unwrap();
    assert_eq!(streamed.len(), 2);
    for (event, item) in streamed.iter().zip(&flow_res.items) {
        let crate::events::RunnerEvent::ItemCompleted {
            item: streamed_item,
        } = event;
        assert_eq!(streamed_item.iteration, item.iteration);
        assert_eq!(streamed_item.status_code, item.status_code);
    }

    let mut environment = Environment::new("env_scope", "ws_scope", "Local").unwrap();
    environment.variables = vec![EnvironmentVariable::masked("tok", "super-secret-token").unwrap()];
    state.store().save_environment(&environment).unwrap();
    mutate_scope_request(&state, "r1", |req| {
        req.url = format!("{base}/r1?k={{{{tok}}}}");
        req.test_script.clear();
    });
    mutate_scope_request(&state, "r2", |req| {
        req.url = "http://127.0.0.1:1/r2?k={{tok}}".into();
    });
    let mut masked_in = scope_input(None, Some(&["r1", "r2"]));
    masked_in.environment_id = Some("env_scope".into());
    let masked_run = run_collection_impl(&state, masked_in, None, &crate::events::NoEvents)
        .await
        .unwrap();
    let sent = &masked_run.items[0];
    assert_eq!(sent.status_code, Some(200));
    assert!(sent.url.contains("k=••••••") && !sent.url.contains("super-secret-token"));
    let failed = &masked_run.items[1];
    assert!(failed.error.is_some() && !failed.passed);
    assert!(
        failed.url.starts_with("http://127.0.0.1:1/r2?k=")
            && !failed.url.contains("super-secret-token")
    );
    let failed_id = HistoryEntryId::new(failed.history_entry_id.as_deref().unwrap()).unwrap();
    assert_eq!(
        state
            .store()
            .get_execution_history("ws_scope", &failed_id)
            .unwrap()
            .unwrap()
            .request_id
            .as_ref()
            .map(|id| id.as_str()),
        Some("r2")
    );

    // Pre-request script widening GET -> DELETE is re-checked by the production guard.
    let mut guard_env = Environment::new("env_guard", "ws_scope", "Production").unwrap();
    guard_env.confirm_before_send = true;
    state.store().save_environment(&guard_env).unwrap();
    mutate_scope_request(&state, "r3", |req| {
        req.url = "http://127.0.0.1:1/r3".into();
        req.pre_request_script = r#"pm.request.method = "DELETE";"#.into();
    });
    let send = |confirmed| {
        request_execute_impl(
            &state,
            execute_input("ws_scope", "r3", Some("env_guard"), confirmed),
            None,
        )
    };
    assert!(matches!(
        send(None).await,
        Err(AppError::ConfirmationRequired { .. })
    ));
    assert!(!matches!(
        send(Some(true)).await,
        Err(AppError::ConfirmationRequired { .. })
    ));
    let run = |confirmed| {
        let mut input = scope_input(None, Some(&["r3"]));
        input.environment_id = Some("env_guard".into());
        input.confirmed = confirmed;
        run_collection_impl(&state, input, None, &crate::events::NoEvents)
    };
    assert!(
        run(None).await.unwrap().items[0]
            .error
            .as_deref()
            .unwrap()
            .contains("confirm before sending")
    );
    assert!(
        !run(Some(true)).await.unwrap().items[0]
            .error
            .as_deref()
            .unwrap()
            .contains("confirm before sending")
    );

    workspace_create_impl(&state, "ws_other".into(), "Other".into()).unwrap();
    let env_other = Environment::new("env_other", "ws_other", "Other").unwrap();
    let req_other =
        Request::new("req_other", "ws_other", "Other", "http://127.0.0.1:1/never").unwrap();
    {
        let store = state.store();
        store.save_environment(&env_other).unwrap();
        store.save_request(&req_other).unwrap();
    }
    let exec = |ws: &str, req: &str, env: Option<&str>| {
        request_execute_impl(&state, execute_input(ws, req, env, None), None)
    };
    assert!(matches!(
        exec("ws_other", "r1", None).await,
        Err(AppError::NotFound { .. })
    ));
    let foreign_env = exec("ws_other", "req_other", Some("env_scope"))
        .await
        .unwrap_err();
    assert!(
        matches!(foreign_env, AppError::NotFound { .. })
            && foreign_env.message().contains("env_scope")
    );
    let run_ws = |ws: &str, col: &str, env: Option<&str>| {
        let mut input = run_input(ws, col);
        input.environment_id = env.map(Into::into);
        run_collection_impl(&state, input, None, &crate::events::NoEvents)
    };
    for (ws, col, env) in [
        ("ws_other", "col_scope", None),
        ("ws_scope", "col_scope", Some("env_missing")),
        ("ws_scope", "col_scope", Some("env_other")),
    ] {
        assert!(matches!(
            run_ws(ws, col, env).await,
            Err(AppError::NotFound { .. })
        ));
    }
    assert!(
        state
            .store()
            .list_execution_history("ws_other", 10)
            .unwrap()
            .is_empty()
    );

    let mut col = Collection::new("col_scope", "ws_scope", "Scope").unwrap();
    col.pre_request_script = r#"
        console.log("collection-pre");
        pm.collectionVariables.set("token", "from-collection");
        pm.request.headers.add({ key: "X-Collection", value: "yes" });
    "#
    .into();
    col.test_script = r#"
        console.log("collection-test");
        pm.test("status 200", () => { pm.response.to.have.status(200); });
    "#
    .into();
    let mut fold = Folder::new("fold_a", "col_scope", "A").unwrap();
    fold.pre_request_script = r#"
        console.log("folder-pre");
        pm.request.headers.add({ key: "X-Folder", value: "yes" });
    "#
    .into();
    fold.test_script = r#"
        console.log("folder-test");
        pm.test("content type", () => { pm.response.to.have.header("Content-Type", "application/json"); });
    "#
    .into();
    {
        let store = state.store();
        store.save_collection(&col).unwrap();
        store.save_folder(&fold).unwrap();
    }
    mutate_scope_request(&state, "r2", |req| {
        req.url = format!("{base}/echo?token={{{{token}}}}");
        req.pre_request_script = r#"
            console.log("request-pre");
            pm.variables.set("localValue", "local");
        "#
        .into();
        req.test_script = r#"
            console.log("request-test");
            pm.test("local scope survives phases", () => {
              pm.expect(pm.variables.get("localValue")).to.equal("local");
            });
            pm.test("json ok", () => {
              pm.expect(pm.response.json().ok).to.equal(true);
            });
        "#
        .into();
    });
    let script_res =
        request_execute_impl(&state, execute_input("ws_scope", "r2", None, None), None)
            .await
            .unwrap();
    let received = server.received_requests(6);
    assert!(received[5].contains("x-collection: yes") && received[5].contains("x-folder: yes"));
    assert!(received[5].contains("token=from-collection"));
    assert_eq!(script_res.script_tests.len(), 4);
    assert!(script_res.script_tests.iter().all(|test| test.passed));
    assert_eq!(
        script_res
            .console
            .iter()
            .map(|entry| entry.message.as_str())
            .collect::<Vec<_>>(),
        vec![
            "collection-pre",
            "folder-pre",
            "request-pre",
            "collection-test",
            "folder-test",
            "request-test"
        ]
    );
    let saved_col = state
        .store()
        .get_collection(&col.workspace_id, &col.id)
        .unwrap()
        .unwrap();
    assert!(
        saved_col
            .variables
            .iter()
            .any(|v| { v.key == "token" && v.value.as_deref() == Some("from-collection") })
    );
}

#[tokio::test]
async fn cancellation_and_draft_sends_preserve_markers_passwords_and_timeouts() {
    let silent_server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/hang", silent_server.local_addr().unwrap());
    let state = state_with_workspaces(&["ws_cancel", "ws_run_cancel", "ws_drafts"]);
    save_request(
        &state,
        &Request::new("req_cancel", "ws_cancel", "Cancel", &url).unwrap(),
    );

    let started = std::time::Instant::now();
    let (result, cancelled) = tokio::join!(
        request_execute_impl(
            &state,
            execute_input("ws_cancel", "req_cancel", None, None),
            Some("tok-1".into()),
        ),
        async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            execution_cancel_impl(&state, "tok-1".into())
        }
    );
    assert!(cancelled);
    let error = result.unwrap_err();
    assert!(
        error.is_cancelled()
            && error
                == AppError::Cancelled {
                    message: "Request cancelled".to_owned(),
                    bytes_received: Some(0),
                }
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(
        history_list(&state, "ws_cancel".into(), None, None)
            .unwrap()
            .is_empty()
    );
    assert!(!execution_cancel_impl(&state, "tok-1".into()));

    let delayed = TestServer::spawn_sequence_with_delay(
        vec![
            TestServerAction::Respond(OK_RESPONSE.into()),
            TestServerAction::Respond(OK_RESPONSE.into()),
            TestServerAction::Respond(OK_RESPONSE.into()),
        ],
        Duration::from_millis(150),
    );
    let collection = Collection::new("col_run_cancel", "ws_run_cancel", "Cancelled").unwrap();
    {
        let store = state.store();
        store.save_collection(&collection).unwrap();
        for (index, id) in ["one", "two", "three"].into_iter().enumerate() {
            let mut request = Request::new(id, "ws_run_cancel", id, &delayed.base_url).unwrap();
            request.collection_id = Some(collection.id.clone());
            request.position = index as i64;
            store.save_request(&request).unwrap();
        }
    }
    let (run_res, run_cancelled) = tokio::join!(
        run_collection_impl(
            &state,
            run_input("ws_run_cancel", "col_run_cancel"),
            Some("tok-run".into()),
            &crate::events::NoEvents,
        ),
        async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            execution_cancel_impl(&state, "tok-run".into())
        }
    );
    let run_res = run_res.unwrap();
    assert!(run_cancelled && run_res.stopped && run_res.items.len() < 3);

    // Draft proxy password unmasking, agent marker retention, and timeout clamping.
    let proxy = TestServer::spawn(OK_RESPONSE);
    let server = TestServer::spawn_sequence(vec![
        TestServerAction::Respond(OK_RESPONSE.into()),
        TestServerAction::Respond(OK_RESPONSE.into()),
        TestServerAction::Respond(OK_RESPONSE.into()),
    ]);
    let proxy_url = proxy.base_url.replace("http://", "");
    let mut saved_proxy =
        Request::new("req_proxy", "ws_drafts", "Proxy", "http://target.invalid/x").unwrap();
    saved_proxy.network_options.proxy_url = Some(format!("http://user:s3cret-pw@{proxy_url}"));
    save_request(&state, &saved_proxy);
    let mut draft_proxy =
        draft_request_input("req_proxy", "ws_drafts", "http://target.invalid/x".into());
    draft_proxy.network_options.proxy_url = Some(format!("http://user:****@{proxy_url}"));
    let proxy_res = request_execute_draft_impl(
        &state,
        ExecuteDraftRequestInput {
            request: draft_proxy,
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(proxy_res.status_code, 200);
    use base64::Engine as _;
    let expected_auth = base64::engine::general_purpose::STANDARD
        .encode("user:s3cret-pw")
        .to_lowercase();
    assert!(
        proxy
            .received_request()
            .contains(&format!("proxy-authorization: basic {expected_auth}"))
    );

    save_request(
        &state,
        &Request::new(
            "req_marker",
            "ws_drafts",
            "Marker",
            format!("{}/stored", server.base_url),
        )
        .unwrap(),
    );
    assert!(
        state
            .store()
            .mark_request_agent_touched("ws_drafts", "req_marker", 1)
            .unwrap()
    );
    let mut draft_marker = draft_request_input(
        "req_marker",
        "ws_drafts",
        format!("{}/draft", server.base_url),
    );
    draft_marker.network_options.timeout_ms = 0;
    let draft_res = request_execute_draft_impl(
        &state,
        ExecuteDraftRequestInput {
            request: draft_marker.clone(),
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(draft_res.status_code, 200);
    // A send that carried nothing to a host no one else declares showed the user no host: the
    // marker stays, so the agent's request cannot make its host a declared one.
    assert!(
        state
            .store()
            .agent_touched_request_ids("ws_drafts")
            .unwrap()
            .contains("req_marker")
    );
    // Once a request the user owns declares the host, a UI draft to the stored request's host
    // clears its agent marker.
    save_request(
        &state,
        &Request::new(
            "req_owner",
            "ws_drafts",
            "Owner",
            format!("{}/owner", server.base_url),
        )
        .unwrap(),
    );
    request_execute_draft_impl(
        &state,
        ExecuteDraftRequestInput {
            request: draft_marker.clone(),
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    assert!(
        state
            .store()
            .agent_touched_request_ids("ws_drafts")
            .unwrap()
            .is_empty()
    );
    assert!(
        state
            .store()
            .mark_request_agent_touched("ws_drafts", "req_marker", 2)
            .unwrap()
    );
    request_execute_impl(
        &state,
        execute_input("ws_drafts", "req_marker", None, None),
        None,
    )
    .await
    .unwrap();
    assert!(
        state
            .store()
            .agent_touched_request_ids("ws_drafts")
            .unwrap()
            .is_empty()
    );
    server.received_requests(3);

    let stored_timeout = |timeout_ms: u64| {
        let mut input = draft_marker.clone();
        input.id = "req_timeout".into();
        input.network_options.timeout_ms = timeout_ms;
        request_save_impl(&state, input).unwrap();
        state
            .store()
            .get_request(
                &WorkspaceId::new("ws_drafts").unwrap(),
                &RequestId::new("req_timeout").unwrap(),
            )
            .unwrap()
            .unwrap()
            .network_options
            .timeout_ms
    };
    assert_eq!(stored_timeout(0), NetworkOptions::default().timeout_ms);
    assert_eq!(stored_timeout(u64::MAX), 3_600_000);
    assert_eq!(stored_timeout(1_500), 1_500);
}

/// When the history row cannot be written (here its workspace is gone), the response
/// still arrives and says so.
#[tokio::test]
async fn a_send_whose_history_row_is_not_written_still_returns_its_response() {
    let state = state_with_workspaces(&[]);
    let server = TestServer::spawn(OK_RESPONSE);
    let response = request_execute_draft_impl(
        &state,
        ExecuteDraftRequestInput {
            request: draft_request_input(
                "req_gone",
                "ws_gone",
                format!("{}/gone", server.base_url),
            ),
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        (
            response.status_code,
            response.history_id,
            response.history_saved
        ),
        (200, None, false)
    );
}

/// The UI asks once before sending a request the agent wrote that carries a secret to a
/// host its environment does not declare; any one condition missing sends without asking.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_written_request_with_a_secret_to_an_undeclared_host_asks_once() {
    let state = state_with_workspaces(&["ws_ag"]);
    let server = TestServer::spawn_sequence(vec![
        TestServerAction::Respond(OK_RESPONSE.into()),
        TestServerAction::Respond(OK_RESPONSE.into()),
    ]);
    let closed = format!("http://127.0.0.1:{}", free_port());
    let save = |url: &str, secret: bool, touched: bool, declared: Option<&str>| {
        let mut environment = Environment::new("env_ag", "ws_ag", "Dev").unwrap();
        environment.variables = vec![EnvironmentVariable::masked("token", "tok_ag_1").unwrap()];
        environment.declared_hosts = declared.map(str::to_owned).into_iter().collect();
        let store = state.store();
        store.save_environment(&environment).unwrap();
        let mut request = Request::new("r_ag", "ws_ag", "Agent request", url).unwrap();
        if secret {
            request.headers = vec![KeyValue {
                key: "Authorization".into(),
                value: "Bearer {{token}}".into(),
                enabled: true,
                description: None,
            }];
        }
        store.save_request(&request).unwrap();
        if touched {
            store
                .mark_request_agent_touched("ws_ag", "r_ag", 1)
                .unwrap();
        } else {
            store.clear_request_agent_touched("ws_ag", "r_ag").unwrap();
        }
        request
    };
    let draft = |request: &Request, confirmed: Option<bool>| {
        let mut input = draft_request_input("r_ag", "ws_ag", request.url.clone());
        input.headers = request.headers.clone();
        ExecuteDraftRequestInput {
            request: input,
            environment_id: Some("env_ag".into()),
            confirmed,
            ..Default::default()
        }
    };
    let closed_host = closed.trim_start_matches("http://").to_owned();
    for touched in [false, true] {
        for secret in [false, true] {
            for declared in [false, true] {
                let request = save(
                    &format!("{closed}/x"),
                    secret,
                    touched,
                    declared.then_some(closed_host.as_str()),
                );
                let result = request_execute_draft_impl(&state, draft(&request, None), None).await;
                let asked = matches!(&result, Err(AppError::ConfirmationRequired { .. }));
                assert_eq!(
                    asked,
                    touched && secret && !declared,
                    "touched {touched}, secret {secret}, declared {declared}: {result:?}"
                );
                if asked && let Err(error) = &result {
                    assert_eq!(
                        error.code(),
                        Some(crate::error::ErrorCode::AgentTouchedSecret)
                    );
                }
            }
        }
    }

    // In a guarded environment the one `confirmed` flag answers the production guard too, so the
    // question says so: the code names the guard as well.
    let request = save(&format!("{closed}/x"), true, true, None);
    let mut environment = Environment::new("env_ag", "ws_ag", "Prod").unwrap();
    environment.variables = vec![EnvironmentVariable::masked("token", "tok_ag_1").unwrap()];
    environment.confirm_before_send = true;
    environment.confirm_safe_methods = true;
    state.store().save_environment(&environment).unwrap();
    let guarded = request_execute_draft_impl(&state, draft(&request, None), None).await;
    assert_eq!(
        guarded.err().and_then(|error| error.code()),
        Some(crate::error::ErrorCode::AgentTouchedSecretGuarded)
    );

    // Confirmed once, the send goes out and clears the marker: the next send does not ask.
    let request = save(&format!("{}/x", server.base_url), true, true, None);
    let sent = request_execute_draft_impl(&state, draft(&request, Some(true)), None)
        .await
        .unwrap();
    assert_eq!(sent.status_code, 200);
    assert!(
        state
            .store()
            .agent_touched_request_ids("ws_ag")
            .unwrap()
            .is_empty()
    );
    let again = request_execute_draft_impl(&state, draft(&request, None), None).await;
    assert_eq!(again.unwrap().status_code, 200);
}

#[tokio::test]
async fn history_keeps_the_definition_before_scripts_and_inherited_auth_and_reopens_it_resolved() {
    let state = state_with_workspaces(&["ws_hist"]);
    let server = TestServer::spawn(OK_RESPONSE);
    let mut collection = Collection::new("col_hist", "ws_hist", "Billing").unwrap();
    collection.auth = AuthConfig::Bearer {
        token_ref: "collection-literal-token".into(),
    };
    state.store().save_collection(&collection).unwrap();
    let mut request = Request::new(
        "req_hist",
        "ws_hist",
        "Invoices",
        format!("{}/invoices", server.base_url),
    )
    .unwrap();
    request.collection_id = Some(collection.id.clone());
    request.auth = AuthConfig::Inherit;
    request.pre_request_script = "pm.request.headers.add({ key: 'X-Added', value: '1' });".into();
    save_request(&state, &request);

    let response = request_execute_impl(
        &state,
        execute_input("ws_hist", "req_hist", None, None),
        None,
    )
    .await
    .unwrap();
    let sent = server.received_request();
    assert!(sent.contains("authorization: bearer collection-literal-token"));
    assert!(sent.contains("x-added: 1"));

    let entry = history_get_impl(&state, "ws_hist".into(), response.history_id.unwrap()).unwrap();
    let snapshot = entry.request_snapshot.clone().unwrap();
    // The definition before the script ran, with the inherited auth kept as a reference.
    assert_eq!(snapshot.auth, AuthConfig::Inherit);
    assert!(
        snapshot
            .headers
            .iter()
            .all(|header| header.key != "X-Added")
    );
    assert!(
        !serde_json::to_string(&snapshot)
            .unwrap()
            .contains("collection-literal-token")
    );

    // Reopened, it leaves the collection and carries the collection's auth as it is now; the
    // request's own script runs once more on a clean definition.
    let draft = crate::app::request_model_service::request_draft_from_history_resolved(
        &state, entry, "ws_hist", None,
    )
    .unwrap();
    assert_eq!(draft.auth, collection.auth);
    assert!(draft.collection_id.is_none() && draft.revision.is_none());
    assert!(draft.headers.iter().all(|header| header.key != "X-Added"));
}
