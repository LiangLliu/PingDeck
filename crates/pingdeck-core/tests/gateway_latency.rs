//! What the gateway adds to a call, split by what can be proven without a clock.
//!
//! The default run (`cargo test`, debug or release, any machine load) asserts only properties
//! that do not depend on how fast the machine is:
//! - **structure:** the gateway adds no upstream request, forwards a same-protocol body unchanged
//!   and answers with the upstream's bytes;
//! - **no buffering:** a streamed chunk reaches the client while the upstream is still holding the
//!   rest of the answer (the upstream only finishes after the client confirmed the first chunk);
//! - **logging never delays the response:** with the log writer stalled (the database write lock
//!   is held), requests are still answered and their rows are stored once the writer can write;
//! - **relative overhead, release builds only:** direct and gateway calls are *paired and
//!   interleaved* in one run, so drift and load hit both alike. The PRD limit is applied to the
//!   lower quartile of the per-pair differences, an estimator that a busy machine cannot push up
//!   (it would take three quarters of the pairs being slow), over up to three rounds. When the
//!   direct baseline itself is too noisy the case is reported as *inconclusive* and passes: noise
//!   is never a failure.
//!
//! The absolute p50/p95 table is the perf harness, an `#[ignore]` test:
//! `cargo test -p pingdeck-core --release --test gateway_latency -- --ignored --nocapture`
//! (see `docs/development.md` §3.3).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use common::{
    ALIAS, FINAL_TEXT, Reply, TOKEN, TempDir, UPSTREAM_MODEL, Upstream, configure_gateway,
    configure_llm, gateway_events, http, openai_chat, wait_for_requests,
};
use pingdeck_core::app::{
    AppState, llm_gateway_start_impl, llm_gateway_status_impl, llm_log_list_impl,
    settings_update_impl,
};
use pingdeck_core::model::settings::SettingsSection;
use serde_json::{Value, json};

const CONCURRENCY: usize = 4;
/// Frames of the long stream.
const CHUNKS: usize = 200;
/// The user text that makes the fake upstream answer with [`many_chunks`].
const CHUNKS_MARKER: &str = "many chunks";

// ------------------------------------------------------------------------------- rig

