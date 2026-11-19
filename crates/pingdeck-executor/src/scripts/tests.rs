use serde_json::{Value, json};

use super::bridge::script_http_request;
use super::runtime::{TIMEOUT_PREFIX, TRUNCATION_MARKER};
use super::*;
use pingdeck_base::cancel::CANCELLED_MESSAGE;
use pingdeck_model::ai_request::AiScriptResult;
use pingdeck_model::models::{HttpMethod, KeyValue};
use pingdeck_model::{Environment, EnvironmentVariable};

fn request() -> Request {
    let mut request = Request::new("req_1", "ws_1", "Get user", "{{baseUrl}}/users/1").unwrap();
    request.headers.push(KeyValue {
        key: "Accept".to_owned(),
        value: "application/json".to_owned(),
        enabled: true,
        description: None,
    });
    request
}

fn environment() -> Environment {
    let mut environment = Environment::new("env_1", "ws_1", "Local").unwrap();
    environment.variables =
        vec![EnvironmentVariable::plain("baseUrl", "https://api.test").unwrap()];
    environment
}

fn input(scripts: Vec<(&str, &str)>) -> ScriptInput {
    ScriptInput {
        phase: ScriptPhase::PreRequest,
        scripts: scripts
            .into_iter()
            .map(|(label, source)| ScriptSource {
                label: label.to_owned(),
                source: source.to_owned(),
            })
            .collect(),
        request: request(),
        response: None,
        ai: None,
        environment: Some(environment()),
        cancel: None,
        collection_variables: Vec::new(),
        globals: Vec::new(),
        local: BTreeMap::new(),
        iteration_data: BTreeMap::new(),
        cookie_jar: Vec::new(),
        info: ScriptInfo {
            request_name: "Get user".to_owned(),
            request_id: "req_1".to_owned(),
            iteration: 0,
            iteration_count: 1,
            event_name: "prerequest".to_owned(),
        },
        limits: ScriptLimits::default(),
        confirmed: false,
    }
}

