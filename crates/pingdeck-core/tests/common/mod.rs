//! Shared helpers of the integration smoke tests: temp dirs, a fake OpenAI-compatible upstream
//! on `127.0.0.1:0`, a raw HTTP/1.1 client and the LLM provider/key/model setup every
//! end-to-end test starts from. Each test binary uses a subset, hence `dead_code`.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[allow(unused_imports)]
pub use pingdeck_app::test_upstream::{Recorded, Reply, Upstream, sse};

use pingdeck_core::app::{
    AppState, SaveProviderKeyInput, SaveRequestInput, llm_key_save_impl, llm_model_save_impl,
    llm_provider_save_impl, llm_subscribe_impl, settings_update_impl,
};
use pingdeck_core::llm::events::LlmEvent;
use pingdeck_core::llm::store::{
    ModelSource, Provider, ProviderKey, ProviderModel, RequestLogEntry,
};
use pingdeck_core::llm::wire::{AuthStyle, Protocol, ProviderEndpoint, Quirks};
use pingdeck_core::model::events::Sequenced;
use pingdeck_core::model::models::{
    AuthConfig, HttpMethod, NetworkOptions, ProtocolKind, RequestBody,
};
use pingdeck_core::model::settings::SettingsSection;
use serde_json::{Value, json};

/// Gateway client token every smoke test configures.
pub const TOKEN: &str = "pdk_c_smoke_token_0123456789";
/// Model reference the clients ask for, in name form (`<provider name>/<model>`).
pub const ALIAS: &str = "Fake upstream/fake-model-1";
/// Model id the reference names at the fake upstream.
pub const UPSTREAM_MODEL: &str = "fake-model-1";
/// Secret of the provider key (what the fake upstream must receive as bearer).
pub const UPSTREAM_SECRET: &str = "sk-fake-upstream-secret";
/// The text the fake upstream answers with.
pub const FINAL_TEXT: &str = "pong from fake upstream";

// ------------------------------------------------------------------------------ temp dirs

/// A unique directory under the system temp dir, removed (recursively) on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(name: &str) -> Self {
        // The clock alone is not unique: its resolution can be a microsecond, so two tests of one
        // process creating a directory together would share it (and delete it under each other).
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let serial = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "pd-smoke-{name}-{}-{nanos}-{serial}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn join(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn python_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// A currently free loopback port that is not the mock servers' first port.
pub fn free_port(avoid: u16) -> u16 {
    loop {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        if port >= 1024 && port != avoid {
            return port;
        }
    }
}

// ----------------------------------------------------------------------- fake upstream

