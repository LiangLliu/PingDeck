//! URL, path and query variable resolution and HTTP method handling.

use super::support::{TestServer, environment_with_plain_values, environment_with_secret};
use crate::executor::request::{request_url, variable_scopes};
use crate::executor::send::execute_request;
use crate::executor::types::{ExecutionError, ExecutorOptions};
use spanbay_model::{HttpMethod, KeyValue, ProtocolKind, Request, RequestBody};

#[test]
fn request_url_schemes_query_encoding_path_placeholders_and_auto_encoding_hold() {
    let scopes = variable_scopes(None);
    for (typed, expected) in [
        ("localhost:3001/crud-3", "http://localhost:3001/crud-3"),
        ("127.0.0.1:3001/test", "http://127.0.0.1:3001/test"),
        ("[::1]:3001/test", "http://[::1]:3001/test"),
        ("example.com/api/v1", "http://example.com/api/v1"),
        (" example.com/spaced ", "http://example.com/spaced"),
        (
            "//example.com/protocol-relative",
            "http://example.com/protocol-relative",
        ),
        ("http://example.com/kept", "http://example.com/kept"),
        ("HTTPS://example.com/kept", "https://example.com/kept"),
        ("ftp://example.com/kept", "ftp://example.com/kept"),
    ] {
        let req = Request::new("req_scheme", "ws_billing", "No scheme", typed).unwrap();
        assert_eq!(
            request_url(&req, &scopes).unwrap().as_str(),
            expected,
            "{typed}"
        );
    }

    let tpl_req = Request::new(
        "req_url_template",
        "ws_billing",
        "URL template",
        "{{base}}/{{path}}?{{key}}=pre%2F{{value}}%25&duplicate=1&duplicate=2#{{fragment}}",
    )
    .unwrap();
    let env = environment_with_plain_values(vec![
        ("base", "http://example.com"),
        ("path", "items"),
        ("key", "k&= 你好%"),
        ("value", "a&b=c 你好%2F"),
        ("fragment", "section"),
    ]);
    let url = request_url(&tpl_req, &variable_scopes(Some(&env))).unwrap();
    assert_eq!(
        url.as_str(),
        "http://example.com/items?k%26%3D+%E4%BD%A0%E5%A5%BD%25=pre%2Fa%26b%3Dc+%E4%BD%A0%E5%A5%BD%252F%25&duplicate=1&duplicate=2#section"
    );

    let mut blank_path = Request::new(
        "req_path_blank",
        "ws_billing",
        "Get user",
        "http://127.0.0.1:3001/users/:id/{section}",
    )
    .unwrap();
    blank_path.path_variables = vec![
        KeyValue {
            key: "id".into(),
            value: String::new(),
            enabled: true,
            description: None,
        },
        KeyValue {
            key: "section".into(),
            value: String::new(),
            enabled: true,
            description: None,
        },
    ];
    assert_eq!(
        request_url(&blank_path, &scopes).unwrap().as_str(),
        "http://127.0.0.1:3001/users/:id/%7Bsection%7D"
    );

    let mut enc_req = Request::new(
        "req_enc",
        "ws_billing",
        "Enc",
        "{{base}}/files/:name?q={{value}}",
    )
    .unwrap();
    enc_req.path_variables = vec![KeyValue {
        key: "name".into(),
        value: "a/b".into(),
        enabled: true,
        description: None,
    }];
    let enc_env =
        environment_with_plain_values(vec![("base", "http://example.com"), ("value", "x&y=z")]);
    let enc_scopes = variable_scopes(Some(&enc_env));
    assert_eq!(
        request_url(&enc_req, &enc_scopes).unwrap().as_str(),
        "http://example.com/files/a%2Fb?q=x%26y%3Dz"
    );
    enc_req.network_options.encode_url_automatically = false;
    assert_eq!(
        request_url(&enc_req, &enc_scopes).unwrap().as_str(),
        "http://example.com/files/a/b?q=x&y=z"
    );
}

