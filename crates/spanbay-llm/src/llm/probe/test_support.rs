//! Test doubles for the probe tests: a fake HTTP upstream on 127.0.0.1 and canned bodies.

// Also compiled under the `test-support` feature for the app crate's tests; panicking on a
// failed fake listener is the intended failure mode there, so the production no-panic lints do
// not apply.
#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

use super::*;
use crate::llm::runtime::DbHandle;
use crate::llm::wire::{AuthStyle, ProviderEndpoint, Quirks};
use spanbay_model::settings::Settings;

/// One request the fake upstream received (`headers` is lowercased).
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub headers: String,
    pub body: String,
}

pub enum Reply {
    Json(u16, String),
    /// SSE payloads and the pause between frames in milliseconds.
    Sse(Vec<String>, u64),
    /// A 307 pointing at another URL.
    Redirect(String),
    Hang,
    /// A JSON error with a `Retry-After` header (seconds).
    RetryAfter(u16, u64, String),
    /// The inner reply after a pause in milliseconds.
    After(u64, Box<Reply>),
}

impl Reply {
    pub fn json(status: u16, body: &Value) -> Self {
        Self::Json(status, body.to_string())
    }
}

/// Requests being served right now, and the most that ever were.
#[derive(Debug, Default)]
pub struct InFlight {
    pub current: AtomicUsize,
    pub max: AtomicUsize,
}

impl InFlight {
    fn enter(&self) -> usize {
        let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.max.fetch_max(now, Ordering::SeqCst);
        now
    }

    fn leave(&self) {
        self.current.fetch_sub(1, Ordering::SeqCst);
    }

    pub fn max(&self) -> usize {
        self.max.load(Ordering::SeqCst)
    }
}

pub struct Fake {
    pub addr: SocketAddr,
    pub seen: Arc<Mutex<Vec<Seen>>>,
    /// Woken (with a stored permit) when a request arrives.
    pub arrived: Arc<Notify>,
    /// How many requests were being served when each one arrived, itself included.
    pub inflight_at_arrival: Arc<Mutex<Vec<usize>>>,
    pub inflight: Arc<InFlight>,
}

impl Fake {
    pub fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

pub async fn read_request(socket: &mut TcpStream) -> Option<Seen> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    let head_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let path = request_line.next()?.to_owned();
    let headers = lines.collect::<Vec<_>>().join("\n").to_ascii_lowercase();
    let length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buffer.len() < head_end + length {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body = String::from_utf8_lossy(&buffer[head_end..]).into_owned();
    Some(Seen {
        method,
        path,
        headers,
        body,
    })
}

pub async fn spawn_fake(handler: impl Fn(&Seen) -> Reply + Send + Sync + 'static) -> Fake {
    spawn_counted(handler, Arc::new(InFlight::default())).await
}

/// [`spawn_fake`] that counts its in-flight requests in `inflight` (shared between fakes to
/// count across providers).
pub async fn spawn_counted(
    handler: impl Fn(&Seen) -> Reply + Send + Sync + 'static,
    inflight: Arc<InFlight>,
) -> Fake {
    let arrived = Arc::new(Notify::new());
    let at_arrival = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(handler);
    let log = Arc::clone(&seen);
    let wake = Arc::clone(&arrived);
    let arrivals = Arc::clone(&at_arrival);
    let counter = Arc::clone(&inflight);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            };
            let handler = Arc::clone(&handler);
            let log = Arc::clone(&log);
            let wake = Arc::clone(&wake);
            let arrivals = Arc::clone(&arrivals);
            let counter = Arc::clone(&counter);
            tokio::spawn(async move {
                let Some(request) = read_request(&mut socket).await else {
                    return;
                };
                let now = counter.enter();
                log.lock().unwrap().push(request.clone());
                arrivals.lock().unwrap().push(now);
                wake.notify_one();
                let mut reply = handler(&request);
                while let Reply::After(pause, inner) = reply {
                    tokio::time::sleep(Duration::from_millis(pause)).await;
                    reply = *inner;
                }
                // Free the slot before the answer goes out, so a client that already has the
                // answer can never see its own predecessor as still in flight.
                counter.leave();
                match reply {
                    Reply::Json(status, body) => {
                        let head = format!(
                            "HTTP/1.1 {status} Status\r\ncontent-type: application/json\r\n\
                             content-length: {}\r\nconnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = socket.write_all(head.as_bytes()).await;
                        let _ = socket.write_all(body.as_bytes()).await;
                    }
                    Reply::RetryAfter(status, seconds, body) => {
                        let head = format!(
                            "HTTP/1.1 {status} Status\r\ncontent-type: application/json\r\n\
                             retry-after: {seconds}\r\ncontent-length: {}\r\n\
                             connection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = socket.write_all(head.as_bytes()).await;
                        let _ = socket.write_all(body.as_bytes()).await;
                    }
                    Reply::Redirect(location) => {
                        let head = format!(
                            "HTTP/1.1 307 Temporary Redirect\r\nlocation: {location}\r\n\
                             content-length: 0\r\nconnection: close\r\n\r\n"
                        );
                        let _ = socket.write_all(head.as_bytes()).await;
                    }
                    Reply::Sse(frames, pause) => {
                        let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                                    cache-control: no-cache\r\nconnection: close\r\n\r\n";
                        let _ = socket.write_all(head.as_bytes()).await;
                        for (index, frame) in frames.iter().enumerate() {
                            if index > 0 {
                                tokio::time::sleep(Duration::from_millis(pause)).await;
                            }
                            let _ = socket
                                .write_all(format!("data: {frame}\n\n").as_bytes())
                                .await;
                            let _ = socket.flush().await;
                        }
                    }
                    Reply::Hang => tokio::time::sleep(Duration::from_secs(30)).await,
                    Reply::After(..) => {}
                }
                let _ = socket.shutdown().await;
            });
        }
    });
    Fake {
        addr,
        seen,
        arrived,
        inflight_at_arrival: at_arrival,
        inflight,
    }
}