/// The OpenAI-chat behaviour every end-to-end test shares: a request that offers tools and has
/// no tool result yet gets a tool call to its `echo` tool, else the first tool (`{"msg": ...}` for
/// an `echo` tool, else `{"city":"Paris"}`); everything else gets [`FINAL_TEXT`]. Streams and JSON are both
/// supported; any other path is a 404.
pub fn openai_chat(_number: usize, request: &Recorded) -> Reply {
    if request.path != "/v1/chat/completions" {
        return Reply::Json(404, json!({"error": {"message": "no such route"}}));
    }
    let body = &request.body;
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    // The harness tools of a chat (`read_attachment`, `todo_write`, ...) cannot be disabled and
    // come first, so an `echo` tool is looked for before the first tool is taken.
    let offered: Vec<&str> = body["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["function"]["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let tool_name = offered
        .iter()
        .find(|name| name.ends_with("echo"))
        .or_else(|| offered.first())
        .map(|name| (*name).to_owned());
    let answered = messages.iter().any(|message| message["role"] == "tool");
    let call = tool_name.filter(|_| !answered).map(|name| {
        let arguments = if name.ends_with("echo") {
            r#"{"msg":"hello from the model"}"#
        } else {
            r#"{"city":"Paris"}"#
        };
        (name, arguments)
    });
    openai_reply(
        body,
        call.as_ref()
            .map(|(name, arguments)| ("tc_1", name.as_str(), *arguments)),
    )
}

/// An OpenAI chat answer to `body`, streamed when it asks to be: the tool call `(id, name,
/// arguments)` when given, else [`FINAL_TEXT`]; usage 11 in / 7 out.
pub fn openai_reply(body: &Value, call: Option<(&str, &str, &str)>) -> Reply {
    let model = body["model"].as_str().unwrap_or(UPSTREAM_MODEL);
    let usage = json!({"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18});
    if body["stream"] != true {
        let (message, finish) = match &call {
            Some((id, name, arguments)) => (
                json!({"role": "assistant", "content": null, "tool_calls": [{"id": id,
                    "type": "function", "function": {"name": name, "arguments": arguments}}]}),
                "tool_calls",
            ),
            None => (json!({"role": "assistant", "content": FINAL_TEXT}), "stop"),
        };
        return Reply::Json(
            200,
            json!({"id": "chatcmpl-1", "object": "chat.completion", "created": 1, "model": model,
                "choices": [{"index": 0, "message": message, "finish_reason": finish}],
                "usage": usage}),
        );
    }
    let chunk = |delta: Value, finish: Value| {
        sse(
            &json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1,
            "model": model, "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}),
        )
    };
    let mut frames = vec![chunk(
        json!({"role": "assistant", "content": ""}),
        Value::Null,
    )];
    let finish = match &call {
        Some((id, name, arguments)) => {
            frames.push(chunk(
                json!({"tool_calls": [{"index": 0, "id": id, "type": "function",
                    "function": {"name": name, "arguments": ""}}]}),
                Value::Null,
            ));
            let (head, tail) = arguments.split_at(arguments.len() / 2);
            for fragment in [head, tail] {
                frames.push(chunk(
                    json!({"tool_calls": [{"index": 0, "function": {"arguments": fragment}}]}),
                    Value::Null,
                ));
            }
            "tool_calls"
        }
        None => {
            let (head, tail) = FINAL_TEXT.split_at(10);
            for text in [head, tail] {
                frames.push(chunk(json!({"content": text}), Value::Null));
            }
            "stop"
        }
    };
    frames.push(chunk(json!({}), json!(finish)));
    frames.push(sse(
        &json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1,
        "model": model, "choices": [], "usage": usage}),
    ));
    frames.push("data: [DONE]\n\n".to_owned());
    Reply::Sse(frames)
}

// ------------------------------------------------------------------- raw HTTP/1.1 client

/// A parsed HTTP response.
pub struct HttpResponse {
    pub status_line: String,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|error| panic!("not JSON ({error}): {}", self.text()))
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Prints the status line and the first 300 bytes of the body.
    pub fn print(&self, label: &str) {
        let cut = self.body.len().min(300);
        println!(
            "[{label}] {}\n    body[..{cut}]: {}",
            self.status_line,
            String::from_utf8_lossy(&self.body[..cut]).replace('\n', "\\n")
        );
    }
}

/// Sends one request with `Connection: close` and reads the response to the end. A `host`
/// header in `headers` replaces the default `127.0.0.1:<port>`.
pub fn http(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&Value>,
) -> HttpResponse {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(20))).ok();
    let mut head = format!("{method} {path} HTTP/1.1\r\n");
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("host"))
    {
        head.push_str(&format!("host: 127.0.0.1:{port}\r\n"));
    }
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    let payload = body.map(Value::to_string).unwrap_or_default();
    if body.is_some() {
        head.push_str("content-type: application/json\r\n");
    }
    head.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        payload.len()
    ));
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(payload.as_bytes()).unwrap();
    let mut raw = Vec::new();
    // A reset after the response is fine: whatever arrived before it is what counts.
    let _ = stream.read_to_end(&mut raw);
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> HttpResponse {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("no response head in {:?}", String::from_utf8_lossy(raw)));
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.lines();
    let status_line = lines.next().unwrap().to_owned();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let rest = &raw[split + 4..];
    let chunked = headers
        .iter()
        .any(|(name, value)| name == "transfer-encoding" && value.contains("chunked"));
    HttpResponse {
        status_line,
        status,
        headers,
        body: if chunked {
            dechunk(rest)
        } else {
            rest.to_vec()
        },
    }
}

fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(end) = raw.windows(2).position(|window| window == b"\r\n") {
        let size = std::str::from_utf8(&raw[..end])
            .ok()
            .and_then(|line| usize::from_str_radix(line.split(';').next()?.trim(), 16).ok());
        let Some(size) = size.filter(|size| *size > 0) else {
            break;
        };
        raw = &raw[end + 2..];
        let Some(chunk) = raw.get(..size) else { break };
        out.extend_from_slice(chunk);
        raw = raw.get(size + 2..).unwrap_or_default();
    }
    out
}

// ---------------------------------------------------------------------- request input

/// A GET request `id` of `workspace_id` to `url`, everything else empty (the crate's `Default`
/// for it is test-only and invisible here).
pub fn request_input(workspace_id: &str, id: &str, url: String) -> SaveRequestInput {
    SaveRequestInput {
        id: id.into(),
        workspace_id: workspace_id.into(),
        collection_id: None,
        folder_id: None,
        name: id.into(),
        protocol: ProtocolKind::HttpRest,
        method: HttpMethod::Get,
        url,
        query_params: Vec::new(),
        path_variables: Vec::new(),
        headers: Vec::new(),
        auth: AuthConfig::None,
        body: RequestBody::None,
        network_options: NetworkOptions::default(),
        assertions: Vec::new(),
        description: String::new(),
        pre_request_script: String::new(),
        test_script: String::new(),
        position: 0,
        mode: Default::default(),
        ai: None,
        examples: None,
    }
}

// ------------------------------------------------------------------------ LLM setup

/// The provider and key [`configure_llm`] stored.
pub struct LlmSetup {
    pub provider: Provider,
    pub key: ProviderKey,
}

/// Saves, through the `llm_*_impl` use-cases, a provider speaking OpenAI chat at the fake
/// upstream, one key and the tool-capable model [`ALIAS`] names.
pub fn configure_llm(state: &AppState, upstream_port: u16) -> LlmSetup {
    let provider = llm_provider_save_impl(
        state,
        Provider {
            id: "fake".into(),
            name: "Fake upstream".into(),
            preset_id: None,
            category: "custom".into(),
            base_url: format!("http://127.0.0.1:{upstream_port}"),
            auth: AuthStyle::Bearer,
            endpoints: vec![ProviderEndpoint {
                protocol: Protocol::OpenaiChat,
                path_prefix: "/v1".into(),
                auth: None,
                url_mode: Default::default(),
                enabled: true,
            }],
            models_endpoint: None,
            quirks: Quirks::default(),
            proxy_url: String::new(),
            enabled: true,
            position: 0,
            notes: String::new(),
            created_at_ms: 0,
            updated_at_ms: 0,
            ..Default::default()
        },
    )
    .unwrap();
    let key = llm_key_save_impl(
        state,
        SaveProviderKeyInput {
            id: "fake-key".into(),
            provider_id: provider.id.clone(),
            name: "Fake key".into(),
            secret: Some(UPSTREAM_SECRET.into()),
            enabled: true,
            position: 0,
            api_user_id: None,
            management_token: None,
            notes: None,
            group_name: None,
            protocols: None,
        },
    )
    .unwrap();
    llm_model_save_impl(
        state,
        ProviderModel {
            provider_id: provider.id.clone(),
            model_id: UPSTREAM_MODEL.into(),
            source: ModelSource::Manual,
            role: None,
            context: None,
            max_output: None,
            vision: false,
            tools: true,
            notes: None,
            enabled: true,
            ..Default::default()
        },
    )
    .unwrap();
    LlmSetup { provider, key }
}

