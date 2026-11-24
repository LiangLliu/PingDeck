//! Tests of the MCP client against a fake stdio server (python3, required) and a fake
//! streamable-HTTP server on a loopback socket.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::*;
use crate::agent::tools::AgentTool;
use pingdeck_llm::llm::hub::HubImage;

const FAKE_SERVER: &str = r#"
import sys, json, os, time
cancelled = []
def send(o):
    sys.stdout.write(json.dumps(o) + "\n")
    sys.stdout.flush()
def reply(i, r):
    send({"jsonrpc": "2.0", "id": i, "result": r})
def text(s, err=False):
    return {"content": [{"type": "text", "text": s}], "isError": err}
print("not json, ignored")
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    m = json.loads(line)
    method = m.get("method")
    i = m.get("id")
    p = m.get("params") or {}
    if method == "initialize":
        only = os.environ.get("MCP_ONLY_VERSION")
        if only and p.get("protocolVersion") != only:
            send({"jsonrpc": "2.0", "id": i, "error": {"code": -32602, "message": "Unsupported protocol version",
                  "data": {"supported": [only], "requested": p.get("protocolVersion")}}})
            continue
        caps = {"tools": {}}
        if os.environ.get("MCP_RESOURCES"):
            caps["resources"] = {}
        reply(i, {"protocolVersion": only or "2025-06-18", "capabilities": caps,
                  "serverInfo": {"name": "fake", "version": "1.2.3"}})
    elif method == "notifications/cancelled":
        cancelled.append(p.get("requestId"))
    elif method == "tools/list":
        if p.get("cursor") is None:
            reply(i, {"tools": [{"name": "echo", "description": "Echo", "inputSchema": {"type": "object"},
                                 "annotations": {"readOnlyHint": True}}], "nextCursor": "page2"})
        else:
            reply(i, {"tools": [
                {"name": "fail", "description": "Fails", "inputSchema": {"type": "object"},
                 "annotations": {"destructiveHint": False}},
                {"name": "slow", "inputSchema": {"type": "object"}},
                {"description": "nameless, skipped"}]})
    elif method == "tools/call":
        name = p.get("name")
        a = p.get("arguments") or {}
        if name == "echo":
            reply(i, {"content": [{"type": "text", "text": json.dumps(a, sort_keys=True)},
                                  {"type": "image", "data": "AAAA", "mimeType": "image/png"}]})
        elif name == "fail":
            reply(i, text("nope", True))
        elif name == "slow":
            time.sleep(a.get("seconds", 1))
            reply(i, text("done"))
        elif name == "pid":
            reply(i, text(str(os.getpid())))
        elif name == "cwd":
            reply(i, text(os.getcwd()))
        elif name == "env":
            reply(i, text(os.environ.get("MCP_TEST_VAR", "")))
        elif name == "big":
            reply(i, text("x" * (2 * 1024 * 1024 + 1024)))
        elif name == "structured":
            reply(i, {"content": [], "structuredContent": {"a": 1}})
        elif name == "cancels":
            reply(i, text(json.dumps(cancelled)))
        elif name == "ping_me":
            send({"jsonrpc": "2.0", "id": 900, "method": "ping"})
            reply(i, text(json.dumps(json.loads(sys.stdin.readline()).get("result"))))
        elif name == "roots_me":
            send({"jsonrpc": "2.0", "id": 901, "method": "roots/list"})
            reply(i, text(str(json.loads(sys.stdin.readline())["error"]["code"])))
        elif name == "progress":
            tok = (p.get("_meta") or {}).get("progressToken")
            for n in range(int(a.get("seconds", 1) / 0.25) + 1):
                time.sleep(0.25)
                send({"jsonrpc": "2.0", "method": "notifications/progress",
                      "params": {"progressToken": tok, "progress": n}})
            reply(i, text("done"))
        elif name == "announce":
            send({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
            reply(i, text("ok"))
        elif name == "exit":
            os._exit(0)
        else:
            send({"jsonrpc": "2.0", "id": i, "error": {"code": -32602, "message": "Unknown tool: " + str(name)}})
    elif method == "ping":
        reply(i, {})
    elif method == "resources/list":
        reply(i, {"resources": [{"uri": "file:///notes.md", "name": "notes", "mimeType": "text/markdown"}]})
    elif method == "resources/read":
        reply(i, {"contents": [{"uri": p.get("uri"), "mimeType": "text/markdown", "text": "notes body"}]})
    elif i is not None:
        send({"jsonrpc": "2.0", "id": i, "error": {"code": -32601, "message": "Method not found"}})
"#;

const LONG: Duration = Duration::from_secs(10);

/// The stdio tests drive a `python3` fake server; without it they fail instead of passing untested.
fn require_python() {
    let found = Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        found,
        "the MCP stdio tests need python3 on PATH for their fake server"
    );
}

fn python_config(script: &str, env: &[(&str, &str)]) -> McpTransportConfig {
    McpTransportConfig::Stdio {
        command: "python3".to_owned(),
        args: vec!["-c".to_owned(), script.to_owned()],
        env: env
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
        env_allow: Vec::new(),
        cwd: None,
    }
}

async fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
    // 30 s in all (20 ms apart): only a failing test runs the loop out, however loaded the machine is.
    for _ in 0..1500 {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    condition()
}

fn process_exists(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .output()
        .is_ok_and(|output| output.status.success())
}

async fn connect_fake(env: &[(&str, &str)]) -> McpConnection {
    McpConnection::connect(&python_config(FAKE_SERVER, env), None, None, LONG)
        .await
        .unwrap()
}

#[tokio::test]
async fn stdio_connect_list_tools_call_cancel_and_manager_lifecycle() {
    let missing = McpTransportConfig::Stdio {
        command: "pingdeck-no-such-command".to_owned(),
        args: Vec::new(),
        env: BTreeMap::new(),
        env_allow: Vec::new(),
        cwd: None,
    };
    assert!(matches!(
        McpConnection::connect(&missing, None, None, LONG).await,
        Err(AppError::Io { .. })
    ));

    require_python();
    let connection = connect_fake(&[("MCP_TEST_VAR", "from-env")]).await;
    assert_eq!(
        connection.server_info(),
        &McpServerInfo {
            name: "fake".to_owned(),
            version: "1.2.3".to_owned(),
            protocol_version: "2025-06-18".to_owned(),
            resources: false,
        }
    );
    assert!(connection.is_alive());

    let tools = connection.list_tools(LONG).await.unwrap();
    assert_eq!(
        tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["echo", "fail", "slow"]
    );
    assert!(tools[0].read_only && !tools[0].destructive);

    let echo = connection
        .call_tool("echo", json!({"msg": "hi"}), LONG, None)
        .await
        .unwrap();
    assert_eq!(
        echo.parts,
        [
            HubPart::text("{\"msg\": \"hi\"}"),
            HubPart::Image {
                image: HubImage::Base64 {
                    media_type: "image/png".to_owned(),
                    data: "AAAA".to_owned(),
                }
            },
        ]
    );
    let fail = connection
        .call_tool("fail", Value::Null, LONG, None)
        .await
        .unwrap();
    assert!(fail.is_error);
    assert_eq!(
        connection
            .call_tool("structured", json!({}), LONG, None)
            .await
            .unwrap()
            .text(),
        "{\"a\":1}"
    );
    assert_eq!(
        connection
            .call_tool("env", json!({}), LONG, None)
            .await
            .unwrap()
            .text(),
        "from-env"
    );
    assert_eq!(
        connection
            .call_tool("ping_me", json!({}), LONG, None)
            .await
            .unwrap()
            .text(),
        "{}"
    );
    assert_eq!(
        connection
            .call_tool("roots_me", json!({}), LONG, None)
            .await
            .unwrap()
            .text(),
        "-32601"
    );
    assert!(
        connection
            .call_tool("big", json!({}), LONG, None)
            .await
            .unwrap()
            .text()
            .ends_with("… [truncated]")
    );
    assert_eq!(
        connection
            .call_tool("missing", json!({}), LONG, None)
            .await
            .err()
            .unwrap(),
        AppError::network("Unknown tool: missing")
    );

    // Timeout and cancellation notify the server
    let err = connection
        .call_tool(
            "slow",
            json!({"seconds": 0.3}),
            Duration::from_millis(80),
            None,
        )
        .await
        .err()
        .unwrap();
    assert!(err.message().contains("timed out"));
    let notices = connection
        .call_tool("cancels", json!({}), LONG, None)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&notices.text())
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let token = CancelToken::new();
    let remote = token.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        remote.cancel();
    });
    assert!(
        connection
            .call_tool("slow", json!({"seconds": 0.5}), LONG, Some(&token))
            .await
            .err()
            .unwrap()
            .is_cancelled()
    );
    let notices = connection
        .call_tool("cancels", json!({}), LONG, None)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&notices.text())
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        connection
            .call_tool("echo", json!({}), LONG, Some(&token))
            .await
            .err()
            .unwrap()
            .is_cancelled()
    );
    connection.shutdown();

    // Manager reuse, reconnect on exit/config change, and disconnect
    let manager = McpManager::new();
    let cfg = python_config(FAKE_SERVER, &[]);
    let first = manager.connection("a", &cfg, None, LONG).await.unwrap();
    let again = manager.connection("a", &cfg, None, LONG).await.unwrap();
    assert!(Arc::ptr_eq(&first, &again));
    let pid = first
        .call_tool("pid", json!({}), LONG, None)
        .await
        .unwrap()
        .text();
    let _ = first.call_tool("exit", json!({}), LONG, None).await;
    assert!(wait_until(|| !first.is_alive()).await);
    let revived = manager.connection("a", &cfg, None, LONG).await.unwrap();
    assert!(!Arc::ptr_eq(&first, &revived) && revived.is_alive());
    assert!(wait_until(|| !process_exists(&pid)).await);
    manager.disconnect("a");
    assert!(!revived.is_alive());
    manager.disconnect_all();
}

