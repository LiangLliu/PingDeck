//! Scripted one-request-per-connection HTTP/1.1 test server shared by the executor and app
//! tests. Compiled only for tests or with the `test-support` feature, never into a release.
#![allow(clippy::expect_used, clippy::panic)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// What the server does with each accepted connection, in order.
pub enum TestServerAction {
    /// Write this raw HTTP response.
    Respond(String),
    /// Write these raw bytes, ignoring write errors.
    RespondBytes(Vec<u8>),
    /// Read the request and close without answering.
    CloseConnection,
}

/// A loopback server that serves one action per connection and records each raw request.
pub struct TestServer {
    pub base_url: String,
    pub received_request_rx: mpsc::Receiver<Vec<u8>>,
    pub thread_handle: thread::JoinHandle<()>,
}

impl TestServer {
    pub fn spawn(response: impl Into<String>) -> Self {
        Self::spawn_sequence(vec![TestServerAction::Respond(response.into())])
    }

    pub fn spawn_sequence(actions: Vec<TestServerAction>) -> Self {
        Self::spawn_sequence_with_delay(actions, Duration::ZERO)
    }

    /// Like `spawn_sequence`, sleeping `delay` after reading each request.
    pub fn spawn_sequence_with_delay(actions: Vec<TestServerAction>, delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server should bind");
        let address = listener.local_addr().expect("test server address");
        let (received_request_tx, received_request_rx) = mpsc::channel();
        let thread_handle = thread::spawn(move || {
            for action in actions {
                let (mut stream, _) = listener.accept().expect("request should connect");
                let request = read_complete_request(&mut stream);
                received_request_tx
                    .send(request)
                    .expect("request bytes should send");
                thread::sleep(delay);

                match action {
                    TestServerAction::Respond(response) => stream
                        .write_all(response.as_bytes())
                        .expect("response should write"),
                    TestServerAction::RespondBytes(response) => {
                        let _ = stream.write_all(&response);
                    }
                    TestServerAction::CloseConnection => {}
                }
            }
        });

        Self {
            base_url: format!("http://{address}"),
            received_request_rx,
            thread_handle,
        }
    }

    /// The single request, lowercased; joins the server thread.
    pub fn received_request(self) -> String {
        let mut received_requests = self.received_requests(1);
        received_requests.remove(0)
    }

    /// The first `expected_count` requests, lowercased; joins the server thread.
    pub fn received_requests(self, expected_count: usize) -> Vec<String> {
        self.received_request_bytes_many(expected_count)
            .into_iter()
            .map(|request| String::from_utf8_lossy(&request).to_lowercase())
            .collect()
    }

    /// The first `expected_count` raw requests; joins the server thread.
    pub fn received_request_bytes_many(self, expected_count: usize) -> Vec<Vec<u8>> {
        let mut received_requests = Vec::new();
        for _ in 0..expected_count {
            received_requests.push(
                self.received_request_rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("test server should receive request"),
            );
        }
        self.thread_handle
            .join()
            .expect("test server thread should finish");
        received_requests
    }
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn request_content_length(headers: &[u8]) -> usize {
    let headers = String::from_utf8_lossy(headers);
    assert!(
        !headers.lines().any(|line| {
            line.split_once(':').is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("transfer-encoding")
                    && value.trim().eq_ignore_ascii_case("chunked")
            })
        }),
        "test server does not support chunked request bodies"
    );
    headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0)
}

/// Reads one request: headers, then a `Content-Length` body (chunked bodies are rejected).
pub fn read_complete_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 8192];
    let header_end = loop {
        let bytes_read = stream.read(&mut buffer).expect("request should read");
        assert!(
            bytes_read > 0,
            "connection closed before request headers completed"
        );
        request.extend_from_slice(&buffer[..bytes_read]);
        if let Some(header_end) = find_header_end(&request) {
            break header_end;
        }
    };
    let target_length = header_end + request_content_length(&request[..header_end]);
    while request.len() < target_length {
        let bytes_read = stream.read(&mut buffer).expect("request body should read");
        assert!(
            bytes_read > 0,
            "connection closed before request body completed"
        );
        request.extend_from_slice(&buffer[..bytes_read]);
    }
    request.truncate(target_length);
    request
}
