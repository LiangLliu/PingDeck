//! End-to-end smoke tests of the LLM layer through the public `spanbay_core` API: the local
//! gateway translating OpenAI, Anthropic and Gemini clients against a fake OpenAI-compatible
//! upstream, the model test bench persisting its results, and the client-config previews against
//! a temp home. Run with `--nocapture` to see the observed responses and rows.
//!
//! Golden paths: G4 provider, model list, in-app call, usage and log (`golden_g4_*`); G5 client
//! token, cross-protocol call and refusal without a token is
//! `gateway_translates_every_client_protocol_and_guards_access`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use common::{
    ALIAS, FINAL_TEXT, Reply, TOKEN, TempDir, UPSTREAM_MODEL, UPSTREAM_SECRET, Upstream,
    configure_gateway, configure_llm, gateway_events, http, openai_chat, sse_events,
    wait_for_requests,
};
use serde_json::{Value, json};
use spanbay_core::app::{
    AppState, ProbeEvent, ProbeItem, ProbeRunInput, SaveProviderKeyInput, llm_gateway_start_impl,
    llm_gateway_status_impl, llm_gateway_stop_impl, llm_key_save_impl, llm_log_list_impl,
    llm_probe_results_impl, llm_probe_run_impl,
};
use spanbay_core::client_config::{ClientApp, ClientConfigRequest, DetectEnv, detect_all, preview};
use spanbay_core::llm::probe::ProbeTest;
use spanbay_core::llm::probe::plan::CheckSelection;

