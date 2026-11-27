//! Cancellation of in-flight requests, retry backoff and pre-cancelled tokens.

use super::support::unused_local_url;
use crate::executor::send::execute_request;
use crate::executor::types::{ExecutionError, ExecutorOptions};
use spanbay_base::cancel::CancelToken;
use spanbay_model::Request;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[tokio::test]
async fn cancellation_stops_pre_cancelled_in_flight_backoff_and_streaming_body_requests() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let pre_req = cancellable_request(format!("http://{}/never", listener.local_addr().unwrap()));
    let pre_tok = CancelToken::new();
    pre_tok.cancel();
    let err = execute_request(
        &pre_req,
        None,
        ExecutorOptions {
            cancel: Some(pre_tok),
            ..ExecutorOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, ExecutionError::Cancelled { bytes_received: 0 })
            && listener.accept().is_err()
    );

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        if let Ok((_stream, _)) = listener.accept() {
            let _ = accepted_tx.send(());
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
        }
    });
    let tok = CancelToken::new();
    cancel_on_signal(&tok, accepted_rx, Duration::ZERO);
    let started = std::time::Instant::now();
    let err = execute_request(
        &cancellable_request(format!("http://{addr}/hang")),
        None,
        ExecutorOptions {
            cancel: Some(tok),
            ..ExecutorOptions::default()
        },
    )
    .await
    .unwrap_err();
    drop(release_tx);
    assert!(err.is_cancelled() && started.elapsed() < Duration::from_secs(2));

    let mut backoff_req = cancellable_request(format!("{}/gone", unused_local_url()));
    backoff_req.network_options.retry_count = 10;
    let tok = CancelToken::new();
    let tok_cl = tok.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        tok_cl.cancel();
    });
    let started = std::time::Instant::now();
    let err = execute_request(
        &backoff_req,
        None,
        ExecutorOptions {
            cancel: Some(tok),
            ..ExecutorOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert!(err.is_cancelled() && started.elapsed() < Duration::from_secs(2));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (release, release_rx) = mpsc::channel::<()>();
    let (written_tx, written) = mpsc::channel::<()>();
    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut req = [0_u8; 4096];
        let _ = stream.read(&mut req);
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n0123456789");
        let _ = written_tx.send(());
        let _ = release_rx.recv_timeout(Duration::from_secs(10));
    });
    let mut stream_req = cancellable_request(format!("http://{addr}/stalled"));
    stream_req.network_options.retry_count = 0;
    let tok = CancelToken::new();
    cancel_on_signal(&tok, written, Duration::from_millis(200));
    let err = execute_request(
        &stream_req,
        None,
        ExecutorOptions {
            cancel: Some(tok),
            ..ExecutorOptions::default()
        },
    )
    .await
    .unwrap_err();
    drop(release);
    assert!(matches!(
        err,
        ExecutionError::Cancelled { bytes_received: 10 }
    ));
    assert_eq!(
        spanbay_base::error::AppError::from(err),
        spanbay_base::error::AppError::Cancelled {
            message: "Request cancelled".into(),
            bytes_received: Some(10)
        }
    );
}

fn cancellable_request(url: String) -> Request {
    let mut request = Request::new("req_cancel", "ws_billing", "Cancel", url).unwrap();
    request.network_options.timeout_ms = 30_000;
    request.network_options.retry_count = 2;
    request
}

fn cancel_on_signal(token: &CancelToken, signal: mpsc::Receiver<()>, settle: Duration) {
    let token = token.clone();
    thread::spawn(move || {
        let _ = signal.recv();
        thread::sleep(settle);
        token.cancel();
    });
}
