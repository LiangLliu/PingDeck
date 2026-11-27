//! Request body encoding: JSON, raw, form, multipart and GraphQL payloads.

use super::support::{
    TestServer, TestServerAction, TestTempFile, environment_with_plain_values,
    multipart_file_request, write_temp_file,
};
use crate::executor::send::execute_request;
use crate::executor::types::{ExecutionError, ExecutorOptions};
use spanbay_model::{FormPart, HttpMethod, KeyValue, ProtocolKind, Request, RequestBody};
use std::fs;

#[tokio::test]
async fn request_bodies_encode_json_raw_form_graphql_methods_and_custom_content_types() {
    let actions: Vec<_> = (0..13)
        .map(|_| {
            TestServerAction::Respond(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\n{}".into(),
            )
        })
        .collect();
    let server = super::support::TestServer::spawn_sequence(actions);
    let env = environment_with_plain_values(vec![
        ("customerId", "cus_123"),
        ("eventName", "invoice.created"),
        ("clientId", "ping deck"),
        ("operationName", "AccountSummary"),
        ("amount", "4300"),
        ("status", "paid"),
    ]);
    let kv = |key: &str, value: &str, enabled: bool| KeyValue {
        key: key.into(),
        value: value.into(),
        enabled,
        description: None,
    };

    let cases: Vec<(HttpMethod, ProtocolKind, Vec<KeyValue>, RequestBody)> = vec![
        (
            HttpMethod::Post,
            ProtocolKind::HttpRest,
            vec![],
            RequestBody::Json {
                content: "{\"customer_id\":\"{{customerId}}\",\"amount\":4200}".into(),
            },
        ),
        (
            HttpMethod::Post,
            ProtocolKind::HttpRest,
            vec![kv("Content-Type", "application/merge-patch+json", true)],
            RequestBody::Json {
                content: "{\"amount\":4200}".into(),
            },
        ),
        (
            HttpMethod::Post,
            ProtocolKind::HttpRest,
            vec![],
            RequestBody::RawText {
                language: None,
                content: "event={{eventName}}\nsource=spanbay".into(),
            },
        ),
        (
            HttpMethod::Post,
            ProtocolKind::HttpRest,
            vec![kv("Content-Type", "application/x-ndjson", true)],
            RequestBody::RawText {
                language: None,
                content: "{\"event\":\"invoice.created\"}".into(),
            },
        ),
        (
            HttpMethod::Post,
            ProtocolKind::HttpRest,
            vec![],
            RequestBody::FormUrlEncoded {
                fields: vec![
                    kv("grant_type", "client_credentials", true),
                    kv("client_id", "{{clientId}}", true),
                    kv("disabled", "ignored", false),
                ],
            },
        ),
        (
            HttpMethod::Post,
            ProtocolKind::HttpRest,
            vec![kv(
                "Content-Type",
                "application/x-www-form-urlencoded; charset=utf-8",
                true,
            )],
            RequestBody::FormUrlEncoded {
                fields: vec![kv("grant_type", "client_credentials", true)],
            },
        ),
        (
            HttpMethod::Post,
            ProtocolKind::Graphql,
            vec![],
            RequestBody::Graphql {
                query:
                    "query {{operationName}}($customerId: ID!) { account(id: $customerId) { id } }"
                        .into(),
                variables_json: Some("{\"customerId\":\"{{customerId}}\"}".into()),
                operation_name: Some("{{operationName}}".into()),
            },
        ),
        (
            HttpMethod::Post,
            ProtocolKind::Graphql,
            vec![kv("Content-Type", "application/graphql+json", true)],
            RequestBody::Graphql {
                query: "query Viewer { viewer { id } }".into(),
                variables_json: Some("{}".into()),
                operation_name: Some("Viewer".into()),
            },
        ),
        (
            HttpMethod::Put,
            ProtocolKind::HttpRest,
            vec![],
            RequestBody::Json {
                content: "{\"amount\":{{amount}}}".into(),
            },
        ),
        (
            HttpMethod::Patch,
            ProtocolKind::HttpRest,
            vec![],
            RequestBody::RawText {
                language: None,
                content: "status={{status}}".into(),
            },
        ),
        (
            HttpMethod::Get,
            ProtocolKind::HttpRest,
            vec![],
            RequestBody::Json {
                content: "{\"q\":1}".into(),
            },
        ),
        (
            HttpMethod::Get,
            ProtocolKind::HttpRest,
            vec![],
            RequestBody::Json {
                content: String::new(),
            },
        ),
        (
            HttpMethod::Head,
            ProtocolKind::HttpRest,
            vec![],
            RequestBody::None,
        ),
    ];

    for (method, protocol, headers, body) in cases {
        let mut req = Request::new(
            "req_body",
            "ws_billing",
            "Body",
            format!("{}/endpoint", server.base_url),
        )
        .unwrap();
        req.method = method;
        req.protocol = protocol;
        req.headers = headers;
        req.body = body;
        execute_request(&req, Some(&env), ExecutorOptions::default())
            .await
            .unwrap();
    }

    let received = server.received_requests(13);
    assert!(
        received[0].contains("content-type: application/json")
            && received[0].contains("{\"customer_id\":\"cus_123\",\"amount\":4200}")
    );
    assert!(
        received[1].contains("content-type: application/merge-patch+json")
            && !received[1].contains("content-type: application/json")
    );
    assert!(
        received[2].contains("content-type: text/plain; charset=utf-8")
            && received[2].contains("event=invoice.created\nsource=spanbay")
    );
    assert!(received[3].contains("content-type: application/x-ndjson"));
    assert!(
        received[4].contains("content-type: application/x-www-form-urlencoded")
            && received[4].contains("grant_type=client_credentials&client_id=ping+deck")
            && !received[4].contains("disabled=ignored")
    );
    assert!(received[5].contains("content-type: application/x-www-form-urlencoded; charset=utf-8"));
    assert!(received[6].contains("\"query\":\"query accountsummary($customerid: id!) { account(id: $customerid) { id } }\"") && received[6].contains("\"variables\":{\"customerid\":\"cus_123\"}"));
    assert!(received[7].contains("content-type: application/graphql+json"));
    assert!(received[8].starts_with("put /endpoint") && received[8].contains("{\"amount\":4300}"));
    assert!(received[9].starts_with("patch /endpoint") && received[9].contains("status=paid"));
    assert!(received[10].starts_with("get /endpoint") && received[10].ends_with("{\"q\":1}"));
    for empty in &received[11..13] {
        assert!(!empty.contains("content-length") && !empty.contains("content-type"));
    }
}