/// A chat stream of [`CHUNKS`] one-word deltas, then the usage and `[DONE]`.
fn many_chunks() -> Vec<String> {
    let delta = |text: &str| {
        format!(
            "data: {}\n\n",
            json!({"id": "c", "object": "chat.completion.chunk", "model": "m",
                "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}]})
        )
    };
    let mut frames: Vec<String> = (0..CHUNKS).map(|_| delta("word ")).collect();
    frames.push(finish_frame());
    frames.push("data: [DONE]\n\n".to_owned());
    frames
}

fn finish_frame() -> String {
    format!(
        "data: {}\n\n",
        json!({"id": "c", "object": "chat.completion.chunk", "model": "m",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": CHUNKS, "total_tokens": CHUNKS + 5}})
    )
}

fn upstream_handler(number: usize, request: &common::Recorded) -> Reply {
    if request.body["messages"][0]["content"] == CHUNKS_MARKER {
        Reply::Sse(many_chunks())
    } else {
        openai_chat(number, request)
    }
}

/// A started gateway whose alias [`ALIAS`] points at the fake upstream on `upstream_port`.
struct Rig {
    _root: TempDir,
    state: AppState,
    port: u16,
}

fn rig(upstream_port: u16) -> Rig {
    let root = TempDir::new("gateway-latency");
    let state = AppState::open(root.join("pingdeck.sqlite3")).unwrap();
    configure_llm(&state, upstream_port);
    let mut port = configure_gateway(&state);
    // `configure_gateway` picks a free port and releases it; another test of this process may
    // take it before the gateway binds, so a refused bind picks the next one.
    let mut attempts = 0;
    while let Err(error) = llm_gateway_start_impl(&state) {
        attempts += 1;
        assert!(attempts < 10, "the gateway never started: {error:?}");
        port = common::free_port(state.settings().mock.first_port);
        settings_update_impl(
            &state,
            SettingsSection::Ai,
            &json!({"gateway": {"port": port}}),
        )
        .unwrap();
    }
    Rig {
        _root: root,
        state,
        port,
    }
}

fn bearer() -> String {
    format!("Bearer {TOKEN}")
}

/// A request body of about 1 KiB (padding in the user text).
fn chat_body(model: &str, stream: bool) -> Value {
    json!({"model": model, "stream": stream, "messages": [{"role": "user", "content": "x".repeat(900)}]})
}

fn messages_body(model: &str) -> Value {
    json!({"model": model, "max_tokens": 16, "messages": [{"role": "user", "content": "x".repeat(900)}]})
}

fn chunks_request(model: &str) -> Value {
    json!({"model": model, "stream": true, "messages": [{"role": "user", "content": CHUNKS_MARKER}]})
}

/// Sends one request with a 200 answer expected and returns how long it took.
fn timed_post(port: u16, path: &str, headers: &[(&str, &str)], body: &Value) -> Duration {
    let started = Instant::now();
    let response = http(port, "POST", path, headers, Some(body));
    assert_eq!(response.status, 200, "{}", response.text());
    started.elapsed()
}

fn set_capture(state: &AppState, mode: &str) {
    settings_update_impl(
        state,
        SettingsSection::Ai,
        &json!({"gateway": {"capture_mode": mode}}),
    )
    .unwrap();
}

// ------------------------------------------------------------ deterministic properties

#[test]
fn the_gateway_adds_no_upstream_request_and_keeps_the_bytes() {
    const CALLS: usize = 25;
    let upstream = Upstream::start(upstream_handler);
    let rig = rig(upstream.port);
    let auth = bearer();
    let gateway_headers = [("authorization", auth.as_str())];
    let anthropic = [("x-api-key", TOKEN), ("anthropic-version", "2023-06-01")];

    // same protocol: one upstream request per call, the body as sent except `model`, the
    // upstream's answer byte for byte (the same bytes a direct call to the model gets)
    let direct = http(
        upstream.port,
        "POST",
        "/v1/chat/completions",
        &[("authorization", "Bearer sk-direct")],
        Some(&chat_body(UPSTREAM_MODEL, false)),
    );
    for _ in 0..CALLS {
        let response = http(
            rig.port,
            "POST",
            "/v1/chat/completions",
            &gateway_headers,
            Some(&chat_body(ALIAS, false)),
        );
        assert_eq!(response.status, 200);
        assert_eq!(response.body, direct.body, "the upstream's bytes");
    }
    let seen = upstream.requests();
    assert_eq!(seen.len(), 1 + CALLS, "no extra upstream request");
    for request in &seen[1..] {
        assert_eq!(request.body, chat_body(UPSTREAM_MODEL, false));
    }
    assert_eq!(
        direct.json()["choices"][0]["message"]["content"],
        FINAL_TEXT
    );

    // translated: still exactly one upstream request per call
    for _ in 0..CALLS {
        let response = http(
            rig.port,
            "POST",
            "/v1/messages",
            &anthropic,
            Some(&messages_body(ALIAS)),
        );
        assert_eq!(response.status, 200, "{}", response.text());
    }
    assert_eq!(upstream.requests().len(), 1 + 2 * CALLS);

    // streaming: one upstream request, every chunk relayed
    for _ in 0..3 {
        let response = http(
            rig.port,
            "POST",
            "/v1/chat/completions",
            &gateway_headers,
            Some(&chunks_request(ALIAS)),
        );
        assert_eq!(response.status, 200);
        assert_eq!(response.text().matches("word ").count(), CHUNKS);
        assert!(response.text().contains("[DONE]"));
    }
    assert_eq!(upstream.requests().len(), 1 + 2 * CALLS + 3);
}

/// An upstream that sends the first chunk of a stream, then holds the rest until `release` is
/// signalled (or 20 s pass). The thread returns whether it was released in time.
fn gated_upstream() -> (u16, mpsc::Sender<()>, JoinHandle<bool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (release, released) = mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut seen = Vec::new();
        let mut chunk = [0_u8; 4096];
        let (head_end, length) = loop {
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0, "the request ended early");
            seen.extend_from_slice(&chunk[..read]);
            if let Some(end) = seen.windows(4).position(|window| window == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&seen[..end]).to_ascii_lowercase();
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                break (end + 4, length);
            }
        };
        while seen.len() < head_end + length {
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0, "the request body ended early");
            seen.extend_from_slice(&chunk[..read]);
        }
        let first = format!(
            "data: {}\n\n",
            json!({"id": "c", "object": "chat.completion.chunk", "model": "m",
                "choices": [{"index": 0, "delta": {"content": "first"}, "finish_reason": null}]})
        );
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                     cache-control: no-cache\r\nconnection: close\r\n\r\n{first}"
                )
                .as_bytes(),
            )
            .unwrap();
        stream.flush().unwrap();
        let released_in_time = released.recv_timeout(Duration::from_secs(20)).is_ok();
        let rest = format!("{}data: [DONE]\n\n", finish_frame());
        let _ = stream.write_all(rest.as_bytes());
        released_in_time
    });
    (port, release, handle)
}

