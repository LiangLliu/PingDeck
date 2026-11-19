//! Raw loopback HTTP servers and request builders shared by the transport tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::{SafeMethod, SafeRequest};

/// One step of a scripted reply.
#[derive(Clone)]
pub(super) enum Step {
    /// Bytes written as they are.
    Send(Vec<u8>),
    /// A chunk of a chunked body.
    Chunk(&'static str),
    /// Waits before the next step.
    Pause(Duration),
    /// Keeps the connection open and silent.
    Hang,
}

/// Serves `steps` to every connection on a loopback port after reading the request once, then
/// writes the chunked-body terminator; counts the connections.
pub(super) async fn serve(steps: Vec<Step>) -> (u16, Arc<AtomicUsize>) {
    serve_with(steps, true).await
}

/// Like [`serve`]; `chunked_end` says whether the terminator `0\r\n\r\n` follows the steps.
pub(super) async fn serve_with(steps: Vec<Step>, chunked_end: bool) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            let steps = steps.clone();
            tokio::spawn(async move {
                let mut buffer = [0_u8; 4096];
                let _ = socket.read(&mut buffer).await;
                for step in steps {
                    match step {
                        Step::Send(bytes) => {
                            if socket.write_all(&bytes).await.is_err() {
                                return;
                            }
                        }
                        Step::Chunk(text) => {
                            let chunk = format!("{:x}\r\n{text}\r\n", text.len());
                            if socket.write_all(chunk.as_bytes()).await.is_err() {
                                return;
                            }
                        }
                        Step::Pause(delay) => tokio::time::sleep(delay).await,
                        Step::Hang => std::future::pending::<()>().await,
                    }
                }
                if chunked_end {
                    let _ = socket.write_all(b"0\r\n\r\n").await;
                }
                let _ = socket.shutdown().await;
            });
        }
    });
    (port, accepted)
}

/// Waits until a server of [`serve`] accepted its first connection.
pub(super) async fn until_accepted(accepted: &AtomicUsize) {
    while accepted.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// A request to `url` that allows private targets, keeps 1024 body bytes and waits 3 s for a
/// connection.
fn request(method: SafeMethod, url: String, timeout: Duration) -> SafeRequest {
    SafeRequest {
        method,
        url,
        headers: Vec::new(),
        body: None,
        timeout,
        connect_timeout: Duration::from_secs(3),
        max_body: 1024,
        allow_private: true,
    }
}

/// A JSON `GET` with a 5 s limit.
pub(super) fn get(url: String) -> SafeRequest {
    SafeRequest {
        headers: vec![("accept".into(), "application/json".into())],
        ..request(SafeMethod::Get, url, Duration::from_secs(5))
    }
}

/// A `POST` of `{}` with a 30 s limit.
pub(super) fn post(url: String) -> SafeRequest {
    SafeRequest {
        headers: vec![("content-type".into(), "application/json".into())],
        body: Some(b"{}".to_vec()),
        ..request(SafeMethod::Post, url, Duration::from_secs(30))
    }
}