// ---- fake streamable-HTTP server ----

struct Logged {
    verb: String,
    headers: HashMap<String, String>,
    body: Value,
}

struct FakeHttp {
    url: String,
    log: Arc<Mutex<Vec<Logged>>>,
    session: Arc<Mutex<u32>>,
    fail_reinit: Arc<Mutex<bool>>,
    reject: Arc<Mutex<Option<u16>>>,
}

impl FakeHttp {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let log: Arc<Mutex<Vec<Logged>>> = Arc::default();
        let session: Arc<Mutex<u32>> = Arc::default();
        let fail_reinit: Arc<Mutex<bool>> = Arc::default();
        let reject: Arc<Mutex<Option<u16>>> = Arc::default();
        let (shared, current, failing, rejecting) = (
            Arc::clone(&log),
            Arc::clone(&session),
            Arc::clone(&fail_reinit),
            Arc::clone(&reject),
        );
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (log, session, failing, rejecting) = (
                    Arc::clone(&shared),
                    Arc::clone(&current),
                    Arc::clone(&failing),
                    Arc::clone(&rejecting),
                );
                std::thread::spawn(move || serve(stream, &log, &session, &failing, &rejecting));
            }
        });
        Self {
            url,
            log,
            session,
            fail_reinit,
            reject,
        }
    }

    fn config_with(&self, headers: &[(&str, &str)]) -> McpTransportConfig {
        McpTransportConfig::Http {
            url: self.url.clone(),
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    fn reject(&self, status: Option<u16>) {
        *lock(&self.reject) = status;
    }

    fn config(&self) -> McpTransportConfig {
        self.config_with(&[("X-Test", "yes")])
    }

    fn expire_session(&self) {
        *lock(&self.session) += 1;
    }

    fn fail_reinitialize(&self) {
        *lock(&self.fail_reinit) = true;
    }

    fn logged(&self, method: &str) -> Vec<(HashMap<String, String>, Value)> {
        lock(&self.log)
            .iter()
            .filter(|entry| entry.verb == "POST" && entry.body["method"] == method)
            .map(|entry| (entry.headers.clone(), entry.body.clone()))
            .collect()
    }

    fn deletes(&self) -> Vec<HashMap<String, String>> {
        lock(&self.log)
            .iter()
            .filter(|entry| entry.verb == "DELETE")
            .map(|entry| entry.headers.clone())
            .collect()
    }
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, extra: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

fn serve(
    mut stream: TcpStream,
    log: &Mutex<Vec<Logged>>,
    session: &Mutex<u32>,
    fail_reinit: &Mutex<bool>,
    reject: &Mutex<Option<u16>>,
) {
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(clone);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let verb = line
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    let mut headers = HashMap::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let method = body["method"].as_str().unwrap_or_default().to_owned();
    let id = body["id"].clone();
    let current = *lock(session);
    let valid = headers
        .get("mcp-session-id")
        .is_some_and(|v| *v == format!("sess-{current}"));
    let session_ok = method == "initialize" || valid;
    let stale = headers.contains_key("mcp-session-id") && !valid;
    let auth = headers.get("authorization").cloned().unwrap_or_default();
    lock(log).push(Logged {
        verb: verb.clone(),
        headers,
        body: body.clone(),
    });
    if verb == "DELETE" {
        return respond(&mut stream, "200 OK", "text/plain", "", "");
    }
    if let Some(status) = *lock(reject) {
        return respond(
            &mut stream,
            &format!("{status} Rejected"),
            "text/plain",
            "",
            &format!("denied {auth}"),
        );
    }
    if !session_ok {
        let status = if stale {
            "404 Not Found"
        } else {
            "400 Bad Request"
        };
        return respond(&mut stream, status, "text/plain", "", "bad session");
    }
    let json_reply = |stream: &mut TcpStream, extra: &str, result: Value| {
        let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
        respond(
            stream,
            "200 OK",
            "application/json",
            extra,
            &reply.to_string(),
        );
    };
    match method.as_str() {
        "initialize" if *lock(fail_reinit) => respond(
            &mut stream,
            "500 Internal Server Error",
            "text/plain",
            "",
            "no",
        ),
        "initialize" => {
            let n = {
                let mut current = lock(session);
                *current += 1;
                *current
            };
            json_reply(
                &mut stream,
                &format!("Mcp-Session-Id: sess-{n}\r\n"),
                json!({"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "fake-http", "version": "9"}}),
            );
        }
        m if m.starts_with("notifications/") => {
            respond(&mut stream, "202 Accepted", "text/plain", "", "")
        }
        "tools/list" => {
            let note = json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {}});
            let reply = json!({"jsonrpc": "2.0", "id": id, "result": {"tools": [
                {"name": "echo", "description": "d", "inputSchema": {"type": "object"}, "annotations": {"readOnlyHint": true}}]}});
            let head =
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(head.as_bytes());
            let _ = stream
                .write_all(format!("event: message\ndata: {note}\n\ndata: {reply}\n\n").as_bytes());
        }
        "tools/call" => match body["params"]["name"].as_str() {
            Some("slow") => {
                std::thread::sleep(Duration::from_millis(400));
                json_reply(
                    &mut stream,
                    "",
                    json!({"content": [{"type": "text", "text": "done"}]}),
                );
            }
            Some("huge") => respond(
                &mut stream,
                "200 OK",
                "application/json",
                "",
                &" ".repeat(http::MAX_BODY_BYTES + 1),
            ),
            Some("huge_event") => {
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n");
                let _ = stream.write_all(format!("data: {}\n\n", "x".repeat(9 << 20)).as_bytes());
            }
            Some("huge_event_lines") => {
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n");
                for _ in 0..9 {
                    let _ = stream.write_all(format!("data: {}\n", "x".repeat(1 << 20)).as_bytes());
                }
                let _ = stream.write_all(b"\n");
            }
            Some("boom") => {
                let reply = json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": "kaboom"}});
                respond(
                    &mut stream,
                    "200 OK",
                    "application/json",
                    "",
                    &reply.to_string(),
                );
            }
            _ => json_reply(
                &mut stream,
                "",
                json!({"content": [{"type": "text", "text": body["params"]["arguments"].to_string()}]}),
            ),
        },
        _ => {
            let reply = json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Method not found"}});
            respond(
                &mut stream,
                "200 OK",
                "application/json",
                "",
                &reply.to_string(),
            );
        }
    }
}

#[tokio::test]
async fn http_handshake_sse_calls_404_retry_oversized_and_shutdown() {
    for url in ["ftp://example.com/mcp", "not a url"] {
        let cfg = McpTransportConfig::Http {
            url: url.to_owned(),
            headers: BTreeMap::new(),
        };
        assert!(matches!(
            McpConnection::connect(&cfg, None, None, LONG).await,
            Err(AppError::Validation { .. })
        ));
    }

    let server = FakeHttp::start();
    let connection = McpConnection::connect(&server.config(), None, None, LONG)
        .await
        .unwrap();
    assert_eq!(connection.server_info().name, "fake-http");
    let tools = connection.list_tools(LONG).await.unwrap();
    assert_eq!(tools.len(), 1);

    let echo = connection
        .call_tool("echo", json!({"k": 1}), LONG, None)
        .await
        .unwrap();
    assert_eq!(echo.text(), "{\"k\":1}");
    let init = server.logged("initialize");
    assert_eq!(init[0].1["params"]["protocolVersion"], "2025-06-18");
    assert_eq!(init[0].1["params"]["clientInfo"]["name"], "pingdeck");
    assert!(!init[0].0.contains_key("mcp-session-id"));
    for method in ["notifications/initialized", "tools/list", "tools/call"] {
        let (headers, _) = &server.logged(method)[0];
        assert_eq!(headers["mcp-session-id"], "sess-1");
        assert_eq!(headers["mcp-protocol-version"], "2025-06-18");
        assert_eq!(headers["x-test"], "yes");
        assert_eq!(headers["content-type"], "application/json");
        assert_eq!(headers["accept"], "application/json, text/event-stream");
    }
    assert_eq!(
        connection
            .call_tool("boom", json!({}), LONG, None)
            .await
            .err()
            .unwrap(),
        AppError::network("kaboom")
    );
    for (name, message) in [
        ("huge", "MCP server response is over 16 MiB"),
        (
            "huge_event",
            "MCP event stream rejected: an SSE line exceeds the 8 MiB limit",
        ),
        (
            "huge_event_lines",
            "MCP event stream rejected: an SSE event's data exceeds the 8 MiB limit",
        ),
    ] {
        assert_eq!(
            connection
                .call_tool(name, json!({}), LONG, None)
                .await
                .err()
                .unwrap()
                .message(),
            message
        );
    }

    // Timeout sends notifications/cancelled
    assert!(
        connection
            .call_tool("slow", json!({}), Duration::from_millis(80), None)
            .await
            .is_err()
    );
    assert!(wait_until(|| !server.logged("notifications/cancelled").is_empty()).await);

    // Re-initialize once after 404
    server.expire_session();
    assert_eq!(
        connection
            .call_tool("echo", json!({"k": 2}), LONG, None)
            .await
            .unwrap()
            .text(),
        "{\"k\":2}"
    );
    assert_eq!(server.logged("initialize").len(), 2);
    connection.shutdown();
    assert!(wait_until(|| !server.deletes().is_empty()).await);

    let conn2 = McpConnection::connect(&server.config(), None, None, LONG)
        .await
        .unwrap();
    server.expire_session();
    server.fail_reinitialize();
    assert!(
        conn2
            .call_tool("echo", json!({}), LONG, None)
            .await
            .err()
            .unwrap()
            .message()
            .contains("HTTP 500")
    );
}

// ---- manager cache, status, adapter, env, and caps ----

fn manager_with_clock() -> (McpManager, Arc<AtomicU64>) {
    let base = Instant::now();
    let seconds = Arc::new(AtomicU64::new(0));
    let clock: Clock = {
        let seconds = Arc::clone(&seconds);
        Arc::new(move || base + Duration::from_secs(seconds.load(Ordering::SeqCst)))
    };
    (McpManager::with_clock(clock), seconds)
}

fn server_entry(name: &str, trusted: bool) -> crate::agent::model::AgentMcpServer {
    crate::agent::model::AgentMcpServer {
        id: format!("id-{name}"),
        name: name.to_owned(),
        transport: McpTransportConfig::Http {
            url: "http://127.0.0.1:9/mcp".to_owned(),
            headers: BTreeMap::new(),
        },
        enabled: true,
        trusted,
        updated_at_ms: 0,
    }
}

fn tool(name: &str, read_only: bool, destructive: bool) -> McpTool {
    McpTool {
        name: name.to_owned(),
        description: "d".to_owned(),
        input_schema: json!({"type": "object"}),
        read_only,
        destructive,
    }
}

#[tokio::test]
async fn manager_cache_status_and_adapter_calls() {
    let server = FakeHttp::start();
    let (manager, clock) = manager_with_clock();
    let config = server.config();

    let tools = manager
        .ensure_tools("a", &config, None, LONG)
        .await
        .unwrap();
    assert_eq!(manager.cached_tools("a"), Some(tools));
    clock.fetch_add(299, Ordering::SeqCst);
    assert!(manager.cached_tools("a").is_some());
    clock.fetch_add(1, Ordering::SeqCst);
    // Expired while the connection lives: the old list is served and marked for a refresh.
    assert!(manager.cached_tools("a").is_some() && manager.tools_stale("a"));
    manager
        .ensure_tools("a", &config, None, LONG)
        .await
        .unwrap();
    assert_eq!(server.logged("tools/list").len(), 2);
    assert!(!manager.tools_stale("a"));
    manager.disconnect("a");
    assert!(manager.cached_tools("a").is_none());

    // 401 -> NeedsAuth, 403 -> Error, secrets scrubbed
    let secret = "sup3r-secret-token-1";
    let auth_cfg = server.config_with(&[("Authorization", &format!("Bearer {secret}"))]);
    server.reject(Some(401));
    clock.fetch_add(61, Ordering::SeqCst);
    let err = manager
        .connection("b", &auth_cfg, None, LONG)
        .await
        .err()
        .unwrap();
    assert!(!err.message().contains(secret));
    assert_eq!(manager.status("b"), McpServerStatus::NeedsAuth);
    server.reject(Some(403));
    let error = manager
        .connection("c", &auth_cfg, None, LONG)
        .await
        .err()
        .unwrap();
    assert!(!error.message().contains(secret));
    assert!(matches!(manager.status("c"), McpServerStatus::Error { .. }));

    server.reject(None);
    let agent = crate::agent::runtime::AgentRuntime::for_test();
    let settings = pingdeck_model::settings::Settings::default();
    let cancel = CancelToken::new();
    let parts = crate::agent::tools::TestParts::default();
    let mut ctx = crate::agent::tools::ToolContext::for_test(None, &settings, &cancel, &parts);
    ctx.agent = Some(&agent);
    let entry = server_entry("web", true);
    let adapter = McpToolAdapter::new(&entry, server.config(), tool("echo", true, false));
    let ok = adapter.call(&ctx, json!({"k": 1})).await;
    assert_eq!(ok.text_content(), "{\"k\":1}");
    assert!(adapter.inspect(&ctx, &Value::Null).is_ok());
    assert!(matches!(
        adapter.inspect(&ctx, &json!([1])),
        Err(AppError::Validation { .. })
    ));
    let failing = McpToolAdapter::new(&entry, server.config(), tool("boom", true, false));
    let failed = failing.call(&ctx, json!({})).await;
    assert!(failed.is_error);
    assert_eq!(failed.text_content(), "kaboom");
    cancel.cancel();
    let cancelled = adapter.call(&ctx, json!({})).await;
    assert!(cancelled.is_error);
    assert_eq!(cancelled.text_content(), "Request cancelled");
}

#[test]
fn tool_names_trust_hints_server_env_caps_and_secret_scrubbing() {
    use crate::agent::tools::ToolRegistry;

    assert_eq!(mcp_tool_name("files", "read file"), "mcp__files__read_file");
    let long = "a_very_long_tool_name_".repeat(4);
    assert_eq!(mcp_tool_name("files", &format!("{long}_one")).len(), 64);
    assert_ne!(
        mcp_tool_name("files", &format!("{long}_one")),
        mcp_tool_name("files", &format!("{long}_two"))
    );
    assert_eq!(mcp_server_prefix("files"), "mcp__files__");

    let (dotted, underscored) = (server_entry("my.srv", true), server_entry("my_srv", false));
    assert_eq!(mcp_name_key(&dotted.name), mcp_name_key(&underscored.name));
    assert_eq!(
        effective_hints(&dotted, &tool("read", true, false)),
        (true, false)
    );
    assert_eq!(
        effective_hints(&underscored, &tool("read", true, false)),
        (false, true)
    );
    let mut registry = ToolRegistry::new();
    let transport = dotted.transport.clone();
    assert!(registry.register_unique(McpToolAdapter::new(
        &dotted,
        transport.clone(),
        tool("read", true, false)
    )));
    assert!(!registry.register_unique(McpToolAdapter::new(
        &underscored,
        transport,
        tool("read", false, true)
    )));

    // Server environment filtering and proxy stripping
    let parent = [
        ("PATH", "/bin"),
        ("LC_ALL", "C"),
        ("OPENAI_API_KEY", "k"),
        ("EXTRA", "kept"),
    ]
    .map(|(n, v)| (OsString::from(n), OsString::from(v)));
    let env: BTreeMap<String, String> = stdio::server_env(
        parent.into_iter(),
        &["extra".to_owned()],
        &BTreeMap::from([("MINE".to_owned(), "1".to_owned())]),
        Some("http://user:pw@proxy.test:3128"),
        None,
    )
    .into_iter()
    .map(|(k, v)| {
        (
            k.to_string_lossy().into_owned(),
            v.to_string_lossy().into_owned(),
        )
    })
    .collect();
    assert!(
        env.contains_key("EXTRA")
            && env.contains_key("MINE")
            && !env.contains_key("OPENAI_API_KEY")
    );
    assert_eq!(env["HTTP_PROXY"], "http://proxy.test:3128");

    // Tool list caps and secret redaction in Debug/errors
    let items = [
        json!({"name": "fat", "description": "é".repeat(MAX_TOOL_DESCRIPTION_BYTES), "inputSchema": {"type": "object"}}),
        json!({"name": "huge_schema", "description": "d", "inputSchema": {"type": "object", "d": "x".repeat(MAX_TOOL_SCHEMA_BYTES)}}),
        json!({"name": "x".repeat(MAX_TOOL_NAME_BYTES + 1), "inputSchema": {"type": "object"}}),
        // A bad root schema skips only this tool; a bare one is normalized.
        json!({"name": "listy", "inputSchema": {"type": "array"}}),
        json!({"name": "bare", "description": "hid\u{202e}den"}),
    ];
    let (parsed, skipped) = parse_tools(&items);
    assert_eq!(parsed.len(), 2);
    assert_eq!(skipped.len(), 3);
    assert!(skipped.iter().any(|why| why.starts_with("listy: ")));
    // Sorted deterministically by name so provider prompt-cache prefixes never bust.
    assert_eq!(parsed[0].name, "bare");
    assert_eq!(parsed[1].name, "fat");
    assert_eq!(
        parsed[0].input_schema,
        json!({"type": "object", "properties": {}})
    );
    assert!(!parsed[0].description.contains('\u{202e}'));
    assert!(parsed[1].description.ends_with('…'));
    assert!(parsed[1].description.len() <= MAX_TOOL_DESCRIPTION_BYTES + '…'.len_utf8());
    let many: Vec<_> = (0..MAX_TOOLS + 1)
        .map(|i| json!({"name": format!("t{i}"), "inputSchema": {"type": "object"}}))
        .collect();
    assert_eq!(parse_tools(&many).0.len(), MAX_TOOLS);
    let heavy: Vec<_> = (0..MAX_TOOLS).map(|i| json!({"name": format!("h{i}"), "inputSchema": {"type": "object", "d": "x".repeat(10 * 1024)}})).collect();
    let kept = parse_tools(&heavy).0.len();
    assert!(kept > 0 && kept * 10 * 1024 <= MAX_TOOLS_TOTAL_BYTES);

    let cfg = McpTransportConfig::Http {
        url: "https://user:url-secret@example.com/mcp".to_owned(),
        headers: BTreeMap::from([(
            "Authorization".to_owned(),
            "Bearer tok-abcdef123".to_owned(),
        )]),
    };
    assert!(!format!("{cfg:?}").contains("url-secret"));
    let clean = scrub(
        AppError::network("bad: tok-abcdef123"),
        &secret_values(&cfg),
    );
    assert!(!clean.message().contains("tok-abcdef123"));
}

const STUB_PRELUDE: &str = r#"
import sys, json, os, subprocess, time
def handshake(name):
    sys.stdin.readline()
    reply = {"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": "2025-06-18",
             "capabilities": {}, "serverInfo": {"name": name, "version": str(os.getpid())}}}
    sys.stdout.write(json.dumps(reply) + "\n")
    sys.stdout.flush()
    sys.stdin.readline()
"#;

#[tokio::test]
async fn stdio_bounds_lines_shutdown_blocked_writers_and_kills_process_groups() {
    require_python();
    let stub = |body: &str| python_config(&format!("{STUB_PRELUDE}\n{body}\n"), &[]);
    let connection = Arc::new(
        McpConnection::connect(&stub("handshake('x')\ntime.sleep(300)"), None, None, LONG)
            .await
            .unwrap(),
    );
    let pid = connection.server_info().version.clone();
    let call = tokio::spawn({
        let connection = Arc::clone(&connection);
        async move {
            connection
                .call_tool("echo", json!({"big": "x".repeat(8 << 20)}), LONG, None)
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    connection.shutdown();
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(wait_until(|| !process_exists(&pid)).await);
    assert!(call.await.unwrap().is_err());

    let group = McpConnection::connect(
        &stub("grandchild = subprocess.Popen(['sleep', '300'])\nhandshake(str(grandchild.pid))\ntime.sleep(300)"),
        None,
        None,
        LONG,
    )
    .await
    .unwrap();
    let child = group.server_info().version.clone();
    let grandchild = group.server_info().name.clone();
    assert!(process_exists(&child) && process_exists(&grandchild));
    group.shutdown();
    assert!(wait_until(|| !process_exists(&child) && !process_exists(&grandchild)).await);

    let oversized = McpConnection::connect(
        &stub("handshake('x')\nsys.stdin.readline()\nwhile True:\n    sys.stdout.write('y' * 65536)\n    sys.stdout.flush()"),
        None,
        None,
        LONG,
    )
    .await
    .unwrap();
    assert_eq!(
        oversized
            .call_tool("echo", json!({}), LONG, None)
            .await
            .err()
            .unwrap(),
        AppError::network("MCP server sent a line over 16 MiB")
    );
    assert!(!oversized.is_alive());
}

#[tokio::test]
async fn http_redirects_never_send_credentials_to_another_host() {
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    target.set_nonblocking(true).unwrap();
    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    let location = format!("http://{}/steal", target.local_addr().unwrap());
    let url = format!("http://{}/mcp", origin.local_addr().unwrap());
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = origin.accept() {
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer);
            respond(
                &mut stream,
                "307 Temporary Redirect",
                "text/plain",
                &format!("Location: {location}\r\n"),
                "",
            );
        }
    });
    let config = McpTransportConfig::Http {
        url,
        headers: BTreeMap::from([("X-Api-Key".to_owned(), "key-123".to_owned())]),
    };
    let error = McpConnection::connect(&config, None, None, LONG)
        .await
        .err()
        .unwrap();
    assert!(error.message().contains("307"));
    assert!(matches!(target.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
}

const ENV_SERVER: &str = r#"
import sys, json, os
for n in range(250):
    sys.stderr.write("line %d token=%s\n" % (n, os.environ.get("MCP_SECRET", "")))
sys.stderr.flush()
for line in sys.stdin:
    m = json.loads(line)
    i = m.get("id")
    if m.get("method") == "initialize":
        result = {"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "e", "version": "1"}}
    elif m.get("method") == "tools/call":
        result = {"content": [{"type": "text", "text": json.dumps({"env": sorted(os.environ), "cwd": os.getcwd()})}]}
    else:
        continue
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": i, "result": result}) + "\n")
    sys.stdout.flush()
"#;

#[tokio::test]
async fn stdio_environment_cwd_redacted_log_ring_and_status_replay() {
    require_python();
    let manager = McpManager::new();
    let early = Arc::new(Mutex::new(Vec::new()));
    manager
        .subscribe(
            None,
            Arc::new({
                let early = Arc::clone(&early);
                move |event: pingdeck_model::events::Sequenced<McpStreamEvent>| {
                    lock(&early).push(event);
                    true
                }
            }),
        )
        .unwrap();
    let config = python_config(
        ENV_SERVER,
        &[("MCP_SECRET", "supersecret-value-123"), ("MCP_OWN", "1")],
    );
    let connection = manager
        .connection("srv/1", &config, None, LONG)
        .await
        .unwrap();
    let seen: Value = serde_json::from_str(
        &connection
            .call_tool("env", json!({}), LONG, None)
            .await
            .unwrap()
            .text(),
    )
    .unwrap();
    let names: Vec<_> = seen["env"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(!names.iter().any(|name| name.starts_with("CARGO_")));
    assert!(names.contains(&"MCP_OWN") && names.contains(&"PATH"));
    let neutral = std::fs::canonicalize(seen["cwd"].as_str().unwrap()).unwrap();
    assert_eq!(
        neutral,
        std::fs::canonicalize(std::env::temp_dir().join("pingdeck-mcp/srv_1")).unwrap()
    );

    assert!(
        wait_until(|| manager
            .log("srv/1")
            .last()
            .is_some_and(|line| line.starts_with("line 249")))
        .await
    );
    let log = manager.log("srv/1");
    assert_eq!(log.len(), manager::LOG_LINES);
    assert!(log[0].starts_with("line 50 "));
    assert!(
        log.iter()
            .all(|line| !line.contains("supersecret-value-123"))
    );
    assert!(log[0].contains('•'));

    let directory = std::env::temp_dir().join(format!("pingdeck-mcp-cwd-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let mut chosen = config;
    if let McpTransportConfig::Stdio { env_allow, cwd, .. } = &mut chosen {
        *env_allow = vec!["CARGO_MANIFEST_DIR".to_owned()];
        *cwd = Some(directory.to_string_lossy().into_owned());
    }
    let connection = manager
        .connection("srv/1", &chosen, None, LONG)
        .await
        .unwrap();
    let seen: Value = serde_json::from_str(
        &connection
            .call_tool("env", json!({}), LONG, None)
            .await
            .unwrap()
            .text(),
    )
    .unwrap();
    assert!(
        seen["env"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "CARGO_MANIFEST_DIR")
    );
    assert_eq!(
        std::fs::canonicalize(seen["cwd"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&directory).unwrap()
    );
    manager.mark_disabled("srv/1");
    let late = Arc::new(Mutex::new(Vec::new()));
    manager
        .subscribe(
            None,
            Arc::new({
                let late = Arc::clone(&late);
                move |event: pingdeck_model::events::Sequenced<McpStreamEvent>| {
                    lock(&late).push(event);
                    true
                }
            }),
        )
        .unwrap();
    assert_eq!(*lock(&early), *lock(&late));
    manager.forget("srv/1");
    assert!(manager.log("srv/1").is_empty());
    std::fs::remove_dir(&directory).unwrap();
}

/// Saved review baselines hold these fingerprints, so any change to the field order, tags or
/// hashing of a stored config shows up here before it silently invalidates every baseline.
#[test]
fn config_fingerprint_is_pinned_for_fixed_configs() {
    let stdio = McpTransportConfig::Stdio {
        command: "npx".to_owned(),
        args: vec!["-y".to_owned(), "@acme/files".to_owned()],
        env: BTreeMap::from([
            ("MODE".to_owned(), "ro".to_owned()),
            (
                "API_KEY".to_owned(),
                "{{secret:mcp.srv_files.env.API_KEY}}".to_owned(),
            ),
        ]),
        env_allow: vec!["PATH".to_owned()],
        cwd: Some("/work".to_owned()),
    };
    let http = McpTransportConfig::Http {
        url: "https://mcp.example.test/mcp".to_owned(),
        headers: BTreeMap::from([(
            "Authorization".to_owned(),
            "{{secret:mcp.srv_web.header.Authorization}}".to_owned(),
        )]),
    };
    assert_eq!(config_fingerprint(&stdio), "3ac6d25abf8c9ba0");
    assert_eq!(config_fingerprint(&http), "b9d3d914a4697a26");
}

#[test]
fn server_env_puts_the_login_path_over_the_inherited_one_and_an_explicit_path_wins() {
    let path_of = |own: &[(&str, &str)]| {
        let parent = [("PATH", "/usr/bin"), ("HOME", "/h")]
            .map(|(n, v)| (OsString::from(n), OsString::from(v)));
        let own: BTreeMap<String, String> = own
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect();
        let vars = stdio::server_env(
            parent.into_iter(),
            &[],
            &own,
            None,
            Some(std::ffi::OsStr::new("/login/bin:/usr/bin")),
        );
        let paths: Vec<String> = vars
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("PATH"))
            .map(|(_, value)| value.to_string_lossy().into_owned())
            .collect();
        paths
    };
    assert_eq!(path_of(&[]), ["/login/bin:/usr/bin"]);
    assert_eq!(path_of(&[("PATH", "/mine")]), ["/mine"]);
}

#[tokio::test]
async fn a_command_not_found_names_the_path_searched_and_the_fix() {
    let config = McpTransportConfig::Stdio {
        command: "pingdeck-no-such-mcp-server".to_owned(),
        args: Vec::new(),
        env: BTreeMap::new(),
        env_allow: Vec::new(),
        cwd: None,
    };
    let inherited = std::env::var("PATH").unwrap();
    let error = McpConnection::connect(&config, None, None, Duration::from_secs(5))
        .await
        .err()
        .unwrap();
    let text = error.message();
    assert!(
        text.contains(&format!(
            "`pingdeck-no-such-mcp-server` was not found in PATH `{inherited}`"
        )) && text.contains("env.PATH"),
        "{text}"
    );
}

#[tokio::test]
async fn a_server_refusing_our_revision_is_retried_with_one_it_names() {
    require_python();
    let connection = connect_fake(&[("MCP_ONLY_VERSION", "2024-11-05")]).await;
    assert_eq!(connection.server_info().protocol_version, "2024-11-05");
    connection.shutdown();
    let refused = McpConnection::connect(
        &python_config(FAKE_SERVER, &[("MCP_ONLY_VERSION", "1999-01-01")]),
        None,
        None,
        LONG,
    )
    .await;
    assert!(refused.is_err(), "no shared revision: the refusal stands");
}

#[tokio::test]
async fn progress_keeps_a_call_alive_list_changes_are_noticed_and_resources_read() {
    require_python();
    let config = python_config(FAKE_SERVER, &[("MCP_RESOURCES", "1")]);
    let connection = McpConnection::connect(&config, None, None, LONG)
        .await
        .unwrap();
    let short = Duration::from_millis(900);
    // Work longer than the timeout that reports progress every 0.25 s finishes.
    let done = connection
        .call_tool("progress", json!({"seconds": 1.5}), short, None)
        .await
        .unwrap();
    assert_eq!(done.text(), "done");
    // Without progress the same timeout ends it.
    let silent = connection
        .call_tool("slow", json!({"seconds": 1.5}), short, None)
        .await;
    assert!(silent.unwrap_err().message().contains("timed out"));

    assert!(!connection.tools_changed());
    connection
        .call_tool("announce", json!({}), LONG, None)
        .await
        .unwrap();
    assert!(wait_until(|| connection.tools_changed()).await);
    connection.list_tools(LONG).await.unwrap();
    assert!(!connection.tools_changed(), "a new listing clears the mark");

    assert!(connection.server_info().resources);
    let listed = connection.list_resources(LONG).await.unwrap();
    assert_eq!(listed[0].uri, "file:///notes.md");
    let read = connection
        .read_resource("file:///notes.md", LONG, None)
        .await
        .unwrap();
    assert_eq!(read.text(), "notes body");
}