pub fn target(addr: SocketAddr, protocols: &[Protocol]) -> Target {
    Target {
        provider_id: "p".to_owned(),
        proxy_url: String::new(),
        key_cooldown: true,
        max_output: None,
        provider_name: "Fake".to_owned(),
        key_id: "k1".to_owned(),
        secret: "sk-test".to_owned(),
        base_url: format!("http://{addr}"),
        endpoints: protocols
            .iter()
            .map(|protocol| ProviderEndpoint {
                protocol: *protocol,
                path_prefix: if *protocol == Protocol::Gemini {
                    "/v1beta"
                } else {
                    "/v1"
                }
                .to_owned(),
                auth: Some(protocol.default_auth()),
                url_mode: Default::default(),
                enabled: true,
            })
            .collect(),
        auth: AuthStyle::Bearer,
        quirks: Quirks::default(),
        primary_protocol: None,
        model: "m".to_owned(),
    }
}

pub fn runtime() -> LlmRuntime {
    LlmRuntime::new(
        DbHandle::Unavailable,
        Arc::new(Settings::default()),
        Arc::new(spanbay_store::secrets_sqlite::SqliteSecretStore::in_memory().unwrap()),
    )
    .unwrap()
}

pub fn opts() -> ProbeOptions {
    ProbeOptions {
        prompt: "Reply with the single word: pong".to_owned(),
        protocol: None,
        allow_context_probe: false,
        models_endpoint: Some("/models".to_owned()),
        declared_context: None,
        reasoning_model: false,
    }
}

pub async fn run(fake: &Fake, protocols: &[Protocol], test: ProbeTest) -> ProbeResult {
    run_with(fake, protocols, test, &opts()).await
}

pub async fn run_with(
    fake: &Fake,
    protocols: &[Protocol],
    test: ProbeTest,
    options: &ProbeOptions,
) -> ProbeResult {
    run_test(
        &runtime(),
        &target(fake.addr, protocols),
        test,
        options,
        None,
    )
    .await
}

pub const OPENAI: &[Protocol] = &[Protocol::OpenaiChat];

pub fn oa_message(message: &Value, finish: &str, usage: &Value) -> Reply {
    Reply::json(
        200,
        &json!({
            "id": "chatcmpl-1", "model": "m",
            "choices": [{"index": 0, "message": message, "finish_reason": finish}],
            "usage": usage,
        }),
    )
}

pub fn oa_text(text: &str) -> Reply {
    oa_message(
        &json!({"role": "assistant", "content": text}),
        "stop",
        &json!({"prompt_tokens": 10, "completion_tokens": 4, "total_tokens": 14}),
    )
}

pub fn sse_frames() -> Vec<String> {
    [
        r#"{"id":"c1","model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":"Hel"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":"lo wor"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":"ld"},"finish_reason":"stop"}]}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":20,"total_tokens":25}}"#,
        "[DONE]",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// A fake OpenAI Chat upstream that answers every test of the suite.
pub fn openai_fake(request: &Seen) -> Reply {
    if request.method == "GET" {
        return if request.path.starts_with("/models") {
            Reply::json(200, &json!({"data": [{"id": "m"}, {"id": "gpt-x"}]}))
        } else {
            Reply::json(404, &json!({"error": "not found"}))
        };
    }
    let body = &request.body;
    if body.contains("\"stream\":true") {
        return Reply::Sse(sse_frames(), 60);
    }
    if body.contains("get_weather") {
        return if body.contains("\"role\":\"tool\"") {
            oa_text("It is 18 C and cloudy in Paris.")
        } else {
            oa_message(
                &json!({"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_1", "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}
                }]}),
                "tool_calls",
                &json!({"prompt_tokens": 20, "completion_tokens": 8, "total_tokens": 28}),
            )
        };
    }
    if body.contains("response_format") {
        return oa_text(r#"{"country":"France","capital":"Paris"}"#);
    }
    if body.contains("image_url") {
        return oa_text("Red");
    }
    if body.contains("average speed") {
        return oa_message(
            &json!({"role": "assistant", "content": "80 km/h",
                    "reasoning_content": "200 km over 2.5 hours"}),
            "stop",
            &json!({"prompt_tokens": 30, "completion_tokens": 40, "total_tokens": 70,
                    "completion_tokens_details": {"reasoning_tokens": 30}}),
        );
    }
    oa_text("pong")
}

pub fn anthropic_message(content: &Value, stop: &str) -> Reply {
    Reply::json(
        200,
        &json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "m",
            "content": content, "stop_reason": stop,
            "usage": {"input_tokens": 5, "output_tokens": 2},
        }),
    )
}