#[tokio::test]
async fn multipart_binary_and_secret_masking_behave_correctly() {
    const BEGIN: &[u8] = b"BEGIN-UPLOAD";
    const MIDDLE: &[u8] = b"MIDDLE-BYTES";
    const END: &[u8] = b"END-UPLOAD";
    let mut large = vec![b'm'; 128 * 1024];
    large[..BEGIN.len()].copy_from_slice(BEGIN);
    large[64 * 1024..64 * 1024 + MIDDLE.len()].copy_from_slice(MIDDLE);
    let end_offset = large.len() - END.len();
    large[end_offset..].copy_from_slice(END);
    let large_file = write_temp_file("large-upload.bin", &large);
    let bin_file = write_temp_file("payload.bin", &[0xde, 0xad, 0xbe, 0xef]);

    let server = TestServer::spawn_sequence(vec![
        TestServerAction::Respond(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\n{}".into(),
        ),
        TestServerAction::Respond(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\n{}".into(),
        ),
        TestServerAction::Respond(
            "HTTP/1.1 204 No Content\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into(),
        ),
        TestServerAction::Respond(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".into(),
        ),
    ]);

    let mut mp_req = multipart_file_request(&server.base_url, large_file.path(), 0);
    if let RequestBody::Multipart { parts } = &mut mp_req.body {
        parts.push(FormPart {
            key: "description".into(),
            value: "Q4 {{kind}}".into(),
            file_path: None,
            enabled: true,
        });
        parts.push(FormPart {
            key: "disabled".into(),
            value: "ignored".into(),
            file_path: None,
            enabled: false,
        });
    }
    let env = environment_with_plain_values(vec![("kind", "receipt")]);
    execute_request(&mp_req, Some(&env), ExecutorOptions::default())
        .await
        .unwrap();

    let mut custom_mp = Request::new(
        "req_mp_custom",
        "ws_billing",
        "Custom MP",
        format!("{}/receipts", server.base_url),
    )
    .unwrap();
    custom_mp.method = HttpMethod::Post;
    custom_mp.headers = vec![KeyValue {
        key: "Content-Type".into(),
        value: "multipart/form-data; charset=utf-8; note=\"one;two\"; Boundary=custom-boundary"
            .into(),
        enabled: true,
        description: None,
    }];
    custom_mp.body = RequestBody::Multipart {
        parts: vec![FormPart {
            key: "description".into(),
            value: "Q4 receipt".into(),
            file_path: None,
            enabled: true,
        }],
    };
    let custom_res = execute_request(&custom_mp, None, ExecutorOptions::default())
        .await
        .unwrap();

    let mut bin_req = Request::new(
        "req_bin",
        "ws_billing",
        "Bin",
        format!("{}/upload", server.base_url),
    )
    .unwrap();
    bin_req.method = HttpMethod::Post;
    bin_req.body = RequestBody::Binary {
        file_path: format!("  {} \n", bin_file.path().to_string_lossy()),
        content_type: None,
    };
    execute_request(&bin_req, None, ExecutorOptions::default())
        .await
        .unwrap();

    let mut mask_req = Request::new(
        "req_mask",
        "ws_billing",
        "Mask",
        format!("{}/:item", server.base_url),
    )
    .unwrap();
    mask_req.method = HttpMethod::Post;
    mask_req.protocol = ProtocolKind::Graphql;
    mask_req.path_variables = vec![KeyValue {
        key: "item".into(),
        value: "{{pathSecret}}".into(),
        enabled: true,
        description: None,
    }];
    mask_req.body = RequestBody::Graphql {
        query: "{{jsonSecret}}".into(),
        variables_json: None,
        operation_name: None,
    };
    let mut mask_env = environment_with_plain_values(vec![
        ("pathSecret", "path secret"),
        ("jsonSecret", "private\"value\\line\nend"),
    ]);
    mask_env.variables[0].is_secret = true;
    mask_env.variables[1].is_secret = true;
    let mask_res = execute_request(&mask_req, Some(&mask_env), ExecutorOptions::default())
        .await
        .unwrap();

    let raw_reqs = server.received_request_bytes_many(4);
    assert!(
        raw_reqs[0].windows(BEGIN.len()).any(|w| w == BEGIN)
            && raw_reqs[0].windows(MIDDLE.len()).any(|w| w == MIDDLE)
            && raw_reqs[0].windows(END.len()).any(|w| w == END)
    );
    let req0_str = String::from_utf8_lossy(&raw_reqs[0]);
    assert!(
        req0_str.contains("filename=\"large-upload.bin\"")
            && req0_str.contains("Q4 receipt")
            && !req0_str.contains("ignored")
    );

    let req1_str = String::from_utf8_lossy(&raw_reqs[1]);
    let (headers, body) = req1_str.split_once("\r\n\r\n").unwrap();
    let ct = headers
        .lines()
        .find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                .map(|(_, v)| v.trim())
        })
        .unwrap();
    assert!(ct.contains("charset=utf-8; note=\"one;two\"") && !ct.contains("custom-boundary"));
    let boundary = ct.rsplit_once("; boundary=").unwrap().1;
    assert!(
        body.starts_with(&format!("--{boundary}\r\n"))
            && body.ends_with(&format!("--{boundary}--\r\n"))
    );
    assert_eq!(
        custom_res
            .sent_request
            .unwrap()
            .headers
            .iter()
            .find(|h| h.key.eq_ignore_ascii_case("content-type"))
            .unwrap()
            .value,
        ct
    );

    assert!(
        raw_reqs[2]
            .windows(4)
            .any(|w| w == [0xde, 0xad, 0xbe, 0xef])
    );
    let req3_str = String::from_utf8_lossy(&raw_reqs[3]);
    assert!(
        req3_str.starts_with("POST /path%20secret ")
            && req3_str.contains("private\\\"value\\\\line\\nend")
    );
    let mask_sent = mask_res.sent_request.unwrap();
    assert!(
        !mask_sent.url.contains("path%20secret")
            && !mask_sent.body_text.unwrap().contains("private")
    );
}

