//! Response body capture: size cap, truncation and binary payloads.

use super::support::{TestServer, TestServerAction};
use crate::executor::response::MAX_RESPONSE_BODY_BYTES;
use crate::executor::send::execute_request;
use crate::executor::types::{BodyIncomplete, ExecutionError, ExecutorOptions};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use spanbay_model::Request;
use spanbay_response::bodies::INLINE_BODY_LIMIT_BYTES;

#[tokio::test]
async fn response_body_caps_binary_base64_thresholds_and_error_redaction_hold() {
    let data = spanbay_model::settings::DataSettings {
        response_body_cap_bytes: 512 * 1024 * 1024,
        ..Default::default()
    };
    assert_eq!(
        data.response_body_cap(),
        spanbay_response::bodies::MAX_STORED_BYTES
    );
    assert_eq!(
        crate::executor::ExecutorLimits::from(&data).max_body_bytes,
        spanbay_response::bodies::MAX_STORED_BYTES
    );

    let over_cap = "a".repeat(MAX_RESPONSE_BODY_BYTES + 4096);
    let mut split_utf8 = vec![b'a'; MAX_RESPONSE_BODY_BYTES - 1];
    split_utf8.extend_from_slice(&[0xc3, 0xa9]);
    let mut inline_max = vec![0xff_u8; INLINE_BODY_LIMIT_BYTES];
    inline_max[0] = 0x80;
    let mut inline_over = vec![0xff_u8; INLINE_BODY_LIMIT_BYTES + 1];
    inline_over[0] = 0x80;

    let bin_resp = |bytes: Vec<u8>| {
        let mut r = format!("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", bytes.len()).into_bytes();
        r.extend_from_slice(&bytes);
        TestServerAction::RespondBytes(r)
    };
    let server = TestServer::spawn_sequence(vec![
        TestServerAction::Respond(format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{over_cap}",
            over_cap.len()
        )),
        bin_resp(vec![0, 0xff, 0x80, b'A']),
        bin_resp(split_utf8.clone()),
        bin_resp(inline_max.clone()),
        bin_resp(inline_over.clone()),
        TestServerAction::RespondBytes(
            b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 100\r\n\r\n0123456789"
                .to_vec(),
        ),
    ]);
    let req = Request::new(
        "req_resp",
        "ws_billing",
        "Resp",
        format!("{}/data?api_key=hunter2", server.base_url),
    )
    .unwrap();

    let r_over = execute_request(&req, None, ExecutorOptions::default())
        .await
        .unwrap();
    assert!(
        r_over.body_truncated
            && r_over.body_size_bytes == MAX_RESPONSE_BODY_BYTES
            && r_over.body_base64.is_none()
    );
    // The declared length is the real size; the captured size is the cap.
    assert_eq!(r_over.body_total_bytes, Some(over_cap.len() as u64));

    let r_small_bin = execute_request(&req, None, ExecutorOptions::default())
        .await
        .unwrap();
    assert_eq!(
        BASE64_STANDARD
            .decode(r_small_bin.body_base64.unwrap())
            .unwrap(),
        [0, 0xff, 0x80, b'A']
    );

    let r_split = execute_request(&req, None, ExecutorOptions::default())
        .await
        .unwrap();
    assert!(
        r_split.body_truncated
            && r_split.body_base64.is_none()
            && r_split.raw_body.as_deref() == Some(&split_utf8[..MAX_RESPONSE_BODY_BYTES])
    );

    let r_inline = execute_request(&req, None, ExecutorOptions::default())
        .await
        .unwrap();
    assert!(
        r_inline.body_base64.is_some() && r_inline.raw_body.as_deref() == Some(&inline_max[..])
    );

    let r_inline_over = execute_request(&req, None, ExecutorOptions::default())
        .await
        .unwrap();
    assert!(
        r_inline_over.body_base64.is_none()
            && r_inline_over.raw_body.as_deref() == Some(&inline_over[..])
    );

    let err = execute_request(&req, None, ExecutorOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(&err, ExecutionError::BodyRead(msg) if !msg.contains("hunter2")));
}

/// A declared charset decodes the body, so assertions read the real characters; an
/// undeclared body stays UTF-8.
#[tokio::test]
async fn declared_charset_decodes_the_body_for_assertions() {
    // `{"name":"汉字"}` in GBK.
    let mut gbk = br#"{"name":""#.to_vec();
    gbk.extend_from_slice(&[0xba, 0xba, 0xd7, 0xd6]);
    gbk.extend_from_slice(br#""}"#);
    let mut reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=GBK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        gbk.len()
    )
    .into_bytes();
    reply.extend_from_slice(&gbk);
    let server = TestServer::spawn_sequence(vec![TestServerAction::RespondBytes(reply)]);
    let request = Request::new("req_gbk", "ws_billing", "GBK", server.base_url.clone()).unwrap();

    let result = execute_request(&request, None, ExecutorOptions::default())
        .await
        .unwrap();
    assert_eq!(result.body_text, r#"{"name":"汉字"}"#);
    let checked = crate::assertions::evaluate_assertions(
        &[spanbay_model::models::Assertion::JsonPathEquals {
            path: "$.name".into(),
            value: serde_json::json!("汉字"),
        }],
        &result,
    );
    assert!(checked[0].passed, "{checked:?}");
    assert_eq!(result.raw_body.as_deref(), Some(gbk.as_slice()));
}

/// A server that answers with `head`, then writes each chunk after its delay and keeps the
/// connection open until the test ends (or 10 s pass).
fn stalling_server(head: &'static str, chunks: Vec<(u64, &'static str)>) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request);
        let _ = stream.write_all(head.as_bytes());
        for (delay, chunk) in chunks {
            std::thread::sleep(std::time::Duration::from_millis(delay));
            if stream.write_all(chunk.as_bytes()).is_err() {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(10));
    });
    format!("http://{addr}/stream")
}