#[tokio::test]
async fn live_requests_resolve_variables_queries_paths_secrets_and_methods() {
    use super::support::TestServerAction;
    let server = TestServer::spawn_sequence(vec![
        TestServerAction::Respond("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into()),
        TestServerAction::Respond("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-SpanBay-Test: yes\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}".into()),
        TestServerAction::Respond("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into()),
        TestServerAction::Respond("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into()),
        TestServerAction::Respond("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into()),
        TestServerAction::Respond("HTTP/1.1 204 No Content\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into()),
        TestServerAction::Respond("HTTP/1.1 200 OK\r\nX-SpanBay-Test: head\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into()),
        TestServerAction::Respond("HTTP/1.1 204 No Content\r\nAllow: GET,POST,OPTIONS\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into()),
    ]);
    let kv = |k: &str, v: &str, enabled: bool| KeyValue {
        key: k.into(),
        value: v.into(),
        enabled,
        description: None,
    };

    let mut q_req = Request::new(
        "req_q",
        "ws_billing",
        "Q",
        "{{base}}/{{path}}?{{key}}=pre%2F{{secret}}%25&override=old&override=older&keep=%2B&disabled=original",
    )
    .unwrap();
    q_req.query_params = vec![
        kv("override", "first", true),
        kv("override", "{{row}}", true),
        kv("disabled", "ignored", false),
    ];
    let mut q_env = environment_with_plain_values(vec![
        ("base", &server.base_url),
        ("path", "items"),
        ("key", "k&= 你好%"),
        ("secret", "a&injected=bad 你好%2F"),
        ("row", "x&y=z %"),
    ]);
    q_env.variables[3].is_secret = true;
    let q_res = execute_request(&q_req, Some(&q_env), ExecutorOptions::default())
        .await
        .unwrap();
    assert!(
        !q_res.url.contains("injected%3D")
            && !q_res.sent_request.unwrap().url.contains("injected%3D")
    );

    let mut inv_req = Request::new(
        "req_inv",
        "ws_billing",
        "Inv",
        format!("{}/{{{{path}}}}?keep=yes&customer_id=old", server.base_url),
    )
    .unwrap();
    inv_req.query_params = vec![
        kv("customer_id", "{{customerId}}", true),
        kv("disabled", "ignored", false),
    ];
    inv_req.headers = vec![
        kv("Accept", "application/json", true),
        kv("X-Trace-Id", "{{traceId}}", true),
    ];
    let inv_env = environment_with_plain_values(vec![
        ("path", "invoices"),
        ("customerId", "cus_123"),
        ("traceId", "trace-abc"),
    ]);
    let inv_res = execute_request(&inv_req, Some(&inv_env), ExecutorOptions::default())
        .await
        .unwrap();
    assert_eq!(
        (inv_res.status_code, inv_res.body_text.as_str()),
        (200, "{\"ok\":true}")
    );

    let mut path_req = Request::new(
        "req_path",
        "ws_billing",
        "Path",
        format!("{}/users/:id/{{kind}}", server.base_url),
    )
    .unwrap();
    path_req.path_variables = vec![kv("id", "42", true), kv("kind", "full profile", true)];
    execute_request(&path_req, None, ExecutorOptions::default())
        .await
        .unwrap();

    let health_req = Request::new(
        "req_health",
        "ws_billing",
        "Health",
        format!("{}/health", server.base_url),
    )
    .unwrap();
    assert!(
        !execute_request(&health_req, None, ExecutorOptions::default())
            .await
            .unwrap()
            .url
            .ends_with('?')
    );

    let mut sec_req = Request::new(
        "req_sec",
        "ws_billing",
        "Sec",
        format!("{}/headers", server.base_url),
    )
    .unwrap();
    sec_req.headers = vec![kv("Authorization", "Bearer {{token}}", true)];
    execute_request(
        &sec_req,
        Some(&environment_with_secret("token", "local-secret")),
        ExecutorOptions::default(),
    )
    .await
    .unwrap();

    for method in [HttpMethod::Delete, HttpMethod::Head, HttpMethod::Options] {
        let mut m_req = Request::new(
            "req_m",
            "ws_billing",
            "M",
            format!("{}/invoices", server.base_url),
        )
        .unwrap();
        m_req.method = method;
        execute_request(&m_req, None, ExecutorOptions::default())
            .await
            .unwrap();
    }

    let received = server.received_requests(8);
    assert!(received[0].starts_with("get /items?k%26%3d+%e4%bd%a0%e5%a5%bd%25=pre%2fa%26injected%3dbad+%e4%bd%a0%e5%a5%bd%252f%25&keep=%2b&disabled=original&override=first&override=x%26y%3dz+%25 http/1.1"));
    assert!(
        received[1].starts_with("get /invoices?keep=yes&customer_id=cus_123 http/1.1")
            && received[1].contains("x-trace-id: trace-abc")
            && !received[1].contains("customer_id=old")
    );
    assert!(received[2].starts_with("get /users/42/full%20profile"));
    assert!(received[3].starts_with("get /health http/1.1"));
    assert!(received[4].contains("authorization: bearer local-secret"));
    assert!(received[5].starts_with("delete /invoices http/1.1"));
    assert!(received[6].starts_with("head /invoices http/1.1"));
    assert!(received[7].starts_with("options /invoices http/1.1"));

    let err_sec = Request::new(
        "req_err",
        "ws_billing",
        "Err",
        "http://127.0.0.1:1/?token={{secret}}",
    )
    .unwrap();
    let mut sec_env = environment_with_plain_values(vec![("secret", "a&private=value")]);
    sec_env.variables[0].is_secret = true;
    assert!(
        !execute_request(&err_sec, Some(&sec_env), ExecutorOptions::default())
            .await
            .unwrap_err()
            .to_string()
            .contains("private")
    );

    let missing = Request::new(
        "req_miss",
        "ws_billing",
        "Miss",
        "http://127.0.0.1:1/{{missing}}",
    )
    .unwrap();
    assert!(
        execute_request(&missing, None, ExecutorOptions::default())
            .await
            .unwrap_err()
            .to_string()
            .contains("unresolved variable missing")
    );

    let mut gql_get = Request::new(
        "req_gql_get",
        "ws_billing",
        "GQL GET",
        "http://127.0.0.1:1/graphql",
    )
    .unwrap();
    gql_get.protocol = ProtocolKind::Graphql;
    gql_get.method = HttpMethod::Get;
    gql_get.body = RequestBody::Graphql {
        query: "{ viewer { id } }".into(),
        variables_json: None,
        operation_name: None,
    };
    assert!(
        execute_request(&gql_get, None, ExecutorOptions::default())
            .await
            .unwrap_err()
            .to_string()
            .contains("only GraphQL POST execution is supported")
    );
}

#[tokio::test]
async fn an_unresolved_send_names_every_missing_variable_of_the_request() {
    let mut request = Request::new(
        "req_keys",
        "ws_billing",
        "Keys",
        "http://127.0.0.1:1/{{tenant}}/orders?page={{tenant}}",
    )
    .unwrap();
    request.headers.push(KeyValue {
        key: "Authorization".into(),
        value: "Bearer {{token}}".into(),
        enabled: true,
        description: None,
    });
    request.body = RequestBody::Json {
        content: r#"{"customer":"{{customer_id}}"}"#.into(),
    };
    let error = execute_request(&request, None, ExecutorOptions::default())
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "unresolved variable tenant");
    let ExecutionError::UnresolvedVariable { keys } = error else {
        panic!("{error:?}");
    };
    assert_eq!(keys, ["tenant", "token", "customer_id"]);
}
