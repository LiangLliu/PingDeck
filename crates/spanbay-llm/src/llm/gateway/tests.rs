//! Integration tests of the gateway: a real listener, a real SQLite file and fake upstreams.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

use crate::llm::adapters::{InboundHint, codec};
use crate::llm::events::LlmEvent;
use crate::llm::health::HealthSnapshot;
use crate::llm::hub::{HubEvent, HubPart, HubResponse, collect_events};
use crate::llm::runtime::{DbConn, DbHandle, LlmRuntime};
use crate::llm::sse::SseParser;
use crate::llm::store::{
    self, LogPage, ModelRef, ModelSource, Provider, ProviderKey, ProviderModel, RequestLogEntry,
};
use crate::llm::wire::{AuthStyle, Protocol, ProviderEndpoint};
use spanbay_base::error::AppError;
use spanbay_base::secrets::SecretStore;
use spanbay_model::events::Sequenced;
use spanbay_model::settings::{AiGatewaySettings, Settings};

pub const TOKEN: &str = "pdk_test_token_0123456789";

pub(super) fn mac_lan() -> super::lan::LanSnapshot {
    super::lan::LanSnapshot {
        addresses: vec![super::lan::Interface::new(
            "en0",
            std::net::Ipv4Addr::new(192, 168, 1, 20),
        )],
        hostname: Some("mac".into()),
    }
}

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Value,
    pub raw: Vec<u8>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

#[derive(Debug, Clone)]
pub enum Step {
    Frame(String),
    Pause(Duration),
    Die,
    Hang,
}

#[derive(Debug, Clone)]
pub enum Reply {
    Json(u16, Value),
    Text(u16, &'static str, String),
    Sse(Vec<Step>),
    Hang,
    After(Duration, Box<Reply>),
    WithHeaders(Vec<(&'static str, String)>, Box<Reply>),
}

pub type Handler = Arc<dyn Fn(usize, &Recorded) -> Reply + Send + Sync>;

pub struct Fake {
    pub port: u16,
    pub log: Arc<Mutex<Vec<Recorded>>>,
    pub task: JoinHandle<()>,
}

impl Fake {
    pub async fn start(
        handler: impl Fn(usize, &Recorded) -> Reply + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let log: Arc<Mutex<Vec<Recorded>>> = Arc::default();
        let handler: Handler = Arc::new(handler);
        let shared = Arc::clone(&log);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => {
                            connections.spawn(serve(stream, Arc::clone(&shared), Arc::clone(&handler)));
                        }
                        Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                    },
                    Some(_) = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Self { port, log, task }
    }

    pub async fn always(reply: Reply) -> Self {
        Self::start(move |_, _| reply.clone()).await
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.log.lock().unwrap().clone()
    }

    pub fn count(&self) -> usize {
        self.log.lock().unwrap().len()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<Recorded> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0_u8; 8192];
    let head_end = loop {
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let path = request_line.next()?.to_owned();
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body: Vec<u8> = buffer[head_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Some(Recorded {
        method,
        path,
        headers,
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
        raw: body,
    })
}

async fn serve(mut stream: TcpStream, log: Arc<Mutex<Vec<Recorded>>>, handler: Handler) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    let number = {
        let mut log = log.lock().unwrap();
        log.push(request.clone());
        log.len() - 1
    };
    write_reply(&mut stream, handler(number, &request)).await;
}

pub fn write_reply<'a>(
    stream: &'a mut TcpStream,
    reply: Reply,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        match reply {
            Reply::Json(status, body) => {
                fixed(
                    stream,
                    status,
                    "application/json",
                    &serde_json::to_string(&body).unwrap(),
                    &[],
                )
                .await;
            }
            Reply::Text(status, ct, body) => fixed(stream, status, ct, &body, &[]).await,
            Reply::WithHeaders(extra, inner) => match *inner {
                Reply::Json(status, body) => {
                    fixed(
                        stream,
                        status,
                        "application/json",
                        &serde_json::to_string(&body).unwrap(),
                        &extra,
                    )
                    .await;
                }
                Reply::Text(status, ct, body) => fixed(stream, status, ct, &body, &extra).await,
                other => write_reply(stream, other).await,
            },
            Reply::Hang => std::future::pending::<()>().await,
            Reply::After(delay, inner) => {
                tokio::time::sleep(delay).await;
                write_reply(stream, *inner).await;
            }
            Reply::Sse(steps) => {
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n";
                if stream.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                for step in steps {
                    match step {
                        Step::Frame(text) => {
                            let chunk = format!("{:x}\r\n{text}\r\n", text.len());
                            if stream.write_all(chunk.as_bytes()).await.is_err() {
                                return;
                            }
                        }
                        Step::Pause(delay) => tokio::time::sleep(delay).await,
                        Step::Die => return,
                        Step::Hang => std::future::pending::<()>().await,
                    }
                }
                let _ = stream.write_all(b"0\r\n\r\n").await;
            }
        }
        let _ = stream.shutdown().await;
    })
}