#[test]
fn a_streamed_chunk_reaches_the_client_before_the_upstream_has_finished() {
    let (upstream_port, release, upstream) = gated_upstream();
    let rig = rig(upstream_port);
    let payload = chunks_request(ALIAS).to_string();
    let head = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nhost: 127.0.0.1:{}\r\nauthorization: {}\r\n\
         content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        rig.port,
        bearer(),
        payload.len()
    );
    let mut client = TcpStream::connect(("127.0.0.1", rig.port)).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    client.write_all(head.as_bytes()).unwrap();
    client.write_all(payload.as_bytes()).unwrap();
    let mut seen = Vec::new();
    let mut chunk = [0_u8; 4096];
    // The first chunk must arrive while the upstream still waits for us: a gateway that buffers
    // the answer would only deliver it after the upstream gave up waiting.
    while !String::from_utf8_lossy(&seen).contains("first") {
        let read = client.read(&mut chunk).unwrap();
        assert!(read > 0, "the stream ended before the first chunk");
        seen.extend_from_slice(&chunk[..read]);
    }
    release.send(()).unwrap();
    let _ = client.read_to_end(&mut seen);
    assert!(
        upstream.join().unwrap(),
        "the first chunk only reached the client after the upstream finished: the stream is buffered"
    );
    assert!(String::from_utf8_lossy(&seen).contains("[DONE]"));
}