#[test]
fn pm_tests_variables_request_mutations_and_post_response_work() {
    let output = run_scripts(input(vec![(
        "Request",
        r#"
        console.log("hello", 42);
        pm.test("status is 200", () => { pm.expect(200).to.equal(200); });
        pm.test("fails", () => { pm.expect(1).to.equal(2); });
        pm.test("async assertion", async () => { pm.expect(1).to.equal(2); });
        pm.test("after await", async () => { await Promise.resolve(); pm.expect(1).to.equal(2); });
        pm.test("rejected promise", () => Promise.reject(new Error("rejected")));
        pm.test("thenable", () => ({ then(resolve, reject) { reject(new Error("rejected")); } }));
        pm.test("function thenable", () => Object.assign(function () {}, { then() {} }));
        pm.test("synchronous object", () => ({ then: 1 }));
        pm.test("async pass", async () => { await Promise.resolve(); pm.expect(1).to.equal(1); });
        pm.environment.set("  token ", "abc");
        pm.collectionVariables.set("customer", "cus_1");
        pm.variables.set("baseUrl", "https://local.test");
        pm.variables.set("seen", pm.variables.get("baseUrl"));
        pm.variables.set("seenToken", pm.environment.get("token"));
        pm.variables.set("resolved", pm.variables.replaceIn("{{baseUrl}}/{{$randomInt}}"));
        pm.variables.set("guid", pm.variables.replaceIn("{{$guid}}"));
        pm.request.headers.add({ key: "X-Test", value: "1" });
        pm.request.headers.upsert({ key: "Accept", value: "text/plain" });
        pm.request.url.addQueryParams([{ key: "page", value: "2" }]);
        pm.variables.set("queryPage", pm.request.url.query.get("page"));
        pm.request.url.query.add([{ key: "size", value: "10" }]);
        pm.request.url.query.remove("page");
        pm.request.method = "POST";
        pm.execution.setNextRequest("Login");
        "#,
    )]));
    assert!(output.error.is_none());
    assert_eq!(output.console[0].message, "hello 42");
    // Async callbacks are recorded when they settle, after the synchronous ones.
    assert_eq!(output.tests.len(), 9);
    let by_name = |name: &str| output.tests.iter().find(|test| test.name == name).unwrap();
    assert!(by_name("status is 200").passed && by_name("synchronous object").passed);
    assert!(by_name("async pass").passed);
    for (name, error) in [
        ("fails", "expected values to be equal"),
        ("async assertion", "expected values to be equal"),
        ("after await", "expected values to be equal"),
        ("rejected promise", "rejected"),
        ("thenable", "rejected"),
        ("function thenable", "Async pm.test did not finish"),
    ] {
        let test = by_name(name);
        assert!(
            !test.passed && test.error.as_deref().is_some_and(|e| e.contains(error)),
            "{name}: {test:?}"
        );
    }
    assert!(output.mutations.environment.keys.contains("token"));
    assert!(
        output
            .collection_variables
            .iter()
            .any(|v| v.key == "customer")
    );
    assert_eq!(
        output.local.get("seen").map(String::as_str),
        Some("https://local.test")
    );
    assert_eq!(
        output.local.get("seenToken").map(String::as_str),
        Some("abc")
    );
    assert!(
        output
            .local
            .get("resolved")
            .is_some_and(|v| v.starts_with("https://local.test/"))
    );
    assert_eq!(output.local.get("guid").map(String::len), Some(36));
    assert_eq!(output.request.method, HttpMethod::Post);
    assert!(
        output
            .request
            .headers
            .iter()
            .any(|h| h.key == "X-Test" && h.value == "1")
    );
    assert!(
        output
            .request
            .headers
            .iter()
            .any(|h| h.key == "Accept" && h.value == "text/plain")
    );
    assert!(
        output
            .request
            .query_params
            .iter()
            .any(|p| p.key == "size" && p.value == "10")
    );
    assert!(!output.request.query_params.iter().any(|p| p.key == "page"));
    assert_eq!(output.local.get("queryPage").map(String::as_str), Some("2"));
    assert_eq!(output.next_request, Some(Some("Login".to_owned())));

    let mut post_input = ai_input(
        r#"
        pm.test("json name", () => {
          pm.expect(pm.response.json().name).to.equal("Ada");
          pm.response.to.have.status(200);
          pm.response.to.have.jsonBody("name", "Ada");
          pm.variables.set("text", pm.response.text());
        });
        pm.execution.setNextRequest(null);
        "#,
        r#"{"name":"Ada"}"#,
        None,
    );
    post_input
        .response
        .as_mut()
        .unwrap()
        .headers
        .push(crate::executor::ExecutionHeader {
            key: "Content-Type".into(),
            value: "application/json".into(),
        });
    let post_out = run_scripts(post_input);
    assert!(post_out.error.is_none() && post_out.tests[0].passed);
    assert_eq!(
        post_out.local.get("text").map(String::as_str),
        Some(r#"{"name":"Ada"}"#)
    );
    assert_eq!(post_out.next_request, Some(None));
}
/// A post-response input over `body_text` with the given AI run behind `pm.response.ai`.
fn ai_input(source: &str, body_text: &str, ai: Option<AiScriptResult>) -> ScriptInput {
    let mut script_input = input(vec![("Request", source)]);
    script_input.phase = ScriptPhase::PostResponse;
    script_input.info.event_name = "test".to_owned();
    script_input.ai = ai;
    script_input.response = Some(ExecutionResult {
        status_code: 200,
        status_text: "OK".to_owned(),
        url: "pingdeck-provider://deepseek/chat/completions".to_owned(),
        elapsed_ms: 233,
        attempt_count: 1,
        headers: Vec::new(),
        cookies: Vec::new(),
        body_text: body_text.to_owned(),
        body_size_bytes: body_text.len(),
        raw_body: None,
        body_handle: None,
        suggested_view: crate::executor::SuggestedView::Raw,
        body_truncated: false,
        body_total_bytes: None,
        body_incomplete: None,
        saved_body_path: None,
        body_base64: None,
        raw_body_available: true,
        sent_request: None,
        assertion_results: Vec::new(),
        script_tests: Vec::new(),
        console: Vec::new(),
        script_error: None,
        next_request: None,
        stop_run: false,
        timing: Default::default(),
        call_chain: Default::default(),
        history_id: None,
        history_saved: false,
        ai: None,
        cookie_report: Default::default(),
    });
    script_input
}

fn bible_run() -> AiScriptResult {
    use pingdeck_model::ai_request::{AiScriptToolCall, AiScriptUsage};
    AiScriptResult {
        reply: "pong".to_owned(),
        reasoning: "thinking".to_owned(),
        tool_calls: vec![AiScriptToolCall {
            id: "call_1".to_owned(),
            name: "lookup".to_owned(),
            arguments: r#"{"q":1}"#.to_owned(),
        }],
        finish_reason: Some("stop".to_owned()),
        model: Some("deepseek-chat".to_owned()),
        usage: AiScriptUsage {
            input_tokens: Some(9),
            output_tokens: Some(2),
            cached_tokens: None,
            reasoning_tokens: Some(0),
        },
        ttft_ms: Some(120),
        total_ms: 233,
        tokens_per_second: None,
        event_count: 5,
    }
}

const SSE_BODY: &str =
    "data: {\"choices\":[{\"delta\":{\"content\":\"pong\"}}]}\n\ndata: [DONE]\n\n";

#[test]
fn pm_response_ai_exposes_read_only_run_without_leaking_provider_secrets() {
    let output = run_scripts(ai_input(
        r#"
        pm.test("shape", () => {
          const ai = pm.response.ai;
          const check = (ok, what) => { if (!ok) { throw new Error(what); } };
          check(ai.reply === "pong" && ai.reasoning === "thinking", "reply/reasoning");
          check(ai.toolCalls.length === 1 && ai.toolCalls[0].id === "call_1" && ai.toolCalls[0].name === "lookup", "toolCalls");
          check(ai.finishReason === "stop" && ai.model === "deepseek-chat", "finish/model");
          check(ai.usage.inputTokens === 9 && ai.usage.outputTokens === 2 && ai.usage.cachedTokens === null, "usage");
          check(ai.ttftMs === 120 && ai.totalMs === 233 && ai.eventCount === 5 && ai.tokensPerSecond === null, "timing");
        });
        try { pm.response.ai.reply = "hacked"; } catch (e) {}
        try { pm.response.ai.usage.inputTokens = 1; } catch (e) {}
        try { pm.response.ai.toolCalls.push({}); } catch (e) {}
        try { pm.response.ai = null; } catch (e) {}
        pm.variables.set("after", pm.response.ai.reply + pm.response.ai.usage.inputTokens + pm.response.ai.toolCalls.length);
        pm.variables.set("raw", pm.response.text());
        try { pm.response.json(); pm.variables.set("json", "parsed"); } catch (e) { pm.variables.set("json", "threw"); }
        pm.variables.set("dump", JSON.stringify([
          Object.keys(pm), Object.keys(pm.request), Object.keys(pm.response),
          Object.keys(pm.response.ai), pm.response.ai, pm.info
        ]));
        "#,
        SSE_BODY,
        Some(bible_run()),
    ));
    assert!(
        output.error.is_none() && output.tests[0].passed,
        "{:?}",
        output.tests[0].error
    );
    assert_eq!(
        output.local.get("after").map(String::as_str),
        Some("pong91")
    );
    assert_eq!(output.local.get("raw").map(String::as_str), Some(SSE_BODY));
    assert_eq!(output.local.get("json").map(String::as_str), Some("threw"));
    let dump = output.local.get("dump").unwrap().to_lowercase();
    for needle in [
        "provider", "apikey", "api_key", "baseurl", "base_url", "secret",
    ] {
        assert!(!dump.contains(needle), "{needle} leaked: {dump}");
    }

    let http_only = run_scripts(ai_input(
        r#"pm.variables.set("type", typeof pm.response.ai + ":" + ("ai" in pm.response));"#,
        r#"{"ok":true}"#,
        None,
    ));
    assert_eq!(
        http_only.local.get("type").map(String::as_str),
        Some("undefined:false")
    );
}
fn serve_once() -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0_u8; 2048];
        let _ = stream.read(&mut buffer);
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
    });
    (address, server)
}