fn sparse_file_of_len(name: &str, len: u64) -> TestTempFile {
    let temp_file = write_temp_file(name, b"");
    fs::OpenOptions::new()
        .write(true)
        .open(temp_file.path())
        .and_then(|f| f.set_len(len))
        .unwrap();
    temp_file
}

#[tokio::test]
async fn file_limits_non_regular_files_and_preview_secret_masking_hold() {
    use crate::executor::body::{MAX_REQUEST_BODY_BYTES, SentBody, sent_body_preview};

    let over_limit = sparse_file_of_len("huge.bin", MAX_REQUEST_BODY_BYTES + 1);
    let mut binary = Request::new(
        "req_huge",
        "ws_billing",
        "Huge",
        "http://127.0.0.1:1/upload",
    )
    .unwrap();
    binary.method = HttpMethod::Post;
    binary.body = RequestBody::Binary {
        file_path: over_limit.path().to_string_lossy().into_owned(),
        content_type: None,
    };
    let multipart = multipart_file_request("http://127.0.0.1:1", over_limit.path(), 0);
    for req in [binary, multipart] {
        let err = execute_request(&req, None, ExecutorOptions::default())
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ExecutionError::MultipartFileRead { message, too_large: true, .. } if message == "file is larger than the 1024 MiB request body limit")
        );
    }

    let half1 = sparse_file_of_len("first.bin", MAX_REQUEST_BODY_BYTES / 2 + 1);
    let half2 = sparse_file_of_len("second.bin", MAX_REQUEST_BODY_BYTES / 2 + 1);
    let mut combined = multipart_file_request("http://127.0.0.1:1", half1.path(), 0);
    if let RequestBody::Multipart { parts } = &mut combined.body {
        parts.push(FormPart {
            key: "second".into(),
            value: String::new(),
            file_path: Some(half2.path().to_string_lossy().into_owned()),
            enabled: true,
        });
    }
    let err = execute_request(&combined, None, ExecutorOptions::default())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ExecutionError::MultipartFileRead { message, .. } if message == "multipart body is larger than the 1024 MiB request body limit")
    );

    let dir_holder = write_temp_file("placeholder.bin", b"");
    let mut non_regular = vec![dir_holder.path().parent().unwrap().to_path_buf()];
    if cfg!(unix) {
        non_regular.push("/dev/null".into());
        non_regular.push("/dev/zero".into());
    }
    for path in non_regular {
        let mut bin = Request::new(
            "req_spec",
            "ws_billing",
            "Spec",
            "http://127.0.0.1:1/upload",
        )
        .unwrap();
        bin.method = HttpMethod::Post;
        bin.body = RequestBody::Binary {
            file_path: path.to_string_lossy().into_owned(),
            content_type: None,
        };
        let mp = multipart_file_request("http://127.0.0.1:1", &path, 0);
        for req in [bin, mp] {
            let err = execute_request(&req, None, ExecutorOptions::default())
                .await
                .unwrap_err();
            assert!(
                matches!(&err, ExecutionError::MultipartFileRead { message, .. } if message == "not a regular file")
            );
        }
    }

    let secret = "s3cr3t-value".to_owned();
    let text = format!("{}{secret}{}", "x".repeat(64 * 1024 - 4), "y".repeat(1024));
    let (preview, size, truncated) =
        sent_body_preview(Some(SentBody::text(text)), std::slice::from_ref(&secret));
    let preview = preview.unwrap();
    assert!(
        truncated
            && size == 64 * 1024 - 4 + secret.len() + 1024
            && !preview.contains("s3cr")
            && preview.ends_with('•')
    );
}