#[test]
fn a_stalled_log_writer_never_delays_a_request() {
    const CALLS: usize = 30;
    let upstream = Upstream::start(upstream_handler);
    let rig = rig(upstream.port);
    let events = gateway_events(&rig.state);
    let auth = bearer();
    let headers = [("authorization", auth.as_str())];
    let body = chat_body(ALIAS, false);
    // The first request of a client also writes its `last_used`; do that before the stall.
    timed_post(rig.port, "/v1/chat/completions", &headers, &body);
    wait_for_requests(&events, 1);

    // Stall the writer: hold the database write lock, so its insert can only wait.
    let db = rig.state.data_dirs().unwrap().db.clone();
    let locker = pingdeck_core::llm::runtime::DbHandle::path(db)
        .open()
        .unwrap();
    locker.busy_timeout(Duration::from_secs(30)).unwrap();
    locker.execute_batch("BEGIN IMMEDIATE").unwrap();
    for _ in 0..CALLS {
        // The writer's own wait for the lock is 5 s: an answer that depended on the writer would
        // come after that, never within 4 s.
        let took = timed_post(rig.port, "/v1/chat/completions", &headers, &body);
        assert!(
            took < Duration::from_secs(4),
            "a request waited for the log: {took:?}"
        );
    }
    let stored = llm_log_list_impl(&rig.state, None, Some(500), None)
        .unwrap()
        .len();
    assert_eq!(
        stored, 1,
        "the writer was stalled: only the warm-up row is stored"
    );

    // Released, the writer stores what was queued (a row can only be lost, and counted, when the
    // stall outlasted the writer's own 5 s wait).
    locker.execute_batch("COMMIT").unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let stored = llm_log_list_impl(&rig.state, None, Some(500), None)
            .unwrap()
            .len() as u64;
        let dropped = llm_gateway_status_impl(&rig.state).log_dropped;
        if stored + dropped > CALLS as u64 {
            assert_eq!(stored + dropped, 1 + CALLS as u64);
            break;
        }
        assert!(Instant::now() < deadline, "rows {stored} dropped {dropped}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ------------------------------------------------------- relative overhead, paired samples

/// Paired samples: for each pair the baseline and the other call run back to back, in alternating
/// order, so a slow moment of the machine hits both.
struct Paired {
    base: Vec<Duration>,
    /// `other - base` of every pair, in microseconds (negative when the other was faster).
    deltas: Vec<i64>,
}

fn micros(duration: Duration) -> i64 {
    i64::try_from(duration.as_micros()).unwrap_or(i64::MAX)
}

fn paired(
    pairs: usize,
    threads: usize,
    base: &(dyn Fn() -> Duration + Sync),
    other: &(dyn Fn() -> Duration + Sync),
) -> Paired {
    let per = pairs / threads;
    // warm-up: connections, caches
    for _ in 0..8 {
        base();
        other();
    }
    let rows: Vec<(Duration, Duration)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    (0..per)
                        .map(|index| {
                            if index % 2 == 0 {
                                let first = base();
                                (first, other())
                            } else {
                                let second = other();
                                (base(), second)
                            }
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect()
    });
    Paired {
        base: rows.iter().map(|(base, _)| *base).collect(),
        deltas: rows
            .iter()
            .map(|(base, other)| micros(*other) - micros(*base))
            .collect(),
    }
}

fn quantile<T: Copy + Ord>(sorted: &[T], fraction: f64) -> T {
    let index = ((sorted.len() as f64 * fraction) as usize).min(sorted.len() - 1);
    sorted[index]
}

/// What one round of pairs says about a limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// The lower quartile of the per-pair differences is within the limit.
    Within,
    /// Over the limit while the baseline was quiet: a real overhead.
    Over,
    /// Over the limit, but the baseline itself was spread wider than `noise`: the machine is
    /// busy and says nothing.
    Inconclusive,
}

fn judge(round: &Paired, limit: Duration, noise: Duration) -> (Verdict, i64) {
    let mut deltas = round.deltas.clone();
    deltas.sort_unstable();
    let lower_quartile = quantile(&deltas, 0.25);
    let mut base = round.base.clone();
    base.sort_unstable();
    let spread = quantile(&base, 0.9).saturating_sub(quantile(&base, 0.1));
    let verdict = if lower_quartile <= micros(limit) {
        Verdict::Within
    } else if spread > noise {
        Verdict::Inconclusive
    } else {
        Verdict::Over
    };
    (verdict, lower_quartile)
}

/// Runs up to three rounds; the case passes on the first round within the limit, or as
/// inconclusive when a noisy round hid the answer. It fails only when every round was quiet and
/// over the limit. Debug builds only report: the budget is a property of the optimised gateway.
fn check(label: &str, limit: Duration, noise: Duration, mut round: impl FnMut() -> Paired) {
    let mut verdicts = Vec::new();
    for _ in 0..3 {
        let (verdict, lower_quartile) = judge(&round(), limit, noise);
        println!(
            "{label}: added (lower quartile of pairs) {lower_quartile} µs, limit {limit:?}: {verdict:?}"
        );
        verdicts.push(verdict);
        if verdict == Verdict::Within {
            return;
        }
    }
    if verdicts.contains(&Verdict::Inconclusive) {
        println!("{label}: inconclusive (the machine was too busy to measure)");
        return;
    }
    if !cfg!(debug_assertions) {
        panic!(
            "{label}: the gateway adds more than {limit:?} over a direct call in every quiet round"
        );
    }
}

#[cfg_attr(
    debug_assertions,
    ignore = "the budget is asserted on release builds only"
)]
#[test]
fn the_added_latency_is_within_the_prd_limits_when_measurable() {
    const PAIRS: usize = 120;
    let upstream = Upstream::start(upstream_handler);
    let rig = rig(upstream.port);
    let direct_port = upstream.port;
    let gateway_port = rig.port;
    let auth = bearer();
    let direct_headers = [("authorization", "Bearer sk-direct")];
    let gateway_headers = [("authorization", auth.as_str())];
    let anthropic = [("x-api-key", TOKEN), ("anthropic-version", "2023-06-01")];
    let (direct_chat, chat) = (chat_body(UPSTREAM_MODEL, false), chat_body(ALIAS, false));
    let messages = messages_body(ALIAS);
    let (direct_many, many) = (chunks_request(UPSTREAM_MODEL), chunks_request(ALIAS));
    let ms = Duration::from_millis;

    let direct_call = || {
        timed_post(
            direct_port,
            "/v1/chat/completions",
            &direct_headers,
            &direct_chat,
        )
    };
    let passthrough = || {
        timed_post(
            gateway_port,
            "/v1/chat/completions",
            &gateway_headers,
            &chat,
        )
    };
    let translated = || timed_post(gateway_port, "/v1/messages", &anthropic, &messages);
    set_capture(&rig.state, "off");
    check("passthrough total (budget 5 ms)", ms(5), ms(3), || {
        paired(PAIRS, CONCURRENCY, &direct_call, &passthrough)
    });
    check("translated total (budget 15 ms)", ms(15), ms(3), || {
        paired(PAIRS, CONCURRENCY, &direct_call, &translated)
    });

    // a long stream: the added time over all chunks, per chunk 1 ms
    let direct_stream = || {
        timed_post(
            direct_port,
            "/v1/chat/completions",
            &direct_headers,
            &direct_many,
        )
    };
    let gateway_stream = || {
        timed_post(
            gateway_port,
            "/v1/chat/completions",
            &gateway_headers,
            &many,
        )
    };
    check(
        "per relayed chunk (budget 1 ms)",
        ms(1) * CHUNKS as u32,
        ms(10),
        || paired(PAIRS / 2, CONCURRENCY, &direct_stream, &gateway_stream),
    );

    // capture on against capture off, one thread: the setting is global
    let capture_off = || {
        set_capture(&rig.state, "off");
        passthrough()
    };
    let capture_on = || {
        set_capture(&rig.state, "all");
        passthrough()
    };
    check("capture on (budget 1 ms)", ms(1), ms(3), || {
        paired(PAIRS, 1, &capture_off, &capture_on)
    });
}