async fn fixed(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &str,
    extra: &[(&'static str, String)],
) {
    let extra: String = extra.iter().map(|(n, v)| format!("{n}: {v}\r\n")).collect();
    let head = format!(
        "HTTP/1.1 {status} Test\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\n{extra}connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

const MAX_LIVE_HARNESSES: usize = 4;
static LIVE_HARNESSES: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

struct HarnessCleanup {
    path: PathBuf,
}

impl HarnessCleanup {
    fn acquire(path: PathBuf) -> Self {
        let (lock, cvar) = &LIVE_HARNESSES;
        let mut active = lock.lock().unwrap_or_else(PoisonError::into_inner);
        while *active >= MAX_LIVE_HARNESSES {
            active = cvar.wait(active).unwrap_or_else(PoisonError::into_inner);
        }
        *active += 1;
        Self { path }
    }
}

impl Drop for HarnessCleanup {
    fn drop(&mut self) {
        for suffix in [
            "",
            "-wal",
            "-shm",
            ".secrets",
            ".secrets-wal",
            ".secrets-shm",
        ] {
            let mut name = self.path.clone().into_os_string();
            name.push(suffix);
            let _ = std::fs::remove_file(name);
        }
        let (lock, cvar) = &LIVE_HARNESSES;
        let mut active = lock.lock().unwrap_or_else(PoisonError::into_inner);
        *active = active.saturating_sub(1);
        cvar.notify_one();
    }
}

pub struct Harness {
    pub llm: Arc<LlmRuntime>,
    pub db: DbConn,
    pub path: PathBuf,
    pub events: Arc<Mutex<Vec<LlmEvent>>>,
    pub http: reqwest::Client,
    _cleanup: HarnessCleanup,
}

#[derive(Debug, Clone)]
pub struct Resp {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.text()))
    }
}

impl Harness {
    pub fn build(tweak: impl FnOnce(&mut AiGatewaySettings)) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pd-gw-{}-{}.sqlite3",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let cleanup = HarnessCleanup::acquire(path.clone());
        drop(spanbay_store::storage::WorkspaceStore::open(&path).unwrap());
        let db = DbHandle::path(&path).open().unwrap();
        store::seed_client_token(&db, "gc_base", "base", TOKEN).unwrap();
        let mut settings = Settings::default();
        settings.ai.gateway.port = 0;
        settings.ai.gateway.require_token = true;
        tweak(&mut settings.ai.gateway);
        let llm = Arc::new(
            LlmRuntime::new(
                DbHandle::path(path.clone()),
                Arc::new(settings),
                Arc::new(spanbay_store::secrets_sqlite::SqliteSecretStore::in_memory().unwrap())
                    as Arc<dyn SecretStore>,
            )
            .unwrap(),
        );
        let events: Arc<Mutex<Vec<LlmEvent>>> = Arc::default();
        let sink_events = Arc::clone(&events);
        llm.events()
            .subscribe_live(Arc::new(move |event: Sequenced<LlmEvent>| {
                sink_events.lock().unwrap().push(event.event);
                true
            }))
            .unwrap();
        let http = reqwest::Client::builder()
            .no_proxy()
            .pool_max_idle_per_host(0)
            .build()
            .unwrap();
        Self {
            llm,
            db,
            path,
            events,
            http,
            _cleanup: cleanup,
        }
    }

    pub fn start(tweak: impl FnOnce(&mut AiGatewaySettings)) -> Self {
        let harness = Self::build(tweak);
        harness.llm.gateway.start(&harness.llm).unwrap();
        harness
    }

    pub fn port(&self) -> u16 {
        self.llm.gateway.status(&self.llm).port
    }

    pub fn set_gateway(&self, edit: impl FnOnce(&mut AiGatewaySettings)) {
        let mut settings = (*self.llm.settings()).clone();
        edit(&mut settings.ai.gateway);
        self.llm.set_settings(Arc::new(settings));
    }

    pub fn add_provider(
        &self,
        id: &str,
        port: u16,
        protocols: &[Protocol],
        keys: &[&str],
        models: &[&str],
    ) {
        let provider = Provider {
            id: id.into(),
            name: format!("Prov {id}"),
            category: "custom".into(),
            base_url: format!("http://127.0.0.1:{port}"),
            auth: AuthStyle::Bearer,
            endpoints: protocols
                .iter()
                .map(|&protocol| ProviderEndpoint {
                    protocol,
                    path_prefix: if protocol == Protocol::Gemini {
                        "/v1beta"
                    } else {
                        "/v1"
                    }
                    .into(),
                    auth: None,
                    url_mode: Default::default(),
                    enabled: true,
                })
                .collect(),
            enabled: true,
            ..Default::default()
        };
        store::save_provider(&self.db, &provider, 1).unwrap();
        for (pos, k) in keys.iter().enumerate() {
            let key = ProviderKey {
                id: (*k).into(),
                provider_id: id.into(),
                name: (*k).into(),
                enabled: true,
                position: i64::try_from(pos).unwrap(),
                ..Default::default()
            };
            store::save_key(&self.db, &key, Some(&format!("secret-{}", key.id)), 1).unwrap();
        }
        for model in models {
            self.add_model(id, model, true);
        }
        self.llm.touch();
    }

    pub fn add_model(&self, provider: &str, model: &str, enabled: bool) {
        let model = ProviderModel {
            provider_id: provider.into(),
            model_id: model.into(),
            source: ModelSource::Manual,
            enabled,
            ..Default::default()
        };
        store::save_model(&self.db, &model).unwrap();
        self.llm.touch();
    }

    pub fn edit_provider(&self, id: &str, edit: impl FnOnce(&mut Provider)) {
        let mut provider = store::get_provider(&self.db, id).unwrap().unwrap();
        edit(&mut provider);
        store::save_provider(&self.db, &provider, 2).unwrap();
        self.llm.touch();
    }

    pub fn add_alias(&self, name: &str, targets: &[(&str, Option<&str>, &str)]) {
        let Some(((provider, _, model), rest)) = targets.split_first() else {
            return;
        };
        let primary = ModelRef {
            provider_id: (*provider).into(),
            model: (*model).into(),
        };
        let mut chain: Vec<ModelRef> = Vec::new();
        for (p, _, m) in rest {
            let fb = ModelRef {
                provider_id: (*p).into(),
                model: (*m).into(),
            };
            if fb != primary && !chain.contains(&fb) {
                chain.push(fb);
            }
        }
        let mut attempts = 0;
        while let Err(error) = store::set_model_fallbacks(&self.db, &primary, &chain)
            .and_then(|()| store::save_legacy_name(&self.db, name, &primary))
        {
            attempts += 1;
            assert!(attempts < 50, "add_alias keeps failing: {error}");
            std::thread::sleep(Duration::from_millis(20));
        }
        self.llm.touch();
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port())
    }

    pub async fn send(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Resp {
        self.send_raw(
            method,
            path,
            headers,
            body.map(|b| serde_json::to_vec(b).unwrap()),
        )
        .await
    }

    pub async fn send_raw(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Vec<u8>>,
    ) -> Resp {
        let mut request = self.http.request(method.parse().unwrap(), self.url(path));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(n, v)| (n.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
            .collect();
        let body = response
            .bytes()
            .await
            .map(|b| b.to_vec())
            .unwrap_or_default();
        Resp {
            status,
            headers,
            body,
        }
    }

    pub async fn post(&self, path: &str, body: &Value) -> Resp {
        let bearer = format!("Bearer {TOKEN}");
        self.send("POST", path, &[("authorization", &bearer)], Some(body))
            .await
    }

    pub async fn post_and_hang_up(&self, path: &str, body: &Value) {
        let payload = serde_json::to_string(body).unwrap();
        let request = format!(
            "POST {path} HTTP/1.1\r\nhost: 127.0.0.1\r\nauthorization: Bearer {TOKEN}\r\ncontent-type: application/json\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{payload}",
            payload.len()
        );
        let mut stream = TcpStream::connect(("127.0.0.1", self.port()))
            .await
            .unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut seen = String::new();
        let mut chunk = [0_u8; 4096];
        tokio::time::timeout(Duration::from_secs(5), async {
            while !seen.contains("data:") {
                let read = stream.read(&mut chunk).await.unwrap_or(0);
                if read == 0 {
                    break;
                }
                seen.push_str(&String::from_utf8_lossy(&chunk[..read]));
            }
        })
        .await
        .expect("no data frame arrived");
        assert!(seen.contains("data:"), "stream ended without frame: {seen}");
        drop(stream);
    }

    fn log_rows(&self) -> Vec<RequestLogEntry> {
        store::list_log(
            &self.db,
            &LogPage {
                limit: store::MAX_LOG_PAGE,
                ..LogPage::default()
            },
        )
        .unwrap()
    }

    pub async fn logs(&self, count: usize) -> Vec<RequestLogEntry> {
        for _ in 0..250 {
            let mut rows = self.log_rows();
            if rows.len() >= count {
                rows.reverse();
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("expected {count} log rows, found {}", self.log_rows().len());
    }

    pub fn health(&self, key: &str, model: &str) -> Option<HealthSnapshot> {
        self.llm
            .health
            .snapshot()
            .into_iter()
            .find(|r| r.key_id == key && r.model_id == model)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.llm.gateway.stop_now(&self.llm);
    }
}

#[derive(Debug, Clone, Default)]
pub struct Canned {
    pub reasoning: Option<&'static str>,
    pub text: Vec<&'static str>,
    pub tool: Option<(&'static str, &'static str, Vec<&'static str>)>,
    pub usage: (u64, u64),
}

impl Canned {
    pub fn text(parts: &[&'static str]) -> Self {
        Self {
            text: parts.to_vec(),
            usage: (11, 7),
            ..Self::default()
        }
    }

    pub fn reasoning() -> Self {
        Self {
            reasoning: Some("let me think"),
            text: vec!["Hel", "lo"],
            usage: (11, 7),
            ..Self::default()
        }
    }

    pub fn tool() -> Self {
        Self {
            tool: Some((
                "call_9",
                "get_weather",
                vec!["{\"ci", "ty\":\"Pa", "ris\"}"],
            )),
            usage: (11, 7),
            ..Self::default()
        }
    }

    pub fn all_text(&self) -> String {
        self.text.concat()
    }
}

pub fn frame(event: Option<&str>, data: &Value) -> Step {
    let head = event.map(|n| format!("event: {n}\n")).unwrap_or_default();
    Step::Frame(format!("{head}data: {data}\n\n"))
}

pub fn canned_json(protocol: Protocol, model: &str, canned: &Canned) -> Value {
    let (input, output) = canned.usage;
    let args = canned.tool.as_ref().map(|(_, _, f)| f.concat());
    match protocol {
        Protocol::OpenaiChat => {
            let mut msg = json!({"role": "assistant", "content": canned.all_text()});
            if let Some(r) = canned.reasoning {
                msg["reasoning_content"] = json!(r);
            }
            let mut finish = "stop";
            if let (Some((id, name, _)), Some(args)) = (&canned.tool, &args) {
                msg["content"] = Value::Null;
                msg["tool_calls"] = json!([{"id": id, "type": "function", "function": {"name": name, "arguments": args}}]);
                finish = "tool_calls";
            }
            json!({"id": "chatcmpl-1", "object": "chat.completion", "created": 1, "model": model,
                "choices": [{"index": 0, "message": msg, "finish_reason": finish}],
                "usage": {"prompt_tokens": input, "completion_tokens": output, "total_tokens": input + output}})
        }
        Protocol::AnthropicMessages => {
            let mut content = Vec::new();
            if let Some(r) = canned.reasoning {
                content.push(json!({"type": "thinking", "thinking": r, "signature": "sig-1"}));
            }
            if !canned.text.is_empty() {
                content.push(json!({"type": "text", "text": canned.all_text()}));
            }
            let mut stop = "end_turn";
            if let (Some((id, name, _)), Some(args)) = (&canned.tool, &args) {
                content.push(json!({"type": "tool_use", "id": id, "name": name, "input": serde_json::from_str::<Value>(args).unwrap()}));
                stop = "tool_use";
            }
            json!({"id": "msg_1", "type": "message", "role": "assistant", "model": model, "content": content,
                "stop_reason": stop, "usage": {"input_tokens": input, "output_tokens": output}})
        }
        Protocol::Gemini => {
            let mut parts = Vec::new();
            if let Some(r) = canned.reasoning {
                parts.push(json!({"text": r, "thought": true}));
            }
            if !canned.text.is_empty() {
                parts.push(json!({"text": canned.all_text()}));
            }
            if let (Some((_, name, _)), Some(args)) = (&canned.tool, &args) {
                parts.push(json!({"functionCall": {"name": name, "args": serde_json::from_str::<Value>(args).unwrap()}}));
            }
            json!({"candidates": [{"content": {"role": "model", "parts": parts}, "finishReason": "STOP", "index": 0}],
                "usageMetadata": {"promptTokenCount": input, "candidatesTokenCount": output, "totalTokenCount": input + output},
                "modelVersion": model, "responseId": "resp-g1"})
        }
        Protocol::OpenaiResponses => {
            let mut items = Vec::new();
            if let Some(r) = canned.reasoning {
                items.push(json!({"type": "reasoning", "id": "rs_1", "summary": [{"type": "summary_text", "text": r}]}));
            }
            if !canned.text.is_empty() {
                items.push(json!({"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
                    "content": [{"type": "output_text", "text": canned.all_text(), "annotations": []}]}));
            }
            if let (Some((id, name, _)), Some(args)) = (&canned.tool, &args) {
                items.push(json!({"type": "function_call", "id": "fc_1", "call_id": id, "name": name, "arguments": args}));
            }
            json!({"id": "resp_1", "object": "response", "model": model, "status": "completed", "output": items,
                "usage": {"input_tokens": input, "output_tokens": output, "total_tokens": input + output}})
        }
        other => panic!("{other:?} has no canned chat answer"),
    }
}

pub fn canned_sse(protocol: Protocol, model: &str, canned: &Canned) -> Vec<Step> {
    let (input, output) = canned.usage;
    let mut steps = Vec::new();
    match protocol {
        Protocol::OpenaiChat => {
            let chunk = |delta: Value, finish: Value| {
                frame(
                    None,
                    &json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": model,
                        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}),
                )
            };
            steps.push(chunk(
                json!({"role": "assistant", "content": ""}),
                Value::Null,
            ));
            if let Some(r) = canned.reasoning {
                steps.push(chunk(json!({"reasoning_content": r}), Value::Null));
            }
            for t in &canned.text {
                steps.push(chunk(json!({"content": t}), Value::Null));
            }
            let mut finish = "stop";
            if let Some((id, name, frags)) = &canned.tool {
                finish = "tool_calls";
                steps.push(chunk(json!({"tool_calls": [{"index": 0, "id": id, "type": "function", "function": {"name": name, "arguments": ""}}]}), Value::Null));
                for f in frags {
                    steps.push(chunk(
                        json!({"tool_calls": [{"index": 0, "function": {"arguments": f}}]}),
                        Value::Null,
                    ));
                }
            }
            steps.push(chunk(json!({}), json!(finish)));
            steps.push(frame(None, &json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": model,
                "choices": [], "usage": {"prompt_tokens": input, "completion_tokens": output, "total_tokens": input + output}})));
            steps.push(Step::Frame("data: [DONE]\n\n".into()));
        }
        Protocol::AnthropicMessages => {
            steps.push(frame(Some("message_start"), &json!({"type": "message_start", "message": {"id": "msg_1", "type": "message",
                "role": "assistant", "model": model, "content": [], "usage": {"input_tokens": input, "output_tokens": 1}}})));
            let mut index = 0;
            let mut block = |start: Value, delta: Value| {
                steps.push(frame(
                    Some("content_block_start"),
                    &json!({"type": "content_block_start", "index": index, "content_block": start}),
                ));
                steps.push(frame(
                    Some("content_block_delta"),
                    &json!({"type": "content_block_delta", "index": index, "delta": delta}),
                ));
                steps.push(frame(
                    Some("content_block_stop"),
                    &json!({"type": "content_block_stop", "index": index}),
                ));
                index += 1;
            };
            if let Some(r) = canned.reasoning {
                block(
                    json!({"type": "thinking", "thinking": ""}),
                    json!({"type": "thinking_delta", "thinking": r}),
                );
            }
            if !canned.text.is_empty() {
                block(
                    json!({"type": "text", "text": ""}),
                    json!({"type": "text_delta", "text": canned.all_text()}),
                );
            }
            let mut stop = "end_turn";
            if let Some((id, name, frags)) = &canned.tool {
                stop = "tool_use";
                steps.push(frame(
                    Some("content_block_start"),
                    &json!({"type": "content_block_start", "index": index,
                    "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}}),
                ));
                for f in frags {
                    steps.push(frame(
                        Some("content_block_delta"),
                        &json!({"type": "content_block_delta", "index": index,
                        "delta": {"type": "input_json_delta", "partial_json": f}}),
                    ));
                }
                steps.push(frame(
                    Some("content_block_stop"),
                    &json!({"type": "content_block_stop", "index": index}),
                ));
            }
            steps.push(frame(Some("message_delta"), &json!({"type": "message_delta", "delta": {"stop_reason": stop}, "usage": {"output_tokens": output}})));
            steps.push(frame(
                Some("message_stop"),
                &json!({"type": "message_stop"}),
            ));
        }
        Protocol::Gemini => {
            let part_frame = |parts: Value| {
                frame(
                    None,
                    &json!({"candidates": [{"content": {"role": "model", "parts": parts}, "index": 0}], "modelVersion": model, "responseId": "resp-g1"}),
                )
            };
            if let Some(r) = canned.reasoning {
                steps.push(part_frame(json!([{"text": r, "thought": true}])));
            }
            for t in &canned.text {
                steps.push(part_frame(json!([{"text": t}])));
            }
            if let Some((_, name, frags)) = &canned.tool {
                let args: Value = serde_json::from_str(&frags.concat()).unwrap();
                steps.push(part_frame(
                    json!([{"functionCall": {"name": name, "args": args}}]),
                ));
            }
            steps.push(frame(None, &json!({"candidates": [{"content": {"role": "model", "parts": [{"text": ""}]},
                "finishReason": "STOP", "index": 0}],
                "usageMetadata": {"promptTokenCount": input, "candidatesTokenCount": output, "totalTokenCount": input + output},
                "modelVersion": model, "responseId": "resp-g1"})));
        }
        Protocol::OpenaiResponses => {
            steps.push(frame(Some("response.created"), &json!({"type": "response.created", "response": {"id": "resp_1", "model": model, "status": "in_progress"}})));
            let mut idx = 0;
            if let Some(r) = canned.reasoning {
                steps.push(frame(Some("response.output_item.added"), &json!({"type": "response.output_item.added", "output_index": idx, "item": {"id": "rs_1", "type": "reasoning", "summary": []}})));
                steps.push(frame(Some("response.reasoning_summary_text.delta"), &json!({"type": "response.reasoning_summary_text.delta", "item_id": "rs_1", "output_index": idx, "summary_index": 0, "delta": r})));
                steps.push(frame(Some("response.output_item.done"), &json!({"type": "response.output_item.done", "output_index": idx, "item": {"id": "rs_1", "type": "reasoning"}})));
                idx += 1;
            }
            if !canned.text.is_empty() {
                steps.push(frame(Some("response.output_item.added"), &json!({"type": "response.output_item.added", "output_index": idx, "item": {"id": "msg_1", "type": "message", "content": []}})));
                for t in &canned.text {
                    steps.push(frame(Some("response.output_text.delta"), &json!({"type": "response.output_text.delta", "item_id": "msg_1", "output_index": idx, "delta": t})));
                }
                steps.push(frame(Some("response.output_item.done"), &json!({"type": "response.output_item.done", "output_index": idx, "item": {"id": "msg_1", "type": "message"}})));
                idx += 1;
            }
            if let Some((id, name, frags)) = &canned.tool {
                steps.push(frame(Some("response.output_item.added"), &json!({"type": "response.output_item.added", "output_index": idx, "item": {"id": "fc_1", "type": "function_call", "call_id": id, "name": name, "arguments": ""}})));
                for f in frags {
                    steps.push(frame(Some("response.function_call_arguments.delta"), &json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "output_index": idx, "delta": f})));
                }
                steps.push(frame(Some("response.function_call_arguments.done"), &json!({"type": "response.function_call_arguments.done", "item_id": "fc_1", "output_index": idx, "arguments": frags.concat()})));
                steps.push(frame(Some("response.output_item.done"), &json!({"type": "response.output_item.done", "output_index": idx, "item": {"id": "fc_1", "type": "function_call"}})));
            }
            steps.push(frame(Some("response.completed"), &json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed",
                "usage": {"input_tokens": input, "output_tokens": output, "total_tokens": input + output}}})));
        }
        other => panic!("{other:?} has no canned chat stream"),
    }
    steps
}