fn weather_tool_anthropic() -> Value {
    json!({"name": "get_weather", "description": "Weather of a city",
        "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}})
}

#[test]
fn gateway_translates_every_client_protocol_and_guards_access() {
    let root = TempDir::new("gateway");
    let upstream = Upstream::start(openai_chat);
    let state = AppState::open(root.join("spanbay.sqlite3")).unwrap();
    configure_llm(&state, upstream.port);
    let port = configure_gateway(&state);
    let events = gateway_events(&state);
    let status = llm_gateway_start_impl(&state).unwrap();
    println!("gateway status: {status:?}");
    assert!(status.running && status.port == port && status.token_required);
    let bearer = format!("Bearer {TOKEN}");

    // --- OpenAI chat client, JSON then stream (same protocol upstream: forwarded).
    let request = json!({"model": ALIAS, "messages": [{"role": "user", "content": "ping"}]});
    let response = http(
        port,
        "POST",
        "/v1/chat/completions",
        &[("authorization", &bearer)],
        Some(&request),
    );
    response.print("openai chat json");
    assert_eq!(response.status, 200);
    assert_eq!(
        response.json()["choices"][0]["message"]["content"],
        FINAL_TEXT
    );

    let request = json!({"model": ALIAS, "stream": true,
        "messages": [{"role": "user", "content": "ping"}]});
    let response = http(
        port,
        "POST",
        "/v1/chat/completions",
        &[("authorization", &bearer)],
        Some(&request),
    );
    response.print("openai chat stream");
    assert_eq!(response.status, 200);
    assert!(
        response
            .header("content-type")
            .is_some_and(|value| value.starts_with("text/event-stream"))
    );
    let frames = sse_events(&response.text());
    let streamed: String = frames
        .iter()
        .filter_map(|(_, data)| data["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(streamed, FINAL_TEXT);
    assert_eq!(frames.last().unwrap().1, json!("[DONE]"));

    // --- Anthropic client with a tool: the OpenAI upstream answers with a tool call.
    let anthropic_headers = [("x-api-key", TOKEN), ("anthropic-version", "2023-06-01")];
    let request = json!({"model": ALIAS, "max_tokens": 64, "tools": [weather_tool_anthropic()],
        "messages": [{"role": "user", "content": "weather in Paris?"}]});
    let response = http(
        port,
        "POST",
        "/v1/messages",
        &anthropic_headers,
        Some(&request),
    );
    response.print("anthropic messages json (tool_use)");
    assert_eq!(response.status, 200);
    let message = response.json();
    assert_eq!(message["stop_reason"], "tool_use");
    let block = message["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["type"] == "tool_use")
        .unwrap_or_else(|| panic!("no tool_use block in {message}"));
    assert_eq!(block["name"], "get_weather");
    assert_eq!(block["input"], json!({"city": "Paris"}));

    let mut request = request;
    request["stream"] = json!(true);
    let response = http(
        port,
        "POST",
        "/v1/messages",
        &anthropic_headers,
        Some(&request),
    );
    response.print("anthropic messages stream (tool_use)");
    assert_eq!(response.status, 200);
    let frames = sse_events(&response.text());
    let names: Vec<&str> = frames
        .iter()
        .filter_map(|(name, _)| name.as_deref())
        .collect();
    assert_eq!(names.first(), Some(&"message_start"), "{names:?}");
    assert_eq!(names.last(), Some(&"message_stop"), "{names:?}");
    let started = frames
        .iter()
        .find(|(_, data)| data["content_block"]["type"] == "tool_use")
        .unwrap_or_else(|| panic!("no tool_use block start in {names:?}"));
    assert_eq!(started.1["content_block"]["name"], "get_weather");
    let arguments: String = frames
        .iter()
        .filter_map(|(_, data)| data["delta"]["partial_json"].as_str())
        .collect();
    assert_eq!(arguments, r#"{"city":"Paris"}"#);
    assert!(
        frames
            .iter()
            .any(|(_, data)| data["delta"]["stop_reason"] == "tool_use")
    );

    // --- Gemini client: `?key=` token, JSON then SSE; the model is percent-encoded in the path.
    let request = json!({"contents": [{"role": "user", "parts": [{"text": "ping"}]}]});
    let model = ALIAS.replace(' ', "%20").replace('/', "%2F");
    let path = format!("/v1beta/models/{model}:generateContent?key={TOKEN}");
    let response = http(port, "POST", &path, &[], Some(&request));
    response.print("gemini generateContent");
    assert_eq!(response.status, 200);
    assert_eq!(
        response.json()["candidates"][0]["content"]["parts"][0]["text"],
        FINAL_TEXT
    );
    let path = format!("/v1beta/models/{model}:streamGenerateContent?alt=sse&key={TOKEN}");
    let response = http(port, "POST", &path, &[], Some(&request));
    response.print("gemini streamGenerateContent");
    assert_eq!(response.status, 200);
    let streamed: String = sse_events(&response.text())
        .iter()
        .filter_map(|(_, data)| data["candidates"][0]["content"]["parts"][0]["text"].as_str())
        .map(str::to_owned)
        .collect();
    assert_eq!(streamed, FINAL_TEXT);

    // --- What the upstream saw: the key and model of the target, never the client token.
    let seen = upstream.requests();
    assert_eq!(seen.len(), 6, "{seen:#?}");
    for request in &seen {
        assert_eq!(request.path, "/v1/chat/completions");
        assert_eq!(request.body["model"], UPSTREAM_MODEL);
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some(format!("Bearer {UPSTREAM_SECRET}").as_str())
        );
    }
    for request in &seen[2..4] {
        assert_eq!(request.body["tools"][0]["function"]["name"], "get_weather");
    }
    println!(
        "upstream saw {} requests, e.g. {}",
        seen.len(),
        seen[2]
            .body
            .to_string()
            .chars()
            .take(300)
            .collect::<String>()
    );

    // --- Every request wrote one log row and pushed one event.
    let logged = wait_for_requests(&events, 6);
    for entry in &logged {
        println!(
            "log: {} {} -> {:?}/{:?} status={} outcome={} stream={} in/out={:?}/{:?}",
            entry.protocol_in,
            entry.requested_model,
            entry.provider_id,
            entry.upstream_model,
            entry.status,
            entry.outcome,
            entry.stream,
            entry.input_tokens,
            entry.output_tokens
        );
        assert_eq!((entry.status, entry.outcome.as_str()), (200, "success"));
        assert_eq!(entry.requested_model, ALIAS);
    }
    let stored = llm_log_list_impl(&state, None, Some(50), None).unwrap();
    assert_eq!(stored.len(), 6);
    assert_eq!(llm_gateway_status_impl(&state).requests, 6);

    // --- Access control: token styles, then Host and Origin.
    let models = |headers: &[(&str, &str)], query: &str| {
        http(port, "GET", &format!("/v1/models{query}"), headers, None)
    };
    let missing = models(&[], "");
    missing.print("no token");
    assert_eq!(missing.status, 401);
    let wrong = models(&[("authorization", "Bearer not-the-token")], "");
    wrong.print("wrong token");
    assert_eq!(wrong.status, 401);
    let wrong_query = models(&[], "?key=not-the-token");
    assert_eq!(wrong_query.status, 401);
    let by_bearer = models(&[("authorization", &bearer)], "");
    by_bearer.print("bearer token");
    assert_eq!(by_bearer.status, 200);
    let body = by_bearer.json();
    let listed: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|model| model["id"].as_str())
        .collect();
    assert!(listed.contains(&ALIAS), "{listed:?}");
    assert_eq!(models(&[("x-api-key", TOKEN)], "").status, 200);
    assert_eq!(models(&[], &format!("?key={TOKEN}")).status, 200);
    assert_eq!(models(&[("x-goog-api-key", TOKEN)], "").status, 200);

    let foreign_host = models(&[("host", "evil.example"), ("authorization", &bearer)], "");
    foreign_host.print("foreign Host header");
    assert_eq!(foreign_host.status, 403);
    let with_origin = models(
        &[
            ("origin", "http://evil.example"),
            ("authorization", &bearer),
        ],
        "",
    );
    with_origin.print("Origin header");
    assert_eq!(with_origin.status, 403);
    let health = http(port, "GET", "/health", &[], None);
    health.print("health without token");
    assert_eq!(health.status, 200);

    let stopped = llm_gateway_stop_impl(&state);
    println!("stopped: {stopped:?}");
    assert!(!stopped.running);
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn model_test_bench_runs_against_the_fake_upstream_and_persists_every_row() {
    let root = TempDir::new("probe");
    // A wrong key is refused with 401, like a real provider would.
    let upstream = Upstream::start(|number, request| {
        let authorized = request.headers.get("authorization").map(String::as_str)
            == Some(format!("Bearer {UPSTREAM_SECRET}").as_str());
        if authorized {
            openai_chat(number, request)
        } else {
            Reply::Json(401, json!({"error": {"message": "invalid api key"}}))
        }
    });
    let state = AppState::open(root.join("spanbay.sqlite3")).unwrap();
    let setup = configure_llm(&state, upstream.port);
    let bad_key = llm_key_save_impl(
        &state,
        SaveProviderKeyInput {
            id: "bad-key".into(),
            provider_id: setup.provider.id.clone(),
            name: "Wrong key".into(),
            secret: Some("sk-wrong".into()),
            enabled: true,
            position: 1,
            api_user_id: None,
            management_token: None,
            notes: None,
            group_name: None,
            protocols: None,
        },
    )
    .unwrap();

    let pushed = Mutex::new(Vec::new());
    let sink = |event: ProbeEvent| {
        pushed.lock().unwrap().push(event);
        true
    };
    let summary = llm_probe_run_impl(
        &state,
        ProbeRunInput {
            items: vec![ProbeItem {
                key_id: setup.key.id.clone(),
                model_id: UPSTREAM_MODEL.into(),
            }],
            checks: CheckSelection::Custom {
                tests: vec![ProbeTest::Chat, ProbeTest::Stream, ProbeTest::Tools],
            },
            protocol: None,
            concurrency: None,
            retry_of: None,
            remaining_of: None,
            alias: None,
            confirmed_cost: false,
        },
        None,
        &sink,
    )
    .await
    .unwrap();
    println!("good key summary: {summary:?}");
    assert_eq!((summary.total, summary.passed, summary.failed), (3, 3, 0));

    let rows = llm_probe_results_impl(&state, Some(&setup.key.id)).unwrap();
    for row in &rows {
        println!("persisted: {}", serde_json::to_string(row).unwrap());
    }
    assert_eq!(rows.len(), 3);
    let by_test: BTreeMap<&str, _> = rows.iter().map(|row| (row.test.as_str(), row)).collect();
    assert_eq!(
        by_test.keys().copied().collect::<Vec<_>>(),
        ["chat", "stream", "tools"]
    );
    for row in &rows {
        assert!(row.ok, "{row:?}");
        assert_eq!(
            (row.endpoint.as_str(), row.model_id.as_str()),
            ("openai_chat", UPSTREAM_MODEL)
        );
        assert!(row.latency_ms.is_some() && row.checked_at_ms > 0);
    }
    assert!(by_test["chat"].message.contains(FINAL_TEXT));
    assert!(
        by_test["stream"].ttft_ms.is_some(),
        "stream measures first token"
    );
    assert_eq!(by_test["tools"].detail["called"], json!({"city": "Paris"}));
    assert_eq!(by_test["chat"].output_tokens, Some(7));

    // The pushed events are exactly the stored rows plus the closing summary.
    {
        let pushed = pushed.lock().unwrap();
        let mut pushed_tests: Vec<&str> = pushed
            .iter()
            .filter_map(|event| match event {
                ProbeEvent::Cell { cell, .. } => Some(cell.test.as_str()),
                _ => None,
            })
            .collect();
        pushed_tests.sort_unstable();
        assert_eq!(pushed_tests, ["chat", "stream", "tools"]);
        assert!(
            matches!(pushed.last(), Some(ProbeEvent::RunFinished { summary: s }) if s == &summary)
        );
    }

    // The wrong key fails, and the failure is persisted and listed too.
    let summary = llm_probe_run_impl(
        &state,
        ProbeRunInput {
            items: vec![ProbeItem {
                key_id: bad_key.id.clone(),
                model_id: UPSTREAM_MODEL.into(),
            }],
            checks: CheckSelection::Custom {
                tests: vec![ProbeTest::Chat],
            },
            protocol: None,
            concurrency: None,
            retry_of: None,
            remaining_of: None,
            alias: None,
            confirmed_cost: false,
        },
        None,
        &spanbay_core::model::events::NoEvents,
    )
    .await
    .unwrap();
    println!("bad key summary: {summary:?}");
    assert_eq!((summary.total, summary.passed, summary.failed), (1, 0, 1));
    let failed = llm_probe_results_impl(&state, Some("bad-key")).unwrap();
    println!("persisted: {}", serde_json::to_string(&failed[0]).unwrap());
    assert_eq!(failed.len(), 1);
    assert!(!failed[0].ok);
    assert_eq!(failed[0].status, Some(401));
    assert!(failed[0].failure_kind.is_some());
    assert_eq!(llm_probe_results_impl(&state, None).unwrap().len(), 4);

    // A reopened state still has the rows (they are in the database file, not in memory).
    drop(state);
    let reopened = AppState::open(root.join("spanbay.sqlite3")).unwrap();
    assert_eq!(llm_probe_results_impl(&reopened, None).unwrap().len(), 4);
}

/// Every file under `dir` as relative path to contents.
fn snapshot(dir: &Path) -> BTreeMap<String, String> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                let rel = path.strip_prefix(base).unwrap().display().to_string();
                out.insert(rel, fs::read_to_string(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

#[test]
fn client_config_previews_show_a_diff_for_each_tool_and_write_nothing() {
    // `llm_client_preview_impl` reads `$HOME` and its `_in` variant is crate-private, so this
    // goes through the public `client_config::preview(home, request)` that it wraps, with
    // the request built the way `llm_client_preview_in` builds it from the settings.
    let home = TempDir::new("client-home");
    let state = AppState::open_in_memory().unwrap();
    let port = configure_gateway(&state);
    let gateway = state.settings().ai.gateway.clone();
    let gateway_url = format!("http://127.0.0.1:{}", gateway.port);
    assert_eq!(gateway.port, port);

    let existing = [
        (
            ".claude/settings.json",
            r#"{"theme": "dark", "env": {"FOO": "bar"}}"#,
            "\"theme\": \"dark\"",
        ),
        (
            ".codex/config.toml",
            "# my codex config\nmodel = \"gpt-5\"\n\n[tui]\nnotifications = true\n",
            "# my codex config",
        ),
        (
            ".config/opencode/opencode.json",
            r#"{"theme": "tokyonight"}"#,
            "tokyonight",
        ),
        (
            ".gemini/settings.json",
            r#"{"ui": {"theme": "GitHub"}}"#,
            "GitHub",
        ),
    ];
    for (rel, text, _) in existing {
        let path = home.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let detected = detect_all(&DetectEnv::home_only(home.path()));
    assert!(
        detected.iter().all(|client| client.installed),
        "{detected:?}"
    );
    let before = snapshot(home.path());

    for app in ClientApp::ALL {
        let plan = preview(
            home.path(),
            &ClientConfigRequest {
                app,
                gateway_url: gateway_url.clone(),
                client_name: "Smoke client".to_owned(),
                token: TOKEN.to_owned(),
                alias: Some(ALIAS.to_owned()),
                fast_alias: None,
                facts: Default::default(),
            },
        )
        .unwrap();
        println!("===== {} =====", app.as_str());
        for file in &plan.files {
            println!(
                "--- {} (existed: {}, changed: {})\n{}",
                file.path,
                file.before.is_some(),
                file.changed,
                file.diff
            );
        }
        assert!(plan.files.iter().any(|file| file.changed), "{app:?}");
        assert!(
            plan.files
                .iter()
                .any(|file| file.diff.contains(&gateway_url)),
            "{app:?}: no diff names {gateway_url}"
        );
        // A preview never shows the real token: before, after and diff carry its mask.
        assert!(
            plan.files.iter().all(|file| {
                !file.diff.contains(TOKEN)
                    && !file.after.contains(TOKEN)
                    && file
                        .before
                        .as_deref()
                        .is_none_or(|before| !before.contains(TOKEN))
            }),
            "{app:?}: the real token leaked into the preview"
        );
        println!(
            "token masked in preview: {}",
            plan.files
                .iter()
                .any(|file| file.after.contains("••••") || file.after.contains("<gateway token>"))
        );
        assert!(plan.files.iter().any(|file| file.after.contains(ALIAS)));
    }
    // Existing content of the tools' own files survives in the planned result.
    for (rel, _, marker) in existing {
        let plan = ClientApp::ALL
            .into_iter()
            .map(|app| {
                preview(
                    home.path(),
                    &ClientConfigRequest {
                        app,
                        gateway_url: gateway_url.clone(),
                        client_name: "Smoke client".to_owned(),
                        token: TOKEN.to_owned(),
                        alias: Some(ALIAS.to_owned()),
                        fast_alias: None,
                        facts: Default::default(),
                    },
                )
                .unwrap()
            })
            .flat_map(|plan| plan.files)
            .find(|file| file.path.replace('\\', "/").ends_with(rel))
            .unwrap_or_else(|| panic!("no plan for {rel}"));
        assert!(
            plan.after.contains(marker),
            "{rel} lost `{marker}`: {}",
            plan.after
        );
    }
    assert_eq!(
        snapshot(home.path()),
        before,
        "preview must not write or back up anything"
    );
    assert!(!before.keys().any(|name| name.ends_with(".bak")));
}

/// G4: a provider at a fake upstream lists its models, the listed model is taken in,
/// one AI request from the app runs on it, and the call shows in the request log and in usage.
#[tokio::test(flavor = "multi_thread")]
async fn golden_g4_provider_models_in_app_call_usage_and_log() {
    use common::{openai_reply, request_input};
    use spanbay_core::app::{
        AiRequestRunInput, ai_request_run_impl, llm_model_list_impl, llm_models_drift_apply_impl,
        llm_models_refresh_impl, llm_provider_save_impl, llm_usage_summary_impl,
        workspace_create_impl,
    };
    use spanbay_core::llm::ai_run::AiRunStatus;
    use spanbay_core::llm::usage::UsageQuery;
    use spanbay_core::llm::wire::Protocol;
    use spanbay_core::model::ai_request::{AiRequestConfig, AiTarget, RequestMode};
    use spanbay_core::model::models::{HttpMethod, RequestBody};

    const LISTED: &str = "fake-model-2";
    let root = TempDir::new("golden-g4");
    let upstream = Upstream::start(|_, request| match request.path.as_str() {
        "/v1/models" => Reply::Json(
            200,
            json!({"object": "list", "data": [
                {"id": UPSTREAM_MODEL, "object": "model", "owned_by": "fake"},
                {"id": LISTED, "object": "model", "owned_by": "fake"}]}),
        ),
        "/v1/chat/completions" => openai_reply(&request.body, None),
        _ => Reply::Json(404, json!({"error": {"message": "no such route"}})),
    });
    let state = AppState::open(root.join("spanbay.sqlite3")).unwrap();
    workspace_create_impl(&state, "ws_golden".into(), "Golden".into()).unwrap();
    let mut provider = configure_llm(&state, upstream.port).provider;
    provider.models_endpoint = Some("/v1/models".into());
    let provider = llm_provider_save_impl(&state, provider).unwrap();

    // --- Model list: the unknown listed model is offered, then taken in.
    let drift = llm_models_refresh_impl(&state, &provider.id, None, None)
        .await
        .unwrap();
    println!("drift: {drift:?}");
    assert_eq!(drift.added, [LISTED]);
    assert!(drift.removed.is_empty() && drift.key_failures.is_empty());
    llm_models_drift_apply_impl(&state, &provider.id, None).unwrap();
    let mut stored: Vec<String> = llm_model_list_impl(&state, Some(&provider.id))
        .unwrap()
        .into_iter()
        .map(|model| model.model_id)
        .collect();
    stored.sort();
    assert_eq!(stored, [UPSTREAM_MODEL, LISTED]);

    // --- One AI request from the API workbench on the listed model.
    let started_ms = now_ms();
    let mut request = request_input("ws_golden", "ai_g4", "/chat/completions".into());
    request.method = HttpMethod::Post;
    request.mode = RequestMode::AiApi;
    request.body = RequestBody::Json {
        content: json!({"model": LISTED, "messages": [{"role": "user", "content": "ping"}]})
            .to_string(),
    };
    request.ai = Some(AiRequestConfig {
        protocol: Protocol::OpenaiChat,
        target: AiTarget::Provider {
            provider_id: provider.id.clone(),
            provider_name: provider.name.clone(),
            base_url_hint: String::new(),
            model: LISTED.into(),
        },
        builder: None,
        compare_models: Vec::new(),
    });
    let result = ai_request_run_impl(
        &state,
        AiRequestRunInput {
            workspace_id: "ws_golden".into(),
            request_id: None,
            request: Some(request),
            environment_id: None,
            confirmed: None,
            keep_exchange: true,
        },
        None,
        &spanbay_core::model::events::NoEvents,
    )
    .await
    .unwrap();
    println!("ai run: {result:?}");
    assert_eq!(result.status, AiRunStatus::Done);
    assert_eq!(result.reply, FINAL_TEXT);
    assert_eq!(
        (result.usage.input_tokens, result.usage.output_tokens),
        (Some(11), Some(7))
    );
    let seen = upstream.requests();
    let call = seen.last().unwrap();
    assert_eq!(
        (call.path.as_str(), &call.body["model"]),
        ("/v1/chat/completions", &json!(LISTED))
    );
    assert_eq!(
        call.headers.get("authorization").map(String::as_str),
        Some(format!("Bearer {UPSTREAM_SECRET}").as_str())
    );

    // --- The request log and usage both have the call.
    let logged = llm_log_list_impl(&state, None, Some(10), None).unwrap();
    assert_eq!(logged.len(), 1, "{logged:?}");
    let row = &logged[0];
    assert_eq!(Some(row.id), result.log_row_id);
    assert_eq!(
        (
            row.source.as_str(),
            row.provider_id.as_deref(),
            row.upstream_model.as_deref(),
            row.status,
            row.outcome.as_str(),
            row.input_tokens,
            row.output_tokens,
        ),
        (
            "api",
            Some(provider.id.as_str()),
            Some(LISTED),
            200,
            "success",
            Some(11),
            Some(7)
        )
    );
    let usage = llm_usage_summary_impl(
        &state,
        UsageQuery {
            provider_id: None,
            key_id: None,
            client_id: None,
            from_ms: started_ms - 3_600_000,
            to_ms: now_ms() + 3_600_000,
            group_by: Vec::new(),
            sources: Vec::new(),
            tz_offset_minutes: 0,
            bucket_minutes: Some(60),
        },
    )
    .await
    .unwrap();
    println!("usage totals: {:?}", usage.totals);
    assert_eq!(
        (
            usage.totals.requests,
            usage.totals.input_tokens,
            usage.totals.output_tokens
        ),
        (1, 11, 7)
    );
    assert!(
        usage
            .by_model
            .iter()
            .any(|row| row.model == LISTED && row.usage.requests == 1),
        "{:?}",
        usage.by_model
    );
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