// ----------------------------------------------------------------------------- perf harness

/// Time from sending a streaming request to the first byte of its body.
fn first_body_byte(port: u16, path: &str, headers: &[(&str, &str)], body: &Value) -> Duration {
    let payload = body.to_string();
    let mut head = format!("POST {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        payload.len()
    ));
    let started = Instant::now();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(payload.as_bytes()).unwrap();
    let mut seen = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0, "the stream ended before a body byte");
        seen.extend_from_slice(&chunk[..read]);
        if let Some(end) = seen.windows(4).position(|window| window == b"\r\n\r\n")
            && seen.len() > end + 4
        {
            return started.elapsed();
        }
    }
}

/// Runs `call` `total` times on `threads` threads and returns every duration, sorted.
fn measure(total: usize, threads: usize, call: &(dyn Fn() -> Duration + Sync)) -> Vec<Duration> {
    let per = total / threads;
    let mut all: Vec<Duration> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|_| scope.spawn(|| (0..per).map(|_| call()).collect::<Vec<_>>()))
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect()
    });
    all.sort();
    all
}

/// Prints p50/p95 of a case and returns the p95.
fn report(label: &str, sorted: &[Duration]) -> Duration {
    let (p50, p95) = (quantile(sorted, 0.50), quantile(sorted, 0.95));
    println!("{label:<34} p50 {p50:>10.3?}  p95 {p95:>10.3?}");
    p95
}

