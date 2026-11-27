//! Transport retry policy, backoff and idempotency rules.

use super::support::{
    TestServer, TestServerAction, multipart_file_request, unused_local_url, write_temp_file,
};
use crate::executor::retry::retry_backoff;
use crate::executor::send::execute_request;
use crate::executor::types::{ExecutionError, ExecutorOptions};
use spanbay_model::{HttpMethod, ProtocolKind, Request, RequestBody};
use std::time::Duration;

#[test]
fn retry_backoff_doubles_from_200ms_and_caps_at_5s() {
    let delays = (1..=8).map(retry_backoff).collect::<Vec<_>>();
    assert_eq!(
        delays,
        [200, 400, 800, 1600, 3200, 5000, 5000, 5000].map(Duration::from_millis)
    );
}

#[tokio::test]
async fn transport_retries_reopen_files_respect_idempotency_and_skip_http_errors() {
    const SENTINEL: &[u8] = b"retry-file-sentinel";
    let temp_file = write_temp_file("retry-upload.bin", SENTINEL);
    let server = TestServer::spawn_sequence(vec![
        TestServerAction::CloseConnection,
        TestServerAction::Respond("HTTP/1.1 204 No Content\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into()),
        TestServerAction::CloseConnection,
        TestServerAction::Respond("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 11\r\n\r\n{\"ok\":true}".into()),
        TestServerAction::CloseConnection,
        TestServerAction::CloseConnection,
        TestServerAction::CloseConnection,
        TestServerAction::CloseConnection,
        TestServerAction::Respond("HTTP/1.1 500 Internal Server Error\r\nConnection: close\r\nContent-Length: 15\r\n\r\n{\"failed\":true}".into()),
    ]);

    let mut mp_put = multipart_file_request(&server.base_url, temp_file.path(), 1);
    mp_put.method = HttpMethod::Put;
    assert_eq!(
        execute_request(&mp_put, None, ExecutorOptions::default())
            .await
            .unwrap()
            .attempt_count,
        2
    );

    let mut get_retry = Request::new(
        "r_get",
        "ws_billing",
        "Retry",
        format!("{}/retry", server.base_url),
    )
    .unwrap();
    get_retry.network_options.retry_count = 1;
    let started = std::time::Instant::now();
    let ok_res = execute_request(&get_retry, None, ExecutorOptions::default())
        .await
        .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(200) && ok_res.attempt_count == 2);

    let mut post_reset = get_retry.clone();
    post_reset.method = HttpMethod::Post;
    post_reset.network_options.retry_count = 2;
    post_reset.body = RequestBody::RawText {
        language: None,
        content: "amount=1".into(),
    };
    let err = execute_request(&post_reset, None, ExecutorOptions::default())
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            ExecutionError::Network {
                connect_phase: false,
                ..
            }
        ) && err.attempt_count() == 1
    );

    let mut gql_reset = post_reset.clone();
    gql_reset.protocol = ProtocolKind::Graphql;
    gql_reset.body = RequestBody::Graphql {
        query: "mutation { charge }".into(),
        variables_json: None,
        operation_name: None,
    };
    assert_eq!(
        execute_request(&gql_reset, None, ExecutorOptions::default())
            .await
            .unwrap_err()
            .attempt_count(),
        1
    );

    let mut exhausted = get_retry.clone();
    exhausted.network_options.retry_count = 1;
    assert_eq!(
        execute_request(&exhausted, None, ExecutorOptions::default())
            .await
            .unwrap_err()
            .attempt_count(),
        2
    );

    let status_500 = execute_request(&get_retry, None, ExecutorOptions::default())
        .await
        .unwrap();
    assert_eq!((status_500.status_code, status_500.attempt_count), (500, 1));

    let raw_reqs = server.received_request_bytes_many(9);
    assert!(
        raw_reqs[..2]
            .iter()
            .all(|r| r.windows(SENTINEL.len()).any(|w| w == SENTINEL))
    );

    let mut conn_post = Request::new(
        "r_conn",
        "ws_billing",
        "Conn",
        format!("{}/charge", unused_local_url()),
    )
    .unwrap();
    conn_post.method = HttpMethod::Post;
    conn_post.network_options.retry_count = 1;
    let conn_err = execute_request(&conn_post, None, ExecutorOptions::default())
        .await
        .unwrap_err();
    assert!(
        matches!(
            conn_err,
            ExecutionError::Network {
                connect_phase: true,
                ..
            }
        ) && conn_err.attempt_count() == 2
    );
}
