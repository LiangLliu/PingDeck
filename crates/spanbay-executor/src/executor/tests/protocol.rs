//! Protocol executor entry points and error message helpers.

use super::support::{TestServer, environment_with_plain_values};
use crate::executor::execute_request;
use crate::executor::types::{ExecutionError, ExecutionResult, ExecutorOptions, truncate_message};
use spanbay_model::{KeyValue, ProtocolKind, Request};

#[tokio::test]
async fn protocol_executor_and_utf8_truncation_hold() {
    assert_eq!(truncate_message("ab你好cd", 4), "ab…");
    assert_eq!(truncate_message("ab你好cd", 5), "ab你…");
    assert_eq!(truncate_message("你好", 0), "…");

    let server = TestServer::spawn(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
    );
    let mut request = Request::new(
        "req_exec",
        "ws_billing",
        "Exec",
        format!("{}/{{{{path}}}}", server.base_url),
    )
    .unwrap();
    request.query_params = vec![KeyValue {
        key: "customer_id".into(),
        value: "{{customerId}}".into(),
        enabled: true,
        description: None,
    }];
    let env = environment_with_plain_values(vec![("path", "invoices"), ("customerId", "cus_123")]);
    let response: ExecutionResult =
        execute_request(&request, Some(&env), ExecutorOptions::default())
            .await
            .unwrap();
    assert!(
        server
            .received_request()
            .starts_with("get /invoices?customer_id=cus_123 http/1.1")
    );
    assert_eq!(
        (
            response.status_code,
            response.attempt_count,
            response.body_truncated,
            response.body_text.as_str()
        ),
        (200, 1, false, "{\"ok\":true}")
    );

    for protocol in [ProtocolKind::WebSocket, ProtocolKind::Grpc] {
        request.protocol = protocol;
        let refused = execute_request(&request, Some(&env), ExecutorOptions::default()).await;
        assert!(matches!(refused, Err(ExecutionError::UnsupportedProtocol)));
    }
}
