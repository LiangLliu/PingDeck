//! Tests of the streaming safe transport against raw loopback servers.

use std::sync::atomic::Ordering;
use std::task::Poll;
use std::time::{Duration, Instant};

use super::*;
use crate::llm::safe_http::client::ReqwestSafeHttp;
use crate::llm::safe_http::test_server::{Step, post, serve, until_accepted};

const HEAD: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nx-request-id: r1\r\ntransfer-encoding: chunked\r\n\r\n";

fn limits(first_byte: u64, idle: u64) -> StreamLimits {
    StreamLimits {
        first_byte: Duration::from_millis(first_byte),
        idle: Duration::from_millis(idle),
        total: None,
    }
}

async fn open(port: u16, limits: StreamLimits) -> Result<SafeStream, SafeTransportError> {
    ReqwestSafeHttp::direct()
        .open_stream(
            post(format!("http://127.0.0.1:{port}/v1")),
            limits,
            &CancelToken::new(),
        )
        .await
}

async fn read_all(stream: &mut SafeStream) -> Result<String, SafeTransportError> {
    let mut text = String::new();
    while let Some(chunk) = stream.next_chunk().await {
        text.push_str(&String::from_utf8_lossy(&chunk?));
    }
    Ok(text)
}

#[tokio::test]
async fn the_head_arrives_first_then_the_chunks_in_order() {
    let (port, _) = serve(vec![
        Step::Send(HEAD.to_vec()),
        Step::Chunk("data: a\n\n"),
        Step::Pause(Duration::from_millis(30)),
        Step::Chunk("data: b\n\n"),
    ])
    .await;
    let mut stream = open(port, limits(2000, 2000)).await.unwrap();
    assert_eq!(stream.status, 200);
    assert_eq!(stream.header("X-Request-Id"), Some("r1"));
    assert_eq!(stream.header("content-type"), Some("text/event-stream"));
    assert_eq!(
        read_all(&mut stream).await.unwrap(),
        "data: a\n\ndata: b\n\n"
    );
    assert!(stream.next_chunk().await.is_none());
}

#[tokio::test]
async fn silence_longer_than_the_idle_limit_times_out_after_the_chunks_before_it() {
    let (port, _) = serve(vec![
        Step::Send(HEAD.to_vec()),
        Step::Chunk("data: a\n\n"),
        Step::Hang,
    ])
    .await;
    let mut stream = open(port, limits(2000, 150)).await.unwrap();
    let first = stream.next_chunk().await.unwrap().unwrap();
    assert_eq!(&first[..], b"data: a\n\n");
    let started = Instant::now();
    let error = stream.next_chunk().await.unwrap().unwrap_err();
    assert_eq!(error, SafeTransportError::Timeout);
    assert!(started.elapsed() >= Duration::from_millis(140));
    assert!(
        stream.next_chunk().await.is_none(),
        "a failed stream stays closed"
    );
}

#[tokio::test]
async fn the_first_byte_limit_governs_the_head() {
    let (port, _) = serve(vec![Step::Hang]).await;
    // Without the first-byte limit the open would wait for the 30 s request timeout instead.
    let error = tokio::time::timeout(Duration::from_secs(10), open(port, limits(150, 5000)))
        .await
        .expect("the first-byte limit did not end the open")
        .unwrap_err();
    assert_eq!(error, SafeTransportError::Timeout);
}

#[tokio::test]
async fn cancel_ends_a_pending_read() {
    let (port, _) = serve(vec![Step::Send(HEAD.to_vec()), Step::Hang]).await;
    let cancel = CancelToken::new();
    let mut stream = ReqwestSafeHttp::direct()
        .open_stream(
            post(format!("http://127.0.0.1:{port}/")),
            limits(2000, 5000),
            &cancel,
        )
        .await
        .unwrap();
    let read = stream.next_chunk();
    tokio::pin!(read);
    // One poll puts the read on the socket: it is pending, so the cancel interrupts a live read.
    let first = std::future::poll_fn(|cx| Poll::Ready(read.as_mut().poll(cx))).await;
    assert!(first.is_pending());
    cancel.cancel();
    let error = tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .expect("the cancel did not end the read")
        .unwrap()
        .unwrap_err();
    assert_eq!(error, SafeTransportError::Cancelled);
}

#[tokio::test]
async fn cancel_during_the_head_wait_ends_the_open() {
    let (port, accepted) = serve(vec![Step::Hang]).await;
    let cancel = CancelToken::new();
    let http = ReqwestSafeHttp::direct();
    let open = http.open_stream(
        post(format!("http://127.0.0.1:{port}/")),
        limits(5000, 5000),
        &cancel,
    );
    let cancel_once_connected = async {
        until_accepted(&accepted).await;
        cancel.cancel();
    };
    let (opened, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(open, cancel_once_connected)
    })
    .await
    .expect("the cancel did not end the open");
    assert_eq!(opened.unwrap_err(), SafeTransportError::Cancelled);
}

#[tokio::test]
async fn the_whole_call_limit_ends_a_slow_but_steady_stream() {
    let mut steps = vec![Step::Send(HEAD.to_vec())];
    for _ in 0..100 {
        steps.push(Step::Chunk("x"));
        steps.push(Step::Pause(Duration::from_millis(30)));
    }
    let (port, _) = serve(steps).await;
    let mut stream = ReqwestSafeHttp::direct()
        .open_stream(
            post(format!("http://127.0.0.1:{port}/")),
            StreamLimits {
                first_byte: Duration::from_secs(2),
                idle: Duration::from_secs(2),
                total: Some(Duration::from_millis(250)),
            },
            &CancelToken::new(),
        )
        .await
        .unwrap();
    // The script would run for 3 s and end cleanly, so only the limit can produce the error.
    let error = read_all(&mut stream).await.unwrap_err();
    assert_eq!(error, SafeTransportError::Timeout);
}

#[tokio::test]
async fn a_redirect_is_returned_not_followed() {
    let (port, _) = serve(vec![Step::Send(
        b"HTTP/1.1 302 Found\r\nlocation: http://169.254.169.254/x\r\ncontent-length: 0\r\n\r\n"
            .to_vec(),
    )])
    .await;
    let stream = open(port, limits(2000, 2000)).await.unwrap();
    assert_eq!(stream.status, 302);
    assert_eq!(stream.header("location"), Some("http://169.254.169.254/x"));
}

#[tokio::test]
async fn an_address_or_localhost_needs_no_dns_and_every_call_measures_its_own_connect() {
    let (port, _) = serve(vec![Step::Send(HEAD.to_vec()), Step::Chunk("a")]).await;
    let transport = ReqwestSafeHttp::direct();
    for host in ["127.0.0.1", "localhost"] {
        let stream = transport
            .open_stream(
                post(format!("http://{host}:{port}/")),
                limits(2000, 2000),
                &CancelToken::new(),
            )
            .await
            .unwrap();
        assert!(stream.dns.is_none(), "{host}");
        let connect = stream
            .connect
            .unwrap_or_else(|| panic!("{host}: connect not measured"));
        assert!(connect < Duration::from_secs(2), "{host}: {connect:?}");
    }
}

#[tokio::test]
async fn every_call_opens_its_own_connection() {
    let (port, accepted) = serve(vec![Step::Send(HEAD.to_vec()), Step::Chunk("a")]).await;
    for _ in 0..3 {
        let mut stream = open(port, limits(2000, 2000)).await.unwrap();
        assert_eq!(read_all(&mut stream).await.unwrap(), "a");
    }
    assert_eq!(accepted.load(Ordering::SeqCst), 3);
}
