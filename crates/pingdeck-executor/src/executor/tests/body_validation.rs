//! Body variable validation: unresolved variables and missing files rejected before any I/O.

use crate::executor::send::execute_request;
use crate::executor::types::ExecutorOptions;
use pingdeck_model::{FormPart, HttpMethod, KeyValue, ProtocolKind, Request, RequestBody};

#[tokio::test]
async fn body_validation_rejects_unresolved_variables_missing_files_and_bad_graphql_before_io() {
    let cases = [
        (
            ProtocolKind::HttpRest,
            RequestBody::Json {
                content: "{\"customer_id\":\"{{missingCustomerId}}\"}".into(),
            },
            "unresolved variable missingCustomerId",
        ),
        (
            ProtocolKind::HttpRest,
            RequestBody::RawText {
                language: None,
                content: "event={{missingEvent}}".into(),
            },
            "unresolved variable missingEvent",
        ),
        (
            ProtocolKind::HttpRest,
            RequestBody::FormUrlEncoded {
                fields: vec![KeyValue {
                    key: "client_id".into(),
                    value: "{{missingClientId}}".into(),
                    enabled: true,
                    description: None,
                }],
            },
            "unresolved variable missingClientId",
        ),
        (
            ProtocolKind::HttpRest,
            RequestBody::Multipart {
                parts: vec![FormPart {
                    key: "receipt".into(),
                    value: String::new(),
                    file_path: Some("/tmp/pingdeck-missing-receipt.pdf".into()),
                    enabled: true,
                }],
            },
            "failed to read multipart file",
        ),
        (
            ProtocolKind::Graphql,
            RequestBody::Graphql {
                query: "query Viewer { viewer { id } }".into(),
                variables_json: Some("{invalid-json".into()),
                operation_name: Some("Viewer".into()),
            },
            "invalid GraphQL variables JSON",
        ),
        (
            ProtocolKind::Graphql,
            RequestBody::Graphql {
                query: "query Viewer { viewer(id: \"{{missingViewerId}}\") { id } }".into(),
                variables_json: Some("{}".into()),
                operation_name: Some("Viewer".into()),
            },
            "unresolved variable missingViewerId",
        ),
    ];

    for (protocol, body, expected) in cases {
        let mut request = Request::new(
            "req_val",
            "ws_billing",
            "Validation",
            "http://127.0.0.1:1/endpoint",
        )
        .unwrap();
        request.protocol = protocol;
        request.method = HttpMethod::Post;
        request.body = body;
        let error = execute_request(&request, None, ExecutorOptions::default())
            .await
            .expect_err("invalid body should reject before network IO");
        assert!(error.to_string().contains(expected), "{error}");
    }
}
