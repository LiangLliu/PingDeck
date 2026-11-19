//! A blocking fake HTTP/1.1 upstream on `127.0.0.1:0` that records each request and answers
//! through a handler. Shared by the in-crate run tests (`cfg(test)`) and, through `#[path]`, by
//! `tests/common`, so it depends on `std` and `serde_json` only.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;

/// One request a fake upstream received.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    /// Header names lower-cased.
    pub headers: HashMap<String, String>,
    pub body: Value,
}

/// How the fake upstream answers one request.
pub enum Reply {
    /// A JSON body with a status.
    Json(u16, Value),
    /// A 200 event stream made of raw SSE frames.
    Sse(Vec<String>),
    /// A 200 event stream whose frames each wait `ms` milliseconds before they are sent, flushed one by one.
    TimedSse(Vec<(u64, String)>),
}

type Handler = Arc<dyn Fn(usize, &Recorded) -> Reply + Send + Sync>;

/// A blocking HTTP/1.1 server on `127.0.0.1:0` (one thread per connection, `Connection: close`).
pub struct Upstream {
    pub port: u16,
    log: Arc<Mutex<Vec<Recorded>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Upstream {
    /// Binds before returning, so the port accepts connections (queued in the backlog) at once.
    pub fn start(handler: impl Fn(usize, &Recorded) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let log: Arc<Mutex<Vec<Recorded>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Handler = Arc::new(handler);
        let counter = Arc::new(AtomicUsize::new(0));
        let (shared_log, shared_stop) = (Arc::clone(&log), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if shared_stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let (log, handler, counter) = (
                    Arc::clone(&shared_log),
                    Arc::clone(&handler),
                    Arc::clone(&counter),
                );
                std::thread::spawn(move || serve(stream, &log, &handler, &counter));
            }
        });
        Self {
            port,
            log,
            stop,
            thread: Some(thread),
        }
    }

    /// Requests received so far, oldest first.
    pub fn requests(&self) -> Vec<Recorded> {
        self.log.lock().unwrap().clone()
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking accept so the loop sees the flag.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(stream: TcpStream, log: &Mutex<Vec<Recorded>>, handler: &Handler, counter: &AtomicUsize) {
    stream.set_read_timeout(Some(Duration::from_secs(15))).ok();
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let (method, path) = (
        parts.next().unwrap_or_default().to_owned(),
        parts.next().unwrap_or_default().to_owned(),
    );
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let length: usize = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0_u8; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let recorded = Recorded {
        method,
        path,
        headers,
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    };
    // Number the request and record it before answering, so a client that saw the answer can
    // already read the log.
    let number = counter.fetch_add(1, Ordering::SeqCst);
    log.lock().unwrap().push(recorded.clone());
    let mut stream = reader.into_inner();
    match handler(number, &recorded) {
        Reply::Json(status, value) => {
            let body = value.to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
        Reply::Sse(frames) => {
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                 cache-control: no-cache\r\nconnection: close\r\n\r\n"
            );
            for frame in frames {
                if stream.write_all(frame.as_bytes()).is_err() || stream.flush().is_err() {
                    return;
                }
            }
        }
        Reply::TimedSse(frames) => {
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                 cache-control: no-cache\r\nconnection: close\r\n\r\n"
            );
            for (wait_ms, frame) in frames {
                std::thread::sleep(Duration::from_millis(wait_ms));
                if stream.write_all(frame.as_bytes()).is_err() || stream.flush().is_err() {
                    return;
                }
            }
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

pub fn sse(data: &Value) -> String {
    format!("data: {data}\n\n")
}