pub fn sse_bytes(steps: &[Step]) -> String {
    steps
        .iter()
        .filter_map(|s| match s {
            Step::Frame(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

pub fn decode_stream(protocol: Protocol, body: &[u8]) -> Result<Vec<HubEvent>, AppError> {
    let mut parser = SseParser::new();
    let mut decoder = codec(protocol).unwrap().stream_decoder();
    let mut events = Vec::new();
    let mut frames = parser.feed(body);
    frames.extend(parser.finish());
    for frame in &frames {
        events.extend(decoder.push(frame)?);
    }
    events.extend(decoder.finish()?);
    Ok(events)
}

pub fn decode_request(
    protocol: Protocol,
    recorded: &Recorded,
    model: &str,
) -> crate::llm::hub::HubRequest {
    let hint = InboundHint {
        model: Some(model.to_owned()),
        stream: recorded.path.contains("stream"),
    };
    codec(protocol)
        .unwrap()
        .decode_request(&recorded.body, &hint)
        .unwrap()
}

pub const CHAT_PROTOCOLS: [Protocol; 4] = [
    Protocol::OpenaiChat,
    Protocol::AnthropicMessages,
    Protocol::Gemini,
    Protocol::OpenaiResponses,
];

pub fn tag(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::OpenaiChat => "chat",
        Protocol::AnthropicMessages => "anth",
        Protocol::Gemini => "gem",
        Protocol::OpenaiResponses => "resp",
        _ => "other",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Case {
    Text,
    Reasoning,
    Tool,
}

impl Case {
    fn canned(self) -> Canned {
        match self {
            Self::Text => Canned::text(&["Hel", "lo ", "world"]),
            Self::Reasoning => Canned::reasoning(),
            Self::Tool => Canned::tool(),
        }
    }
}

fn client_request(
    protocol: Protocol,
    model: &str,
    stream: bool,
    tool_turn: bool,
) -> (String, Value) {
    let args = r#"{"city":"Paris"}"#;
    let schema =
        json!({"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]});
    match protocol {
        Protocol::OpenaiChat => {
            let mut msgs = vec![json!({"role": "system", "content": "be brief"})];
            let mut body = json!({"model": model, "max_tokens": 64, "stream": stream});
            if tool_turn {
                msgs.push(json!({"role": "user", "content": "weather in Paris?"}));
                msgs.push(json!({"role": "assistant", "content": null, "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "get_weather", "arguments": args}}]}));
                msgs.push(json!({"role": "tool", "tool_call_id": "call_1", "content": "sunny"}));
                body["tools"] = json!([{"type": "function", "function": {"name": "get_weather", "description": "weather", "parameters": schema}}]);
            } else {
                msgs.push(json!({"role": "user", "content": "hi"}));
            }
            body["messages"] = json!(msgs);
            ("/v1/chat/completions".into(), body)
        }
        Protocol::AnthropicMessages => {
            let mut body =
                json!({"model": model, "max_tokens": 64, "system": "be brief", "stream": stream});
            body["messages"] = if tool_turn {
                body["tools"] = json!([{"name": "get_weather", "description": "weather", "input_schema": schema}]);
                json!([{"role": "user", "content": "weather in Paris?"},
                    {"role": "assistant", "content": [{"type": "tool_use", "id": "call_1", "name": "get_weather", "input": {"city": "Paris"}}]},
                    {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call_1", "content": "sunny"}]}])
            } else {
                json!([{"role": "user", "content": "hi"}])
            };
            ("/v1/messages".into(), body)
        }
        Protocol::Gemini => {
            let action = if stream {
                "streamGenerateContent?alt=sse"
            } else {
                "generateContent"
            };
            let mut body = json!({"systemInstruction": {"parts": [{"text": "be brief"}]}, "generationConfig": {"maxOutputTokens": 64}});
            body["contents"] = if tool_turn {
                body["tools"] = json!([{"functionDeclarations": [{"name": "get_weather", "description": "weather", "parameters": schema}]}]);
                json!([{"role": "user", "parts": [{"text": "weather in Paris?"}]},
                    {"role": "model", "parts": [{"functionCall": {"name": "get_weather", "args": {"city": "Paris"}}}]},
                    {"role": "user", "parts": [{"functionResponse": {"name": "get_weather", "response": {"result": "sunny"}}}]}])
            } else {
                json!([{"role": "user", "parts": [{"text": "hi"}]}])
            };
            (format!("/v1beta/models/{model}:{action}"), body)
        }
        _ => {
            let mut body = json!({"model": model, "max_output_tokens": 64, "instructions": "be brief", "stream": stream});
            body["input"] = if tool_turn {
                body["tools"] = json!([{"type": "function", "name": "get_weather", "description": "weather", "parameters": schema}]);
                json!([{"role": "user", "content": [{"type": "input_text", "text": "weather in Paris?"}]},
                    {"type": "function_call", "call_id": "call_1", "name": "get_weather", "arguments": args},
                    {"type": "function_call_output", "call_id": "call_1", "output": "sunny"}])
            } else {
                json!([{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}])
            };
            ("/v1/responses".into(), body)
        }
    }
}

pub struct Matrix {
    pub h: Harness,
    pub fakes: Vec<(Protocol, Fake)>,
    pub canned: Arc<Mutex<Canned>>,
}

impl Matrix {
    pub async fn over(h: Harness) -> Self {
        let canned: Arc<Mutex<Canned>> = Arc::default();
        let mut fakes = Vec::new();
        for protocol in CHAT_PROTOCOLS {
            let shared = Arc::clone(&canned);
            let model = format!("m-{}", tag(protocol));
            let answer_model = model.clone();
            let fake = Fake::start(move |_, req| {
                let c = shared.lock().unwrap().clone();
                let streaming = if protocol == Protocol::Gemini {
                    req.path.contains("streamGenerateContent")
                } else {
                    req.body["stream"] == true
                };
                if streaming {
                    Reply::Sse(canned_sse(protocol, &answer_model, &c))
                } else {
                    Reply::Json(200, canned_json(protocol, &answer_model, &c))
                }
            })
            .await;
            h.add_provider(
                tag(protocol),
                fake.port,
                &[protocol],
                &[&format!("k-{}", tag(protocol))],
                &[&model],
            );
            h.add_alias(
                &format!("al-{}", tag(protocol)),
                &[(tag(protocol), None, &model)],
            );
            fakes.push((protocol, fake));
        }
        Self { h, fakes, canned }
    }

    pub fn fake(&self, protocol: Protocol) -> &Fake {
        &self.fakes.iter().find(|(p, _)| *p == protocol).unwrap().1
    }

    async fn run(&self, client: Protocol, upstream: Protocol, stream: bool, case: Case) {
        *self.canned.lock().unwrap() = case.canned();
        let fake = self.fake(upstream);
        let before = fake.count();
        let alias = format!("al-{}", tag(upstream));
        let (path, body) = client_request(client, &alias, stream, case == Case::Tool);
        let response = self.h.post(&path, &body).await;
        let ctx = format!("{client:?} -> {upstream:?}, stream={stream}, {case:?}");
        assert_eq!(response.status, 200, "{ctx}: {}", response.text());
        let seen = fake.requests();
        assert_eq!(seen.len(), before + 1, "{ctx}");

        let model = format!("m-{}", tag(upstream));
        let req = decode_request(upstream, &seen[before], &model);
        assert_eq!(req.stream, stream, "{ctx}");
        if upstream == Protocol::Gemini {
            assert!(
                seen[before].path.contains(&format!("/models/{model}:")),
                "{ctx}"
            );
        } else {
            assert_eq!(req.model, model, "{ctx}");
        }
        assert_eq!(req.system.as_deref(), Some("be brief"), "{ctx}");
        let parts: Vec<&HubPart> = req.messages.iter().flat_map(|m| m.content.iter()).collect();
        if case == Case::Tool {
            let (name, args) = parts
                .iter()
                .find_map(|p| match p {
                    HubPart::ToolCall {
                        name, arguments, ..
                    } => Some((name, arguments)),
                    _ => None,
                })
                .unwrap();
            assert_eq!(name, "get_weather", "{ctx}");
            assert_eq!(
                serde_json::from_str::<Value>(args).unwrap(),
                json!({"city": "Paris"})
            );
            let res = parts.iter().find_map(|p| match p {
                HubPart::ToolResult { content, .. } => Some(crate::llm::hub::text_of(content)),
                _ => None,
            });
            assert!(res.unwrap_or_default().contains("sunny"), "{ctx}");
            assert_eq!(req.tools[0].name, "get_weather", "{ctx}");
        } else {
            assert_eq!(req.messages.last().unwrap().text(), "hi", "{ctx}");
        }

        let (answer, events): (HubResponse, Vec<HubEvent>) = if stream {
            let ev = decode_stream(client, &response.body).unwrap();
            (collect_events(&ev).unwrap(), ev)
        } else {
            let dec = codec(client)
                .unwrap()
                .decode_response(&response.json())
                .unwrap();
            let ev = vec![HubEvent::Start {
                id: dec.id.clone(),
                model: dec.model.clone(),
            }];
            (dec, ev)
        };
        let canned = case.canned();
        let expected_model = if client == upstream { model } else { alias };
        let started = events.iter().find_map(|e| match e {
            HubEvent::Start { model, .. } => Some(model.as_str()),
            _ => None,
        });
        assert_eq!(started, Some(expected_model.as_str()), "{ctx}");
        assert_eq!(answer.text(), canned.all_text(), "{ctx}");
        let reasoning: String = answer
            .content
            .iter()
            .filter_map(|p| match p {
                HubPart::Reasoning { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(reasoning, canned.reasoning.unwrap_or(""), "{ctx}");
        assert_eq!(
            (answer.usage.input_tokens, answer.usage.output_tokens),
            (Some(11), Some(7)),
            "{ctx}"
        );
        let calls = answer.tool_calls();
        if case == Case::Tool {
            assert_eq!((calls.len(), calls[0].1), (1, "get_weather"), "{ctx}");
            assert_eq!(
                serde_json::from_str::<Value>(calls[0].2).unwrap(),
                json!({"city": "Paris"})
            );
            if client != Protocol::Gemini && upstream != Protocol::Gemini {
                assert_eq!(calls[0].0, "call_9", "{ctx}");
            }
            assert_eq!(
                answer.finish,
                crate::llm::hub::HubFinish::ToolCalls,
                "{ctx}"
            );
        } else {
            assert!(calls.is_empty(), "{ctx}");
            assert_eq!(answer.finish, crate::llm::hub::HubFinish::Stop, "{ctx}");
        }
    }
}

pub fn partial_chat_stream() -> Vec<Step> {
    let mut steps =
        canned_sse(Protocol::OpenaiChat, "up", &Canned::text(&["Hel", "lo"]))[..2].to_vec();
    steps.push(Step::Hang);
    steps
}

#[tokio::test(flavor = "multi_thread")]
async fn every_client_protocol_reaches_every_upstream_protocol() {
    let matrix = Matrix::over(Harness::start(|_| {})).await;
    for client in CHAT_PROTOCOLS {
        for upstream in CHAT_PROTOCOLS {
            for stream in [false, true] {
                for case in [Case::Text, Case::Reasoning, Case::Tool] {
                    matrix.run(client, upstream, stream, case).await;
                }
            }
        }
    }
    let logs = matrix.h.logs(96).await;
    assert!(
        logs.iter()
            .all(|r| r.status == 200 && r.outcome == "success")
    );
    assert!(
        logs.iter()
            .all(|r| r.input_tokens == Some(11) && r.output_tokens == Some(7))
    );
    assert_eq!(logs.iter().filter(|r| r.stream).count(), 48);
}

#[tokio::test(flavor = "multi_thread")]
async fn retryable_statuses_fail_over_and_client_disconnect_logs_canceled() {
    let b = Fake::start(|_, req| {
        let canned = Canned::text(&["from b"]);
        if req.body["stream"] == true {
            Reply::Sse(canned_sse(Protocol::OpenaiChat, "up", &canned))
        } else {
            Reply::Json(200, canned_json(Protocol::OpenaiChat, "up", &canned))
        }
    })
    .await;
    for status in [503_u16, 429, 500] {
        let a = Fake::always(Reply::Json(status, json!({"error": {"message": "busy"}}))).await;
        let h = Harness::start(|_| {});
        h.add_provider("a", a.port, &[Protocol::OpenaiChat], &["ka"], &["ma"]);
        h.add_provider("b", b.port, &[Protocol::OpenaiChat], &["kb"], &["mb"]);
        h.add_alias("fo", &[("a", None, "ma"), ("b", None, "mb")]);
        for stream in [false, true] {
            let r = h
                .post(
                    "/v1/chat/completions",
                    &json!({"model": "fo", "messages": [{"role": "user", "content": "hi"}], "stream": stream}),
                )
                .await;
            assert_eq!(r.status, 200, "{status}");
            let text = if stream {
                collect_events(&decode_stream(Protocol::OpenaiChat, &r.body).unwrap())
                    .unwrap()
                    .text()
            } else {
                r.json()["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            };
            assert_eq!(text, "from b");
        }
        // A rate-limited key rests, so the next request starts at b.
        assert_eq!(a.count(), if status == 429 { 1 } else { 2 });
        let logs = h.logs(2).await;
        assert_eq!(logs[0].provider_name.as_deref(), Some("Prov b"));
        let failed_over = logs
            .iter()
            .find(|log| log.attempts.as_array().is_some_and(|rows| rows.len() == 2))
            .unwrap();
        assert_eq!(failed_over.attempts[0]["status"], status);
        assert_eq!(failed_over.attempts[1]["outcome"], "success");
    }

    let hang = Fake::always(Reply::Sse(partial_chat_stream())).await;
    let h = Harness::start(|_| {});
    h.add_provider("a", hang.port, &[Protocol::OpenaiChat], &["ka"], &["ma"]);
    h.add_alias("solo", &[("a", None, "ma")]);
    h.post_and_hang_up(
        "/v1/chat/completions",
        &json!({"model": "solo", "messages": [{"role": "user", "content": "hi"}], "stream": true}),
    )
    .await;
    let logs = h.logs(1).await;
    assert_eq!(
        (logs[0].outcome.as_str(), logs[0].status, logs[0].stream),
        ("canceled", 200, true)
    );
    assert!(logs[0].ttft_ms.is_some());
    // The hung-up first attempt counts for nothing and leaves
    // no health row behind.
    assert!(h.health("ka", "ma").is_none());
}

// C-$5: a model without a price is logged with no cost (never 0) and the reason; the same model
// at a local runtime that needs no key is logged as free.
#[tokio::test(flavor = "multi_thread")]
async fn unpriced_calls_log_no_cost_and_local_runtimes_log_free() {
    let up = Fake::start(|_, _| {
        let canned = Canned::text(&["ok"]);
        Reply::Json(200, canned_json(Protocol::OpenaiChat, "up", &canned))
    })
    .await;
    let h = Harness::start(|_| {});
    h.add_provider(
        "a",
        up.port,
        &[Protocol::OpenaiChat],
        &["ka"],
        &["no-such-model-x"],
    );
    let body =
        json!({"model": "a/no-such-model-x", "messages": [{"role": "user", "content": "hi"}]});
    assert_eq!(h.post("/v1/chat/completions", &body).await.status, 200);
    let logs = h.logs(1).await;
    assert_eq!(
        (logs[0].cost_micro_usd, logs[0].price_source.as_deref()),
        (None, Some("not_in_catalog"))
    );
    h.edit_provider("a", |provider| provider.auth = AuthStyle::None);
    assert_eq!(h.post("/v1/chat/completions", &body).await.status, 200);
    let logs = h.logs(2).await;
    let free = logs
        .iter()
        .find(|log| log.price_source.as_deref() == Some("local_free"))
        .unwrap();
    assert_eq!(free.cost_micro_usd, Some(0));
}