/// Points the gateway at a free port (never `mock.first_port`) with a client token required and
/// stores a client whose token is [`TOKEN`] (a state without a database file cannot run the
/// gateway, so it gets no client); returns the port. Settings validation forbids port 0, so the
/// port is picked here.
pub fn configure_gateway(state: &AppState) -> u16 {
    let first_mock_port = state.settings().mock.first_port;
    let port = free_port(first_mock_port);
    settings_update_impl(
        state,
        SettingsSection::Ai,
        &json!({"gateway": {"port": port, "require_token": true}}),
    )
    .unwrap();
    let gateway = state.settings().ai.gateway.clone();
    assert_eq!(gateway.port, port);
    assert!(gateway.require_token);
    assert_ne!(gateway.port, first_mock_port);
    if let Some(dirs) = state.data_dirs() {
        // The tests hardcode `TOKEN`, so this one client is stored with a known hash.
        let connection = pingdeck_core::llm::runtime::DbHandle::path(&dirs.db)
            .open()
            .unwrap();
        pingdeck_core::llm::store::seed_client_token(&connection, "gc_smoke", "smoke", TOKEN)
            .unwrap();
    }
    port
}

/// Subscribes to the LLM push events (gateway status changes and request-log batches).
pub fn gateway_events(state: &AppState) -> Receiver<Sequenced<LlmEvent>> {
    let (sender, receiver) = mpsc::channel();
    llm_subscribe_impl(
        state,
        Arc::new(move |event: Sequenced<LlmEvent>| sender.send(event).is_ok()),
        None,
    )
    .unwrap();
    receiver
}

/// Blocks (event-driven, 20 s deadline) until `count` request-log rows arrived in log batches.
pub fn wait_for_requests(
    events: &Receiver<Sequenced<LlmEvent>>,
    count: usize,
) -> Vec<RequestLogEntry> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut entries = Vec::new();
    while entries.len() < count {
        let left = deadline.saturating_duration_since(Instant::now());
        match events.recv_timeout(left) {
            Ok(Sequenced {
                event: LlmEvent::GatewayLogBatch { entries: batch },
                ..
            }) => entries.extend(batch),
            Ok(_) => {}
            Err(error) => panic!(
                "only {} of {count} gateway request events arrived: {error}",
                entries.len()
            ),
        }
    }
    entries
}

/// Splits an SSE body into `(event name, data)` pairs; data that is not JSON (`[DONE]`) becomes
/// a JSON string.
pub fn sse_events(text: &str) -> Vec<(Option<String>, Value)> {
    text.split("\n\n")
        .filter(|block| !block.trim().is_empty())
        .map(|block| {
            let mut name = None;
            let mut data = String::new();
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("event:") {
                    name = Some(value.trim().to_owned());
                } else if let Some(value) = line.strip_prefix("data:") {
                    data.push_str(value.trim_start());
                }
            }
            let value = serde_json::from_str(&data).unwrap_or(Value::String(data));
            (name, value)
        })
        .collect()
}

/// A stdio MCP server in inline Python: one `echo` tool (annotated read-only) that returns its
/// arguments as sorted JSON text.
pub const FAKE_MCP_SERVER: &str = r#"
import sys, json
def send(o):
    sys.stdout.write(json.dumps(o) + "\n")
    sys.stdout.flush()
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    m = json.loads(line)
    method, i, p = m.get("method"), m.get("id"), m.get("params") or {}
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": i, "result": {"protocolVersion": "2025-06-18",
              "capabilities": {"tools": {}}, "serverInfo": {"name": "smoke-fake", "version": "0.1"}}})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": i, "result": {"tools": [
              {"name": "echo", "description": "Echo", "inputSchema": {"type": "object"},
               "annotations": {"readOnlyHint": True, "destructiveHint": False}}]}})
    elif method == "tools/call":
        text = json.dumps(p.get("arguments") or {}, sort_keys=True)
        send({"jsonrpc": "2.0", "id": i, "result": {"content": [{"type": "text", "text": text}]}})
    elif i is not None:
        send({"jsonrpc": "2.0", "id": i, "error": {"code": -32601, "message": "Method not found"}})
"#;