#[test]
fn send_request_bridge_caps_ssrf_callback_throws_and_budget_hold() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0_u8; 2048];
        let read = stream.read(&mut buffer).unwrap();
        assert!(String::from_utf8_lossy(&buffer[..read]).contains("GET /script HTTP/1.1"));
        stream
            .write_all(b"HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"ok\":true}")
            .unwrap();
    });
    let script = format!(
        r#"pm.sendRequest("http://{address}/script", (error, response) => {{
          pm.variables.set("out", response.code + ":" + response.text() + ":" + response.headers.get("content-type"));
        }});"#
    );
    let output = run_scripts(input(vec![("Request", &script)]));
    server.join().unwrap();
    assert_eq!(
        output.local.get("out").map(String::as_str),
        Some("201:{\"ok\":true}:application/json")
    );

    let cap = crate::executor::MAX_RESPONSE_BODY_BYTES;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0_u8; 2048];
        let _ = stream.read(&mut buffer);
        let _ = stream.write_all(
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", cap + 100).as_bytes(),
        );
        let _ = stream.write_all(&vec![b'x'; cap + 100]);
    });
    let raw = script_http_request(
        &json!({ "url": format!("http://{address}/big") }),
        None,
        &ScriptLimits::default(),
        &mut Vec::new(),
    )
    .unwrap();
    server.join().unwrap();
    let res: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        (
            &res["truncated"],
            &res["size"],
            res["body"].as_str().map(str::len)
        ),
        (&json!(true), &json!(cap), Some(cap))
    );

    assert!(
        script_http_request(
            &json!({ "url": "http://169.254.169.254/latest/meta-data/" }),
            None,
            &ScriptLimits::default(),
            &mut Vec::new()
        )
        .is_err_and(|e| e.contains("refusing to connect"))
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0_u8; 2048];
        let _ = stream.read(&mut buffer);
        let _ = stream.write_all(
            b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/\r\nContent-Length: 0\r\n\r\n",
        );
    });
    assert!(
        script_http_request(
            &json!({ "url": format!("http://{address}/go") }),
            None,
            &ScriptLimits::default(),
            &mut Vec::new()
        )
        .is_err()
    );
    server.join().unwrap();

    let (address, server) = serve_once();
    assert!(
        script_http_request(
            &json!({ "url": format!("http://{address}/x") }),
            None,
            &ScriptLimits::default(),
            &mut Vec::new()
        )
        .is_ok()
    );
    server.join().unwrap();

    let (address, server) = serve_once();
    let throw_script = format!(
        r#"pm.sendRequest("http://{address}/x", () => {{
          pm.variables.set("calls", String(Number(pm.variables.get("calls") || 0) + 1));
          throw new Error("callback boom");
        }});"#
    );
    let throw_out = run_scripts(input(vec![("Request", &throw_script)]));
    server.join().unwrap();
    assert_eq!(throw_out.local.get("calls").map(String::as_str), Some("1"));
    assert!(
        throw_out
            .error
            .is_some_and(|e| e.message.contains("callback boom"))
    );

    let closed = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let budget_script = format!(
        r#"let limited = 0, leaked = 0;
        for (let i = 0; i < 60; i++) {{
          pm.sendRequest("http://user:hunter2@127.0.0.1:{closed}/p?token=abc123", (err) => {{
            const text = String(err && err.message);
            if (text.includes("limit reached")) limited++;
            if (text.includes("abc123") || text.includes("hunter2")) leaked++;
          }});
        }}
        pm.variables.set("res", limited + ":" + leaked);"#
    );
    let budget_out = run_scripts(input(vec![("A", &budget_script), ("B", &budget_script)]));
    assert_eq!(
        budget_out.local.get("res").map(String::as_str),
        Some("10:0")
    );
}

#[test]
fn output_caps_wall_clock_timeout_and_cancellation_paths_hold() {
    use std::io::{Read, Write};

    let capped = run_scripts(input(vec![(
        "Request",
        r#"for (let i = 0; i < 1500; i++) { console.log(i); pm.test("t" + i, () => {}); }"#,
    )]));
    assert_eq!((capped.console.len(), capped.tests.len()), (1_001, 1_001));
    assert!(capped.console.last().unwrap().message.contains("truncated"));
    assert!(
        capped
            .tests
            .last()
            .unwrap()
            .error
            .as_deref()
            .is_some_and(|e| e.contains("dropped"))
    );

    let long = run_scripts(input(vec![(
        "Request",
        r#"console.log("é".repeat(100000)); pm.test("long", () => { throw new Error("é".repeat(100000)); });"#,
    )]));
    assert!(
        long.console[0].message.ends_with(TRUNCATION_MARKER)
            && long.console[0].message.len() <= 64 * 1024 + TRUNCATION_MARKER.len()
    );
    let err_text = long.tests[0].error.as_deref().unwrap();
    assert!(
        err_text.ends_with(TRUNCATION_MARKER)
            && err_text.len() <= 64 * 1024 + TRUNCATION_MARKER.len()
    );

    let mut stuck = input(vec![(
        "Request",
        "pm.variables.set('a', '1'); while (true) {}",
    )]);
    stuck.limits.loop_iteration_limit = 30_000_000;
    stuck.limits.wall_clock_limit = Duration::from_millis(200);
    let stuck_out = run_scripts(stuck);
    assert!(
        stuck_out
            .error
            .is_some_and(|e| e.source == "Request" && e.message.starts_with(TIMEOUT_PREFIX))
    );
    assert!(stuck_out.local.is_empty());

    let cancel = CancelToken::new();
    let mut loop_cancel = input(vec![("Request", "while (true) {}")]);
    loop_cancel.limits.loop_iteration_limit = 30_000_000;
    loop_cancel.cancel = Some(cancel.clone());
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        cancel.cancel();
    });
    let loop_out = run_scripts(loop_cancel);
    canceller.join().unwrap();
    assert_eq!(
        loop_out.error.map(|e| e.message),
        Some(CANCELLED_MESSAGE.to_owned())
    );

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let pre_cancel = CancelToken::new();
    pre_cancel.cancel();
    assert_eq!(
        script_http_request(
            &json!({ "url": format!("http://{address}/never") }),
            Some(&pre_cancel),
            &ScriptLimits::default(),
            &mut Vec::new()
        ),
        Err(CANCELLED_MESSAGE.to_owned())
    );
    let mut pre_input = input(vec![(
        "Request",
        &format!(r#"pm.sendRequest("http://{address}/never");"#),
    )]);
    pre_input.cancel = Some(pre_cancel);
    assert!(
        run_scripts(pre_input)
            .error
            .is_some_and(|e| e.message.contains(CANCELLED_MESSAGE))
    );
    assert!(
        listener
            .accept()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::WouldBlock)
    );

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (release, hold) = std::sync::mpsc::channel::<()>();
    let (accepted, connected) = std::sync::mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        let _ = accepted.send(());
        let _ = hold.recv();
    });
    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    let canceller = std::thread::spawn(move || {
        let _ = connected.recv();
        trigger.cancel();
    });
    let script = format!(
        r#"try {{ pm.sendRequest("http://{address}/hang", () => pm.variables.set("cb", "1")); }} catch (e) {{ pm.variables.set("caught", "1"); }}
        pm.variables.set("after", "1");"#
    );
    let mut hang_input = input(vec![("Request", &script)]);
    hang_input.cancel = Some(cancel);
    let hang_out = run_scripts(hang_input);
    drop(release);
    canceller.join().unwrap();
    server.join().unwrap();
    assert!(
        hang_out
            .error
            .is_some_and(|e| e.message == CANCELLED_MESSAGE)
            && hang_out.local.is_empty()
    );

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (release, hold) = std::sync::mpsc::channel::<()>();
    let (stalled, partial_sent) = std::sync::mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0_u8; 2048];
        let _ = stream.read(&mut buffer);
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\npartial");
        let _ = stalled.send(());
        let _ = hold.recv();
    });
    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    let canceller = std::thread::spawn(move || {
        let _ = partial_sent.recv();
        trigger.cancel();
    });
    let stall_res = script_http_request(
        &json!({ "url": format!("http://{address}/stall") }),
        Some(&cancel),
        &ScriptLimits::default(),
        &mut Vec::new(),
    );
    drop(release);
    canceller.join().unwrap();
    server.join().unwrap();
    assert_eq!(stall_res, Err(CANCELLED_MESSAGE.to_owned()));
}