/// The absolute table against an instant upstream (1 KiB body, 200 requests at
/// concurrency 4). It depends on the machine being quiet, so it is not part of `cargo test`; run
/// it with `cargo test -p pingdeck-core --release --test gateway_latency -- --ignored --nocapture`.
/// It prints p50 and p95 per case and, in a build without debug assertions, asserts the limits.
#[test]
#[ignore = "perf harness: run explicitly in release mode on a quiet machine"]
fn perf_harness_prints_the_gw_041_table_and_asserts_its_limits() {
    const REQUESTS: usize = 200;
    const WARMUP: usize = 40;
    let upstream = Upstream::start(upstream_handler);
    let rig = rig(upstream.port);
    set_capture(&rig.state, "off");
    let auth = bearer();
    let direct_port = upstream.port;
    let gateway_port = rig.port;
    let direct_headers = [("authorization", "Bearer sk-direct")];
    let gateway_headers = [("authorization", auth.as_str())];
    let anthropic = [("x-api-key", TOKEN), ("anthropic-version", "2023-06-01")];
    let chat = chat_body(ALIAS, false);
    let direct_chat = chat_body(UPSTREAM_MODEL, false);
    let messages = messages_body(ALIAS);
    let stream_via_gateway = chat_body(ALIAS, true);
    let stream_direct = chat_body(UPSTREAM_MODEL, true);

    // --- non-streaming: direct, passthrough, translated
    let direct_call = || {
        timed_post(
            direct_port,
            "/v1/chat/completions",
            &direct_headers,
            &direct_chat,
        )
    };
    measure(WARMUP, CONCURRENCY, &direct_call);
    let direct_total = report(
        "direct, non-streaming",
        &measure(REQUESTS, CONCURRENCY, &direct_call),
    );
    let passthrough = || {
        timed_post(
            gateway_port,
            "/v1/chat/completions",
            &gateway_headers,
            &chat,
        )
    };
    measure(WARMUP, CONCURRENCY, &passthrough);
    let passthrough_total = report(
        "passthrough, non-streaming",
        &measure(REQUESTS, CONCURRENCY, &passthrough),
    );
    let translated = || timed_post(gateway_port, "/v1/messages", &anthropic, &messages);
    measure(WARMUP, CONCURRENCY, &translated);
    let translated_total = report(
        "translated, non-streaming",
        &measure(REQUESTS, CONCURRENCY, &translated),
    );

    // --- streaming first byte: direct, passthrough
    let direct_stream = || {
        first_body_byte(
            direct_port,
            "/v1/chat/completions",
            &direct_headers,
            &stream_direct,
        )
    };
    measure(WARMUP, CONCURRENCY, &direct_stream);
    let direct_first = report(
        "direct, stream first byte",
        &measure(REQUESTS, CONCURRENCY, &direct_stream),
    );
    let gateway_stream = || {
        first_body_byte(
            gateway_port,
            "/v1/chat/completions",
            &gateway_headers,
            &stream_via_gateway,
        )
    };
    measure(WARMUP, CONCURRENCY, &gateway_stream);
    let gateway_first = report(
        "passthrough, stream first byte",
        &measure(REQUESTS, CONCURRENCY, &gateway_stream),
    );

    // --- capture on
    set_capture(&rig.state, "all");
    measure(WARMUP, CONCURRENCY, &passthrough);
    let captured_total = report(
        "passthrough, capture on",
        &measure(REQUESTS, CONCURRENCY, &passthrough),
    );

    // --- a long stream: what each relayed chunk costs
    set_capture(&rig.state, "off");
    let many_direct = || {
        timed_post(
            direct_port,
            "/v1/chat/completions",
            &direct_headers,
            &chunks_request(UPSTREAM_MODEL),
        )
    };
    let many_gateway = || {
        timed_post(
            gateway_port,
            "/v1/chat/completions",
            &gateway_headers,
            &chunks_request(ALIAS),
        )
    };
    measure(WARMUP, CONCURRENCY, &many_direct);
    let many_direct_p95 = report(
        "direct, 200-chunk stream",
        &measure(REQUESTS, CONCURRENCY, &many_direct),
    );
    measure(WARMUP, CONCURRENCY, &many_gateway);
    let many_gateway_p95 = report(
        "passthrough, 200-chunk stream",
        &measure(REQUESTS, CONCURRENCY, &many_gateway),
    );
    let per_chunk = many_gateway_p95.saturating_sub(many_direct_p95) / CHUNKS as u32;

    let added = |gateway: Duration, base: Duration| gateway.saturating_sub(base);
    println!(
        "added, passthrough total        {:?}",
        added(passthrough_total, direct_total)
    );
    println!(
        "added, translated total         {:?}",
        added(translated_total, direct_total)
    );
    println!(
        "added, passthrough first byte   {:?}",
        added(gateway_first, direct_first)
    );
    println!(
        "added, capture on               {:?}",
        added(captured_total, passthrough_total)
    );
    println!("added, per relayed chunk        {per_chunk:?}");

    if !cfg!(debug_assertions) {
        let ms = Duration::from_millis;
        assert!(
            added(passthrough_total, direct_total) <= ms(5),
            "passthrough total"
        );
        assert!(
            added(gateway_first, direct_first) <= ms(5),
            "passthrough first byte"
        );
        assert!(
            added(translated_total, direct_total) <= ms(15),
            "translated total"
        );
        assert!(
            added(captured_total, passthrough_total) <= ms(1),
            "capture on"
        );
        assert!(per_chunk <= ms(1), "per relayed chunk");
    }
}