const SSE_HEAD: &str =
    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 100000\r\n\r\n";

fn stream_request(url: String, idle_ms: u64, total_ms: Option<u64>) -> Request {
    let mut request = Request::new("req_stream", "ws_billing", "Stream", url).unwrap();
    request.network_options.timeout_ms = idle_ms;
    request.network_options.total_timeout_ms = total_ms;
    request
}

/// A stream that keeps sending outlives the idle timeout; one that stalls ends at it, and the
/// total limit ends any stream. Either way the status, headers and received events are kept.
#[tokio::test]
async fn idle_and_total_timeouts_keep_the_partial_stream() {
    let events = vec![
        (0, "data: 1\n\n"),
        (150, "data: 2\n\n"),
        (150, "data: 3\n\n"),
    ];
    let idle = execute_request(
        &stream_request(stalling_server(SSE_HEAD, events.clone()), 400, None),
        None,
        ExecutorOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(idle.status_code, 200);
    assert_eq!(idle.body_text, "data: 1\n\ndata: 2\n\ndata: 3\n\n");
    assert_eq!(idle.body_incomplete, Some(BodyIncomplete::IdleTimeout));
    assert!(!idle.body_truncated);

    let total_events = vec![
        (0, "data: 1\n\n"),
        (120, "data: 2\n\n"),
        (1_200, "data: 3\n\n"),
    ];
    let started = std::time::Instant::now();
    let total = execute_request(
        &stream_request(stalling_server(SSE_HEAD, total_events), 5_000, Some(600)),
        None,
        ExecutorOptions::default(),
    )
    .await
    .unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    assert_eq!(total.body_incomplete, Some(BodyIncomplete::TotalTimeout));
    assert!(
        total.body_text.starts_with("data: 1\n\ndata: 2") && !total.body_text.contains("data: 3"),
        "{:?}",
        total.body_text
    );

    // Before the headers the total limit is a timeout failure, like the client's own.
    let silent = execute_request(
        &stream_request(stalling_server("", Vec::new()), 5_000, Some(150)),
        None,
        ExecutorOptions::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        silent,
        ExecutionError::Network {
            cause: Some(spanbay_base::error::NetworkCause::Timeout),
            ..
        }
    ));
}

/// Stop after the headers keeps what arrived only for a send that asks for it.
#[tokio::test]
async fn a_cancel_after_the_headers_keeps_the_partial_body_when_asked() {
    for keep in [true, false] {
        let token = spanbay_base::cancel::CancelToken::new();
        let cancel = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            cancel.cancel();
        });
        let outcome = execute_request(
            &stream_request(
                stalling_server(SSE_HEAD, vec![(0, "data: 1\n\n")]),
                5_000,
                None,
            ),
            None,
            ExecutorOptions {
                cancel: Some(token),
                keep_partial_on_cancel: keep,
                ..ExecutorOptions::default()
            },
        )
        .await;
        if keep {
            let partial = outcome.unwrap();
            assert_eq!(partial.body_incomplete, Some(BodyIncomplete::Cancelled));
            assert_eq!(partial.body_text, "data: 1\n\n");
        } else {
            assert!(matches!(
                outcome,
                Err(ExecutionError::Cancelled { bytes_received: 9 })
            ));
        }
    }
}

/// Save-to-file writes every byte past the cap; the result captures the cap and reports both
/// sizes and the file.
#[tokio::test]
async fn save_to_file_streams_the_whole_body_past_the_cap() {
    let body = "x".repeat(10_000);
    let reply = format!(
        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let server = TestServer::spawn_sequence(vec![
        TestServerAction::Respond(reply.clone()),
        TestServerAction::Respond(reply),
    ]);
    let dir = std::env::temp_dir().join(format!("spanbay-save-to-file-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("body.bin");
    let request = Request::new(
        "req_save",
        "ws_billing",
        "Save",
        format!("{}/big", server.base_url),
    )
    .unwrap();
    let result = execute_request(
        &request,
        None,
        ExecutorOptions {
            limits: crate::executor::ExecutorLimits {
                max_body_bytes: 1_000,
                ..Default::default()
            },
            save_body_to: Some(path.clone()),
            ..ExecutorOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap().len(), 10_000);
    assert!(result.body_truncated && result.body_size_bytes == 1_000);
    assert_eq!(result.body_total_bytes, Some(10_000));
    assert_eq!(
        result.saved_body_path.as_deref(),
        Some(path.display().to_string().as_str())
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);

    let unwritable = execute_request(
        &request,
        None,
        ExecutorOptions {
            save_body_to: Some(dir.join("missing-dir").join("body.bin")),
            ..ExecutorOptions::default()
        },
    )
    .await;
    assert!(
        matches!(unwritable, Err(ExecutionError::BodyWrite { .. })),
        "{unwritable:?}"
    );
}