/// Every `pm.expect` matcher and both modifiers, once passing and once failing. A failure must
/// be the matcher's own "expected …" message, not a TypeError from a broken matcher.
#[test]
fn pm_expect_matchers_pass_and_fail_as_tabled() {
    let cases: &[(&str, bool)] = &[
        ("pm.expect(1).to.be.ok", true),
        ("pm.expect(0).to.be.ok", false),
        ("pm.expect(true).to.be.true", true),
        ("pm.expect(1).to.be.true", false),
        ("pm.expect(false).to.be.false", true),
        ("pm.expect(0).to.be.false", false),
        ("pm.expect(null).to.be.null", true),
        ("pm.expect(undefined).to.be.null", false),
        ("pm.expect(undefined).to.be.undefined", true),
        ("pm.expect(null).to.be.undefined", false),
        ("pm.expect('').to.be.empty", true),
        ("pm.expect({ a: 1 }).to.be.empty", false),
        ("pm.expect(1).to.equal(1)", true),
        ("pm.expect({ a: 1 }).to.equal({ a: 1 })", false),
        ("pm.expect('a').to.equals('a')", true),
        ("pm.expect('a').to.equals('b')", false),
        ("pm.expect(2).to.eq(2)", true),
        ("pm.expect(2).to.eq('2')", false),
        ("pm.expect({ a: [1] }).to.eql({ a: [1] })", true),
        ("pm.expect([1]).to.eql([2])", false),
        ("pm.expect([]).to.be.a('array')", true),
        ("pm.expect(null).to.be.a('object')", false),
        ("pm.expect({}).to.be.an('Object')", true),
        ("pm.expect('1').to.be.an('number')", false),
        ("pm.expect('team').to.include('ea')", true),
        ("pm.expect([{ a: 1 }]).to.include({ a: 1 })", false),
        ("pm.expect([1, 2]).to.contain(2)", true),
        ("pm.expect(5).to.contain(5)", false),
        ("pm.expect({ a: 1, b: 2 }).to.contains({ a: 1 })", true),
        ("pm.expect({ a: 1 }).to.contains({ a: 2 })", false),
        ("pm.expect({ a: { b: 1 } }).to.have.property('a')", true),
        ("pm.expect({ a: 1 }).to.have.property('a', 2)", false),
        ("pm.expect({ a: 1 }).to.have.ownProperty('a')", true),
        ("pm.expect({}).to.have.ownProperty('toString')", false),
        ("pm.expect([1, 2]).to.have.lengthOf(2)", true),
        ("pm.expect('abc').to.have.lengthOf(2)", false),
        ("pm.expect('abc').to.have.length(3)", true),
        ("pm.expect([]).to.have.length(1)", false),
        ("pm.expect(3).to.be.above(2)", true),
        ("pm.expect(2).to.be.above(2)", false),
        ("pm.expect(3).to.be.gt(2)", true),
        ("pm.expect(1).to.be.gt(2)", false),
        ("pm.expect(1).to.be.below(2)", true),
        ("pm.expect(2).to.be.below(2)", false),
        ("pm.expect(1).to.be.lt(2)", true),
        ("pm.expect(3).to.be.lt(2)", false),
        ("pm.expect(2).to.be.at.least(2)", true),
        ("pm.expect(1).to.be.at.least(2)", false),
        ("pm.expect(2).to.be.gte(2)", true),
        ("pm.expect(1).to.be.gte(2)", false),
        ("pm.expect(2).to.be.at.most(2)", true),
        ("pm.expect(3).to.be.at.most(2)", false),
        ("pm.expect(2).to.be.lte(2)", true),
        ("pm.expect(3).to.be.lte(2)", false),
        ("pm.expect(5).to.be.within(1, 5)", true),
        ("pm.expect(6).to.be.within(1, 5)", false),
        ("pm.expect(1.05).to.be.closeTo(1, 0.1)", true),
        ("pm.expect(1.2).to.be.closeTo(1, 0.1)", false),
        ("pm.expect('inv_42').to.match(/^inv_\\d+$/)", true),
        ("pm.expect('cus_1').to.match(/^inv_/)", false),
        ("pm.expect('hello world').to.have.string('lo w')", true),
        ("pm.expect(42).to.have.string('4')", false),
        ("pm.expect(2).to.be.oneOf([1, 2])", true),
        ("pm.expect({ a: 1 }).to.be.oneOf([{ a: 1 }])", false),
        ("pm.expect(4).to.satisfy((n) => n % 2 === 0)", true),
        ("pm.expect(3).to.satisfy((n) => n % 2 === 0)", false),
        ("pm.expect([]).to.be.instanceof(Array)", true),
        ("pm.expect({}).to.be.instanceof(Array)", false),
        ("pm.expect(1).to.not.equal(2)", true),
        ("pm.expect(1).not.to.equal(1)", false),
        ("pm.expect({ a: 1 }).to.deep.equal({ a: 1 })", true),
        ("pm.expect([{ a: 1 }]).to.deep.include({ a: 2 })", false),
        (
            "pm.expect({ a: [1] }).to.have.deep.property('a', [1])",
            true,
        ),
        ("pm.expect({ a: 1 }).to.not.deep.equal({ a: 1 })", false),
        (
            "pm.expect(class { static make() {} }).itself.to.respondTo('make')",
            true,
        ),
        (
            "pm.expect(class { static make() {} }).to.respondTo('make')",
            false,
        ),
    ];
    let script: String = cases
        .iter()
        .map(|(expression, _)| format!("pm.test({expression:?}, () => {{ {expression}; }});\n"))
        .collect();
    let output = run_scripts(input(vec![("Matchers", &script)]));
    assert!(output.error.is_none(), "{:?}", output.error);
    assert_eq!(output.tests.len(), cases.len());
    let wrong: Vec<_> = cases
        .iter()
        .zip(&output.tests)
        .filter(|((_, passes), test)| {
            test.passed != *passes
                || (!test.passed
                    && !test
                        .error
                        .as_deref()
                        .is_some_and(|e| e.starts_with("expected")))
        })
        .map(|((expression, _), test)| (*expression, test.error.clone()))
        .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The byte-string helpers and the CryptoJS subset give the standard answers.
#[test]
fn btoa_atob_and_cryptojs_digests_match_known_answers() {
    let output = run_scripts(input(vec![(
        "Request",
        r#"
        pm.variables.set("b64", btoa("hello"));
        pm.variables.set("plain", atob(" aGVs bG8= "));
        pm.variables.set("md5", CryptoJS.MD5("").toString());
        pm.variables.set("sha", CryptoJS.SHA256("abc").toString());
        const mac = CryptoJS.HmacSHA256("The quick brown fox jumps over the lazy dog", "key");
        pm.variables.set("hmac", mac.toString());
        pm.variables.set("hmac64", CryptoJS.enc.Base64.stringify(mac));
        pm.variables.set("utf8", CryptoJS.enc.Utf8.parse("é").toString());
        try { btoa("汉"); } catch (error) { pm.variables.set("latin1", "refused"); }
        pm.variables.set("sha1", CryptoJS.SHA1("abc").toString());
        pm.variables.set("sha512", CryptoJS.SHA512("abc").toString().slice(0, 16));
        const fox = "The quick brown fox jumps over the lazy dog";
        pm.variables.set("hmac1", CryptoJS.enc.Base64.stringify(CryptoJS.HmacSHA1(fox, "key")));
        pm.variables.set("hmac512", CryptoJS.HmacSHA512(fox, "key").toString().slice(0, 16));
        postman.setEnvironmentVariable("legacy", "set");
        pm.variables.set("legacy", postman.getEnvironmentVariable("legacy"));
        "#,
    )]));
    assert!(output.error.is_none(), "{:?}", output.error);
    let got = |key: &str| output.local.get(key).map(String::as_str);
    assert_eq!(got("b64"), Some("aGVsbG8="));
    assert_eq!(got("plain"), Some("hello"));
    assert_eq!(got("md5"), Some("d41d8cd98f00b204e9800998ecf8427e"));
    assert_eq!(
        got("sha"),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
    assert_eq!(
        got("hmac"),
        Some("f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8")
    );
    assert_eq!(
        got("hmac64"),
        Some("97yD9DBThCSxMpjmqm+xQ+9NWaFJRhdZl0edvC0aPNg=")
    );
    assert_eq!(got("utf8"), Some("c3a9"));
    assert_eq!(got("latin1"), Some("refused"));
    assert_eq!(
        got("sha1"),
        Some("a9993e364706816aba3e25717850c26c9cd0d89d")
    );
    assert_eq!(got("sha512"), Some("ddaf35a193617aba"));
    assert_eq!(got("hmac1"), Some("3nybhbi3iqa8ino29wqQcBydtNk="));
    assert_eq!(got("hmac512"), Some("b42af09057bac1e2"));
    // The legacy call writes the environment scope `pm.environment` reads.
    assert_eq!(got("legacy"), Some("set"));
    assert!(output.mutations.environment.keys.contains("legacy"));
}

/// Security: `pm.sendRequest` passes the same production guard as the main send. An
/// unconfirmed guarded method is refused before anything is sent and reported to the caller;
/// the caller's confirmation lets it through. Both outcomes are console lines.
#[test]
fn send_request_needs_the_production_confirmation_and_is_logged() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0_u8; 2048];
        let _ = stream.read(&mut buffer);
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
    });
    let script = format!(
        r#"pm.sendRequest({{ url: "http://{address}/charge", method: "POST" }}, (error) => {{
          pm.variables.set("error", error ? error.message : "none");
        }});"#
    );
    let guarded = |confirmed: bool| {
        let mut script_input = input(vec![("Request", &script)]);
        if let Some(environment) = script_input.environment.as_mut() {
            environment.confirm_before_send = true;
        }
        script_input.confirmed = confirmed;
        run_scripts(script_input)
    };

    let refused = guarded(false);
    let message = "Local is guarded: confirm before sending a POST request";
    assert_eq!(refused.confirmation_required.as_deref(), Some(message));
    assert_eq!(
        refused.local.get("error").map(String::as_str),
        Some(message)
    );
    assert!(
        refused.console[0].message.contains("POST http://")
            && refused.console[0].message.contains("refused")
    );

    let sent = guarded(true);
    server.join().unwrap();
    assert!(sent.confirmation_required.is_none());
    assert_eq!(sent.local.get("error").map(String::as_str), Some("none"));
    assert!(
        sent.console[0].message.contains("/charge → 200 OK ("),
        "{:?}",
        sent.console
    );
}