/// The raw language sets the Content-Type when no
/// header does, the editor's header wins, and the generated header is listed as implicit.
#[tokio::test]
async fn raw_language_sets_the_implicit_content_type_and_an_explicit_header_wins() {
    let ok = || {
        TestServerAction::Respond(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\n{}".into(),
        )
    };
    let server = TestServer::spawn_sequence(vec![ok(), ok()]);
    let mut xml = Request::new("req_xml", "ws_billing", "XML", server.base_url.clone()).unwrap();
    xml.method = HttpMethod::Post;
    xml.body = RequestBody::RawText {
        content: "<a/>".into(),
        language: Some(spanbay_model::RawLanguage::Xml),
    };
    let mut explicit = xml.clone();
    explicit.headers = vec![KeyValue {
        key: "Content-Type".into(),
        value: "text/xml".into(),
        enabled: true,
        description: None,
    }];

    let implicit = execute_request(&xml, None, ExecutorOptions::default())
        .await
        .unwrap();
    let chosen = execute_request(&explicit, None, ExecutorOptions::default())
        .await
        .unwrap();
    assert_eq!(
        implicit.sent_request.unwrap().implicit_headers,
        vec!["Content-Type".to_owned()]
    );
    assert!(chosen.sent_request.unwrap().implicit_headers.is_empty());
    let received = server.received_requests(2);
    assert!(received[0].contains("content-type: application/xml\r\n"));
    assert!(received[1].contains("content-type: text/xml\r\n"));
    assert!(!received[1].contains("application/xml"));
}

/// With "encode URL automatically" off, Params rows go out as typed.
#[test]
fn params_rows_follow_the_encode_switch() {
    let mut request = Request::new(
        "req_q",
        "ws_billing",
        "Q",
        "http://example.test/s?keep=a%2Fb&q=old",
    )
    .unwrap();
    request.query_params = vec![KeyValue {
        key: "q".into(),
        value: "a/b+c".into(),
        enabled: true,
        description: None,
    }];
    let scopes = spanbay_model::VariableScopes::new();
    let encoded = crate::executor::request::request_url(&request, &scopes).unwrap();
    assert_eq!(encoded.query(), Some("keep=a%2Fb&q=a%2Fb%2Bc"));
    request.network_options.encode_url_automatically = false;
    let raw = crate::executor::request::request_url(&request, &scopes).unwrap();
    assert_eq!(raw.query(), Some("keep=a%2Fb&q=a/b+c"));
}