/// The import-time scan lists the APIs the sandbox lacks, once each, and leaves the
/// provided ones (and members of other objects) alone.
#[test]
fn compat_scan_lists_missing_apis_once_in_order() {
    let source = r#"
const moment = require("moment");
const sig = CryptoJS.HmacSHA256(body, key).toString(CryptoJS.enc.Base64);
const aes = CryptoJS.AES.encrypt(body, key); // CryptoJS.RC4 in a comment is ignored
setTimeout(() => {}, 10);
config.require("x"); pm.visualizer.set("<b/>"); require("lodash");
postman.setEnvironmentVariable("a", CryptoJS.HmacSHA1(body, key)); postman.getResponseHeader("x");
"#;
    let issues = script_compat_issues(source);
    let found: Vec<(&str, u32)> = issues
        .iter()
        .map(|issue| (issue.api.as_str(), issue.line))
        .collect();
    assert_eq!(
        found,
        vec![
            ("require", 2),
            ("CryptoJS.AES", 4),
            ("setTimeout", 5),
            ("pm.visualizer", 6),
            ("postman.getResponseHeader", 7)
        ]
    );
    assert!(script_compat_issues("pm.test('ok', () => btoa('x'));").is_empty());
}

/// `pm.response.to.have.jsonSchema` and `tv4.validate` use the assertion's validator: a match
/// passes, a violation fails with its location, a non-JSON body fails instead of passing.
#[test]
fn json_schema_matchers_validate_the_response_body() {
    let source = r#"
const schema = { type: "object", required: ["name"], properties: { name: { type: "string" } } };
pm.test("matches", () => pm.response.to.have.jsonSchema(schema));
pm.test("violates", () => pm.response.to.have.jsonSchema({ type: "object", required: ["id"] }));
pm.test("tv4", () => {
  pm.expect(tv4.validate(pm.response.json(), schema)).to.equal(true);
  pm.expect(tv4.validate({ name: 1 }, schema)).to.equal(false);
  pm.expect(tv4.error.message).to.include("$.name");
});
"#;
    let out = run_scripts(ai_input(source, r#"{"name":"Ada"}"#, None));
    let passed: Vec<bool> = out.tests.iter().map(|test| test.passed).collect();
    assert_eq!(passed, [true, false, true], "{:?}", out.tests);
    assert!(
        out.tests[1]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("\"id\" is a required property"))
    );

    let not_json = run_scripts(ai_input(
        r#"pm.test("schema", () => pm.response.to.have.jsonSchema({ type: "object" }));"#,
        "<html>",
        None,
    ));
    assert!(!not_json.tests[0].passed);
}

/// An unknown assertion word fails the test instead of reading `undefined` and passing; the
/// response matchers Postman scripts use exist, negate, and a returned assertion is not a
/// thenable.
#[test]
fn unknown_assertion_words_fail_and_response_matchers_hold() {
    let source = r#"
pm.test("typo", () => { pm.expect(1).to.be.sucess; });
pm.test("response typo", () => { pm.response.to.be.sucess; });
pm.test("success", () => { pm.response.to.be.success; });
pm.test("negated", () => { pm.response.to.not.be.success; });
pm.test("returned", () => pm.expect(1).to.equal(1));
pm.test("body", () => { pm.response.to.have.body({ name: "Ada" }); pm.response.to.be.json; });
pm.test("status text", () => pm.response.to.have.status("OK"));
"#;
    let out = run_scripts(ai_input(source, r#"{"name":"Ada"}"#, None));
    let got: Vec<(&str, bool)> = out
        .tests
        .iter()
        .map(|test| (test.name.as_str(), test.passed))
        .collect();
    assert_eq!(
        got,
        [
            ("typo", false),
            ("response typo", false),
            ("success", true),
            ("negated", false),
            ("returned", true),
            ("body", true),
            ("status text", true),
        ],
        "{:?}",
        out.tests
    );
    assert!(
        out.tests[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("Invalid assertion property: sucess"))
    );
}

/// Deep equality follows chai's deep-eql: key order does not matter, a missing key differs from
/// an `undefined` one, NaN equals NaN; the chains that read a flag (`length.above`,
/// `include.members`, `any.keys`) and `throw` work.
#[test]
fn deep_equality_and_flag_chains_follow_chai() {
    let cases: &[(&str, bool)] = &[
        ("pm.expect({ b: 1, a: 2 }).to.eql({ a: 2, b: 1 })", true),
        ("pm.expect({ a: undefined }).to.eql({})", false),
        ("pm.expect(NaN).to.eql(NaN)", true),
        ("pm.expect([NaN]).to.deep.equal([null])", false),
        ("pm.expect(new Date(0)).to.eql(new Date(1))", false),
        ("pm.expect(1).to.exist", true),
        ("pm.expect(null).to.exist", false),
        ("pm.expect([1, 2, 3]).to.have.length.above(2)", true),
        ("pm.expect([1]).to.have.length.above(2)", false),
        ("pm.expect([1, 2]).to.include.members([2])", true),
        ("pm.expect([1, 2]).to.have.members([2])", false),
        ("pm.expect({ a: 1, b: 2 }).to.have.all.keys('a', 'b')", true),
        ("pm.expect({ a: 1 }).to.have.any.keys('b', 'c')", false),
        (
            "pm.expect(() => { throw new Error('boom'); }).to.throw('boom')",
            true,
        ),
        ("pm.expect(() => {}).to.throw()", false),
        ("pm.expect([]).to.be.an.instanceof(Array)", true),
        (
            "pm.expect({ a: { b: 2 } }).to.have.nested.property('a.b', 2)",
            true,
        ),
        (
            "pm.expect({ a: 1 }).to.have.property('a').that.equals(2)",
            false,
        ),
    ];
    let script: String = cases
        .iter()
        .map(|(expression, _)| format!("pm.test({expression:?}, () => {{ {expression}; }});\n"))
        .collect();
    let output = run_scripts(input(vec![("Chains", &script)]));
    assert!(output.error.is_none(), "{:?}", output.error);
    let wrong: Vec<_> = cases
        .iter()
        .zip(&output.tests)
        .filter(|((_, passes), test)| {
            test.passed != *passes
                || (!test.passed
                    && !test
                        .error
                        .as_deref()
                        .is_some_and(|e| e.starts_with("expected")))
        })
        .map(|((expression, _), test)| (*expression, test.error.clone()))
        .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// `pm.sendRequest` encodes each Postman body mode as Postman sends it and refuses a file body
/// before sending; the response's `Set-Cookie` cookies are readable. `pm.request.body.update`
/// takes a `{ mode, … }` object instead of clearing the body.
#[test]
fn send_request_encodes_body_modes_and_body_update_takes_mode_objects() {
    use crate::test_server::{TestServer, TestServerAction};
    let ok = |extra: &str| {
        TestServerAction::Respond(format!(
            "HTTP/1.1 200 OK\r\n{extra}Content-Length: 2\r\nConnection: close\r\n\r\nok"
        ))
    };
    let server = TestServer::spawn_sequence(vec![
        ok("Set-Cookie: sid=abc; Path=/\r\n"),
        ok(""),
        ok(""),
        ok(""),
    ]);
    let base = server.base_url.clone();
    let script = format!(
        r#"
pm.sendRequest({{ url: "{base}/form", method: "POST", body: {{ mode: "urlencoded", urlencoded: [
  {{ key: "a b", value: "1&2" }}, {{ key: "off", value: "x", disabled: true }} ] }} }},
  (error, response) => pm.variables.set("sid", response.cookies.get("sid")));
pm.sendRequest({{ url: "{base}/multi", method: "POST", body: {{ mode: "formdata", formdata: [{{ key: "field", value: "v1" }}] }} }});
pm.sendRequest({{ url: "{base}/gql", method: "POST", body: {{ mode: "graphql", graphql: {{ query: "{{ me }}", variables: "{{\"id\":1}}" }} }} }});
pm.sendRequest({{ url: "{base}/raw", method: "POST", body: {{ mode: "raw", raw: "{{}}", options: {{ raw: {{ language: "json" }} }} }} }});
pm.sendRequest({{ url: "{base}/file", method: "POST", body: {{ mode: "file", file: {{ src: "/etc/passwd" }} }} }},
  (error) => pm.variables.set("file", error ? error.message : "sent"));
pm.request.body.update({{ mode: "raw", raw: "{{\"k\":1}}", options: {{ raw: {{ language: "json" }} }} }});
"#
    );
    let output = run_scripts(input(vec![("Request", &script)]));
    assert!(output.error.is_none(), "{:?}", output.error);
    let sent: Vec<String> = server
        .received_request_bytes_many(4)
        .into_iter()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .collect();
    assert!(
        sent[0].contains("content-type: application/x-www-form-urlencoded")
            && sent[0].ends_with("\r\n\r\na+b=1%262"),
        "{}",
        sent[0]
    );
    assert!(
        sent[1].contains("multipart/form-data; boundary=")
            && sent[1].contains("name=\"field\"\r\n\r\nv1\r\n"),
        "{}",
        sent[1]
    );
    assert!(
        sent[2].contains("content-type: application/json")
            && sent[2].ends_with(r#"{"query":"{ me }","variables":{"id":1}}"#),
        "{}",
        sent[2]
    );
    assert!(
        sent[3].contains("content-type: application/json") && sent[3].ends_with("\r\n\r\n{}"),
        "{}",
        sent[3]
    );
    assert_eq!(output.local.get("sid").map(String::as_str), Some("abc"));
    assert!(
        output
            .local
            .get("file")
            .is_some_and(|message| message.contains("files cannot be attached"))
    );
    assert_eq!(
        output.request.body,
        pingdeck_model::models::RequestBody::Json {
            content: r#"{"k":1}"#.to_owned()
        }
    );
}

/// Security: a variable a script creates under a credential name or with a token-shaped value is
/// a secret, so exports and folder sync strip it; an empty imported placeholder becomes secret
/// when a credential lands in it; a disabled variable keeps its switch and says so.
#[test]
fn script_written_credentials_are_stored_as_secrets() {
    const JWT: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
    let mut script_input = input(vec![(
        "Request",
        &format!(
            r#"
pm.environment.set("token", "abc");
pm.environment.set("note", "{JWT}");
pm.environment.set("page", "2");
pm.environment.set("accessToken", "xyz");
pm.environment.set("legacy", "new");
pm.environment.set("apiToken", "{JWT}");
pm.environment.set("jwt", "{JWT}");
pm.globals.set("client_secret", "s");
"#
        ),
    )]);
    if let Some(environment) = script_input.environment.as_mut() {
        environment
            .variables
            .push(EnvironmentVariable::plain("accessToken", "").unwrap());
        // A placeholder that receives a token becomes secret; a plain variable the user keeps
        // a token in stays plain.
        environment
            .variables
            .push(EnvironmentVariable::plain("apiToken", "<your token>").unwrap());
        environment
            .variables
            .push(EnvironmentVariable::plain("jwt", format!("{JWT}x")).unwrap());
        let mut legacy = EnvironmentVariable::plain("legacy", "old").unwrap();
        legacy.enabled = false;
        environment.variables.push(legacy);
    }
    let output = run_scripts(script_input);
    assert!(output.error.is_none(), "{:?}", output.error);
    let variables = &output.environment.as_ref().unwrap().variables;
    let find = |key: &str| variables.iter().find(|v| v.key == key).unwrap();
    let secret: Vec<(&str, bool)> = [
        "token",
        "note",
        "page",
        "accessToken",
        "baseUrl",
        "apiToken",
        "jwt",
    ]
    .into_iter()
    .map(|key| (key, find(key).is_secret))
    .collect();
    assert_eq!(
        secret,
        [
            ("token", true),
            ("note", true),
            ("page", false),
            ("accessToken", true),
            ("baseUrl", false),
            ("apiToken", true),
            ("jwt", false)
        ]
    );
    let legacy = find("legacy");
    assert_eq!(
        (legacy.enabled, legacy.value.as_deref()),
        (false, Some("new"))
    );
    assert!(
        output
            .console
            .iter()
            .any(|entry| entry.message.contains("legacy is disabled"))
    );
    assert!(
        output
            .globals
            .iter()
            .any(|v| v.key == "client_secret" && v.is_secret)
    );
}

/// `pm.cookies` reads the jar cookies of the request URL before the response exists, and
/// `pm.cookies.jar()` reads and writes the jar for the request's host only; writes reach the
/// output for the main request and for persisting.
#[test]
fn pm_cookies_read_the_jar_and_the_jar_stays_on_the_request_host() {
    let cookie = |name: &str, value: &str, domain: &str| {
        pingdeck_model::CookieJarEntry::from_resolved_cookie(
            "ws_1", None, name, value, domain, "/", true, None, None, false, false, None, "1",
        )
        .unwrap()
    };
    let mut script_input = input(vec![(
        "Request",
        r#"
pm.request.headers.upsert({ key: "X-XSRF-TOKEN", value: pm.cookies.get("XSRF-TOKEN") });
pm.variables.set("other", String(pm.cookies.has("other")));
const jar = pm.cookies.jar();
jar.set("https://api.test", "session", "s1");
jar.get("api.test", "session", (error, value) => pm.variables.set("got", value));
jar.unset("https://api.test/", "XSRF-TOKEN");
try { jar.get("https://other.test", "other"); } catch (error) { pm.variables.set("refused", error.message); }
jar.getAll("https://other.test", (error) => pm.variables.set("callback", String(!!error)));
"#,
    )]);
    script_input.cookie_jar = vec![
        cookie("XSRF-TOKEN", "x1", "api.test"),
        cookie("other", "o1", "other.test"),
    ];
    let output = run_scripts(script_input);
    assert!(output.error.is_none(), "{:?}", output.error);
    assert!(
        output
            .request
            .headers
            .iter()
            .any(|h| h.key == "X-XSRF-TOKEN" && h.value == "x1")
    );
    let got = |key: &str| output.local.get(key).map(String::as_str);
    assert_eq!(
        (got("other"), got("got"), got("callback")),
        (Some("false"), Some("s1"), Some("true"))
    );
    assert!(got("refused").is_some_and(|message| message.contains("reaches only")));
    let names = |cookies: &[pingdeck_model::CookieJarEntry]| {
        cookies
            .iter()
            .map(|cookie| cookie.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&output.cookie_writes), ["session"]);
    assert_eq!(names(&output.cookie_jar), ["other", "session"]);
    assert_eq!(
        output
            .cookie_deletes
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>(),
        ["cookie:ws_1:api.test:/:XSRF-TOKEN"]
    );
}

/// The jar host is fixed before the script runs: rewriting the variable the request URL is built
/// from does not let `pm.cookies` or `pm.cookies.jar()` reach another site's cookies.
#[test]
fn rewriting_the_base_url_does_not_move_the_cookie_jar_host() {
    let cookie = |name: &str, domain: &str| {
        pingdeck_model::CookieJarEntry::from_resolved_cookie(
            "ws_1", None, name, "v", domain, "/", true, None, None, false, false, None, "1",
        )
        .unwrap()
    };
    let mut script_input = input(vec![(
        "Request",
        r#"
pm.variables.set("baseUrl", "https://bank.test");
pm.variables.set("seen", String(pm.cookies.has("bank_session")));
try { pm.cookies.jar().getAll("https://bank.test"); } catch (error) { pm.variables.set("read", error.message); }
try { pm.cookies.jar().set("https://bank.test", "planted", "x"); } catch (error) { pm.variables.set("write", error.message); }
"#,
    )]);
    script_input.cookie_jar = vec![cookie("bank_session", "bank.test")];
    let output = run_scripts(script_input);
    assert!(output.error.is_none(), "{:?}", output.error);
    let got = |key: &str| output.local.get(key).map(String::as_str);
    assert_eq!(got("seen"), Some("false"));
    for key in ["read", "write"] {
        assert!(
            got(key)
                .is_some_and(|message| message.contains("reaches only the cookies of api.test")),
            "{key}: {:?}",
            got(key)
        );
    }
    assert!(output.cookie_writes.is_empty());
}

/// The import scan reports assertion words and file bodies a script cannot run, and leaves the
/// supported ones alone.
#[test]
fn compat_scan_reports_unknown_assertion_words_and_file_bodies() {
    let source = r#"
pm.expect(code).to.be.sucess;
pm.response.to.be.teapot;
pm.expect(list).to.include.members([1]).and.have.lengthOf(2);
pm.sendRequest({ url, body: { mode: "file", file: {} } });
items.map(f).filter(g);
pm.expect(code).to.be.success;
pm.response.to.have.status(200).and.have.header("X-Id");
pm.response.to.have.property("id");
const img = { src: pm.response.json().url };
"#;
    let found: Vec<(String, u32)> = script_compat_issues(source)
        .into_iter()
        .map(|issue| (issue.api, issue.line))
        .collect();
    assert_eq!(
        found,
        [
            ("pm.expect(…).sucess".to_owned(), 2),
            ("pm.response.to.teapot".to_owned(), 3),
            ("pm.sendRequest file body".to_owned(), 5),
            ("pm.expect(…).success".to_owned(), 7),
            ("pm.response.to.property".to_owned(), 9),
        ]
    );
    // A `src` key is a file only inside a form-data body.
    let form = r#"pm.sendRequest({ url, body: { mode: "formdata", formdata: [{ key: "f", "src": "/tmp/a" }] } });"#;
    assert_eq!(
        script_compat_issues(form)
            .into_iter()
            .map(|issue| issue.api)
            .collect::<Vec<_>>(),
        ["pm.sendRequest file body"]
    );
}
