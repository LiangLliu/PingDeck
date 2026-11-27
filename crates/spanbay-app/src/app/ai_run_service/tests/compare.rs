//! "Run on N models": limits, skips, concurrency, stop, per-row log rows and history entries,
//! the cost estimate, the export and Detect.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

use super::super::compare::{idle_row, row_of, tag_best};
use super::support::{Fx, WS, body_for, pong, upstream};
use crate::app::{
    AiCompareEvent, AiCompareExportFormat, AiCompareResultDto, AiCompareRow, AiCompareRunInput,
    AiDetectInput, ai_compare_estimate_impl, ai_compare_export_impl, ai_compare_run_impl,
    ai_detect_impl, ai_request_cancel_impl, history_get_impl,
};
use crate::error::AppError;
use crate::events::NoEvents;
use crate::llm::ai_run::{AiResultDto, AiRunStatus};
use crate::llm::gateway::tests::{Fake, Reply, Step, canned_json};
use crate::llm::usage::{Cost, PriceOverride, PriceSource};
use crate::llm::wire::{AuthStyle, Protocol};
use crate::workspace::ai_request::CompareModel;

fn entry(provider: &str, model: &str) -> CompareModel {
    CompareModel {
        provider_id: provider.to_owned(),
        model: model.to_owned(),
    }
}

fn input(models: Vec<CompareModel>, concurrency: u8) -> AiCompareRunInput {
    AiCompareRunInput {
        workspace_id: WS.to_owned(),
        request_id: Some("r1".to_owned()),
        request: None,
        environment_id: None,
        confirmed: None,
        keep_exchange: true,
        models,
        concurrency,
    }
}

async fn compare(
    fx: &Fx,
    input: AiCompareRunInput,
    token: Option<&str>,
) -> Result<AiCompareResultDto, AppError> {
    ai_compare_run_impl(&fx.state, input, token.map(str::to_owned), &NoEvents).await
}

/// A fake that answers Chat Completions `pong`, after `delay`, and notes when each call came.
async fn timed_upstream(delay: Duration) -> (Fake, Arc<Mutex<Vec<Instant>>>) {
    let arrivals: Arc<Mutex<Vec<Instant>>> = Arc::default();
    let seen = Arc::clone(&arrivals);
    let fake = Fake::start(move |_, request| {
        seen.lock().unwrap().push(Instant::now());
        let answer = Reply::Json(200, canned_json(Protocol::OpenaiChat, "m", &pong()));
        let _ = request;
        Reply::After(delay, Box::new(answer))
    })
    .await;
    (fake, arrivals)
}

fn base_request(fx: &Fx, provider: &str) {
    fx.provider_request(
        "r1",
        provider,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", false),
    );
}

#[tokio::test]
async fn the_same_body_goes_to_each_model_with_only_the_model_replaced() {
    let a = upstream(Protocol::OpenaiChat, pong()).await;
    let b = upstream(Protocol::OpenaiChat, pong()).await;
    let fx = Fx::new();
    let pa = fx.chat_provider("Alpha", a.port);
    let pb = fx.chat_provider("Beta", b.port);
    base_request(&fx, &pa);
    let result = compare(
        &fx,
        input(
            vec![
                entry(&pa, "m-one"),
                entry(&pb, "m-two"),
                entry(&pa, "m-three"),
            ],
            3,
        ),
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.rows.len(), 3);
    assert!(
        result
            .rows
            .iter()
            .all(|row| row.status == AiRunStatus::Done && row.reply_line == "pong")
    );
    let mut expected =
        serde_json::from_str::<serde_json::Value>(&body_for(Protocol::OpenaiChat, "m", false))
            .unwrap();
    let seen_a = a.requests();
    let seen_b = b.requests();
    assert_eq!(seen_a.len(), 2);
    assert_eq!(seen_b.len(), 1);
    expected["model"] = json!("m-two");
    assert_eq!(seen_b[0].body, expected, "only `model` differs");
    let mut models: Vec<_> = seen_a
        .iter()
        .map(|r| r.body["model"].as_str().unwrap().to_owned())
        .collect();
    models.sort();
    assert_eq!(models, ["m-one", "m-three"]);
    assert_eq!(result.rows[0].provider_name, "Alpha");
    assert_eq!(result.rows[1].model, "m-two");
    assert!(result.request_body.contains("\"max_tokens\":64"));
}

#[tokio::test]
async fn every_row_has_its_own_log_row_and_history_entry_sharing_the_run_id() {
    let a = upstream(Protocol::OpenaiChat, pong()).await;
    let fx = Fx::new();
    let pa = fx.chat_provider("Alpha", a.port);
    base_request(&fx, &pa);
    let result = compare(
        &fx,
        input(
            vec![
                entry(&pa, "m-one"),
                entry(&pa, "m-two"),
                entry(&pa, "m-three"),
            ],
            3,
        ),
        None,
    )
    .await
    .unwrap();
    let mut ids = Vec::new();
    for row in &result.rows {
        let dto = row.result.as_ref().unwrap();
        let id = dto.log_row_id.expect("a log row per model");
        let store = fx.state.store();
        let log = crate::llm::store::get_log(store.connection(), id)
            .unwrap()
            .unwrap();
        assert_eq!(log.entry.source, "api");
        assert_eq!(
            log.entry.source_ref.as_deref(),
            Some(result.run_id.as_str())
        );
        assert_eq!(log.entry.requested_model, row.model);
        assert!(log.entry.has_capture);
        ids.push(id);
    }
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 3);
    let page = crate::app::misc_logic_service::history_page_impl(
        &fx.state,
        WS.to_owned(),
        crate::workspace::HistoryFilter::default(),
        None,
        None,
    )
    .unwrap();
    assert_eq!(page.entries.len(), 3);
    for entry in &page.entries {
        let full =
            history_get_impl(&fx.state, WS.to_owned(), entry.id.as_str().to_owned()).unwrap();
        assert_eq!(full.run_id.as_deref(), Some(result.run_id.as_str()));
        assert_eq!(full.protocol, "openai_chat");
        let snapshot = full.request_snapshot.unwrap();
        let model = snapshot.ai.unwrap().target.model().to_owned();
        assert!(
            ["m-one", "m-two", "m-three"].contains(&model.as_str()),
            "{model}"
        );
    }
}

#[tokio::test]
async fn a_model_whose_provider_lacks_the_protocol_is_skipped_without_a_request() {
    let a = upstream(Protocol::OpenaiChat, pong()).await;
    let claude = upstream(Protocol::AnthropicMessages, pong()).await;
    let fx = Fx::new();
    let pa = fx.chat_provider("Alpha", a.port);
    let pc = fx.provider(
        "Claude",
        claude.port,
        AuthStyle::XApiKey,
        &[(Protocol::AnthropicMessages, "/v1", None)],
    );
    base_request(&fx, &pa);
    let result = compare(
        &fx,
        input(vec![entry(&pa, "m-one"), entry(&pc, "sonnet")], 3),
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.rows[0].status, AiRunStatus::Done);
    let skipped = &result.rows[1];
    assert_eq!(skipped.status, AiRunStatus::Skipped);
    assert_eq!(
        skipped.message.as_deref(),
        Some("Skipped: no Chat Completions endpoint")
    );
    assert!(skipped.result.is_none());
    assert_eq!(claude.count(), 0);
}

#[tokio::test]
async fn the_limits_are_twelve_models_and_one_to_six_calls_at_once() {
    let a = upstream(Protocol::OpenaiChat, pong()).await;
    let fx = Fx::new();
    let pa = fx.chat_provider("Alpha", a.port);
    base_request(&fx, &pa);
    let models = |count: usize| -> Vec<CompareModel> {
        (0..count).map(|n| entry(&pa, &format!("m-{n}"))).collect()
    };
    let error = compare(&fx, input(models(13), 3), None).await.unwrap_err();
    assert_eq!(error.message(), "At most 12 models per run");
    assert!(compare(&fx, input(vec![], 3), None).await.is_err());
    assert!(compare(&fx, input(models(2), 0), None).await.is_err());
    assert!(compare(&fx, input(models(2), 7), None).await.is_err());
    assert_eq!(a.count(), 0, "a refused run sends nothing");
    let result = compare(&fx, input(models(12), 6), None).await.unwrap();
    assert_eq!(result.rows.len(), 12);
    assert_eq!(a.count(), 12);
}

#[tokio::test]
async fn at_most_the_concurrency_calls_are_in_flight_and_three_start_together_by_default() {
    let (fake, arrivals) = timed_upstream(Duration::from_millis(400)).await;
    let fx = Fx::new();
    let pa = fx.chat_provider("Alpha", fake.port);
    base_request(&fx, &pa);
    let models: Vec<CompareModel> = (0..6).map(|n| entry(&pa, &format!("m-{n}"))).collect();
    let started = Instant::now();
    let result = compare(&fx, input(models.clone(), 2), None).await.unwrap();
    let elapsed = started.elapsed();
    assert!(
        result
            .rows
            .iter()
            .all(|row| row.status == AiRunStatus::Done)
    );
    assert!(
        elapsed >= Duration::from_millis(1200),
        "three waves of 400 ms: {elapsed:?}"
    );
    let times = arrivals.lock().unwrap().clone();
    let first = times.iter().min().unwrap();
    let together = times
        .iter()
        .filter(|at| at.duration_since(*first) < Duration::from_millis(250))
        .count();
    assert_eq!(together, 2, "two calls in the first wave");

    arrivals.lock().unwrap().clear();
    let started = Instant::now();
    let _ = compare(&fx, input(models[..3].to_vec(), 3), None)
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_millis(1100));
    let times = arrivals.lock().unwrap().clone();
    let first = times.iter().min().unwrap();
    assert!(
        times
            .iter()
            .all(|at| at.duration_since(*first) < Duration::from_millis(100)),
        "all three start within 100 ms"
    );
}

#[tokio::test]
async fn stop_all_ends_every_call_within_a_second_and_marks_them_stopped() {
    let fake = Fake::always(Reply::Hang).await;
    let fx = Fx::new();
    let pa = fx.chat_provider("Alpha", fake.port);
    base_request(&fx, &pa);
    let models: Vec<CompareModel> = (0..3).map(|n| entry(&pa, &format!("m-{n}"))).collect();
    let state = fx.state.clone();
    let waiting = Arc::new(fake);
    let stopper = {
        let fake = Arc::clone(&waiting);
        tokio::spawn(async move {
            while fake.count() < 3 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let pressed = Instant::now();
            assert!(ai_request_cancel_impl(&state, "cmp".to_owned()));
            pressed
        })
    };
    let result = compare(&fx, input(models, 3), Some("cmp")).await.unwrap();
    let pressed = stopper.await.unwrap();
    assert!(
        pressed.elapsed() < Duration::from_secs(1),
        "{:?}",
        pressed.elapsed()
    );
    assert!(
        result
            .rows
            .iter()
            .all(|row| row.status == AiRunStatus::Stopped),
        "{:?}",
        result.rows
    );
}

#[tokio::test]
async fn one_row_can_be_stopped_alone() {
    let fake = Fake::start(|_, request| {
        if request.body["model"] == "slow" {
            Reply::Hang
        } else {
            Reply::Json(200, canned_json(Protocol::OpenaiChat, "m", &pong()))
        }
    })
    .await;
    let fx = Fx::new();
    let pa = fx.chat_provider("Alpha", fake.port);
    base_request(&fx, &pa);
    let models = vec![
        entry(&pa, "fast-1"),
        entry(&pa, "slow"),
        entry(&pa, "fast-2"),
    ];
    let state = fx.state.clone();
    let waiting = Arc::new(fake);
    let stopper = {
        let fake = Arc::clone(&waiting);
        tokio::spawn(async move {
            while fake.count() < 3 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let pressed = Instant::now();
            assert!(ai_request_cancel_impl(&state, "cmp#1".to_owned()));
            pressed
        })
    };
    let result = compare(&fx, input(models, 3), Some("cmp")).await.unwrap();
    let pressed = stopper.await.unwrap();
    assert!(pressed.elapsed() < Duration::from_secs(1));
    assert_eq!(result.rows[0].status, AiRunStatus::Done);
    assert_eq!(result.rows[1].status, AiRunStatus::Stopped);
    assert_eq!(result.rows[2].status, AiRunStatus::Done);
}

#[tokio::test]
async fn events_announce_the_run_each_row_and_the_end() {
    let a = upstream(Protocol::OpenaiChat, pong()).await;
    let fx = Fx::new();
    let pa = fx.chat_provider("Alpha", a.port);
    base_request(&fx, &pa);
    let events: Arc<Mutex<Vec<AiCompareEvent>>> = Arc::default();
    let sink = {
        let events = Arc::clone(&events);
        move |event: AiCompareEvent| {
            events.lock().unwrap().push(event);
            true
        }
    };
    let result = ai_compare_run_impl(
        &fx.state,
        input(vec![entry(&pa, "m-one"), entry(&pa, "m-two")], 1),
        None,
        &sink,
    )
    .await
    .unwrap();
    let events = events.lock().unwrap();
    assert!(matches!(
        &events[0],
        AiCompareEvent::Started { total: 2, .. }
    ));
    assert!(
        matches!(events.last().unwrap(), AiCompareEvent::Done { result: done } if **done == result)
    );
    let started = events
        .iter()
        .filter(|e| matches!(e, AiCompareEvent::RowStarted { .. }))
        .count();
    let done = events
        .iter()
        .filter(|e| matches!(e, AiCompareEvent::RowDone { .. }))
        .count();
    assert_eq!((started, done), (2, 2));
}

#[tokio::test]
async fn streaming_rows_send_coalesced_frames_between_their_start_and_their_end() {
    const DELAY_MS: u64 = 60;
    let mut steps = vec![Step::Pause(Duration::from_millis(DELAY_MS))];
    for index in 0..30 {
        steps.push(Step::Frame(format!(
            "data: {}\n\n",
            json!({"id": "c", "model": "m", "choices": [{"index": 0, "delta": {"content": format!("{index} ")}}]})
        )));
        steps.push(Step::Pause(Duration::from_millis(5)));
    }
    steps.push(Step::Frame("data: [DONE]\n\n".to_owned()));
    let fake = Fake::always(Reply::Sse(steps)).await;
    let fx = Fx::new();
    let pa = fx.chat_provider("Alpha", fake.port);
    fx.provider_request(
        "r1",
        &pa,
        Protocol::OpenaiChat,
        "m",
        &body_for(Protocol::OpenaiChat, "m", true),
    );
    let events: Arc<Mutex<Vec<AiCompareEvent>>> = Arc::default();
    let sink = {
        let events = Arc::clone(&events);
        move |event: AiCompareEvent| {
            events.lock().unwrap().push(event);
            true
        }
    };
    let models = vec![entry(&pa, "m-1"), entry(&pa, "m-2"), entry(&pa, "m-3")];
    let started = Instant::now();
    let result = ai_compare_run_impl(&fx.state, input(models, 3), None, &sink)
        .await
        .unwrap();
    let elapsed = started.elapsed();
    let events = events.lock().unwrap();
    assert!(matches!(
        &events[0],
        AiCompareEvent::Started { total: 3, .. }
    ));
    assert!(matches!(
        events.last().unwrap(),
        AiCompareEvent::Done { .. }
    ));
    // 30 content frames and `[DONE]`.
    const FRAMES: u32 = 31;
    for row in 0..3 {
        let mut started_at = None;
        let mut done_at = None;
        let mut batches = Vec::new();
        for (at, event) in events.iter().enumerate() {
            match event {
                AiCompareEvent::RowStarted { index, .. } if *index == row => started_at = Some(at),
                AiCompareEvent::RowFrames { index, items } if *index == row => {
                    assert!(done_at.is_none(), "row {row}: frames after RowDone");
                    batches.push((at, items.clone()));
                }
                AiCompareEvent::RowDone { index, .. } if *index == row => done_at = Some(at),
                _ => {}
            }
        }
        let (started_at, done_at) = (started_at.unwrap(), done_at.unwrap());
        assert!(started_at < batches[0].0 && batches.last().unwrap().0 < done_at);
        // Nothing dropped: the batches hold every frame in order, the tail included.
        let items: Vec<_> = batches.iter().flat_map(|(_, items)| items).collect();
        let indexes: Vec<u32> = items.iter().map(|frame| frame.index).collect();
        assert_eq!(indexes, (1..=FRAMES).collect::<Vec<_>>(), "row {row}");
        assert_eq!(items[30].data, "[DONE]");
        // The delay is a lower bound of the time to first byte; offsets never go back.
        assert!(items[0].t_ms >= DELAY_MS, "{}", items[0].t_ms);
        assert!(items.windows(2).all(|pair| pair[1].t_ms >= pair[0].t_ms));
        // One event per window at most, plus the opening one and the final flush.
        let allowed = 2 + (elapsed.as_millis() / 33) as usize;
        assert!(batches.len() <= allowed, "{} in {elapsed:?}", batches.len());
    }
    assert!(
        result
            .rows
            .iter()
            .all(|row| row.status == AiRunStatus::Done)
    );
}

#[tokio::test]
async fn a_guarded_environment_refuses_the_whole_run_and_a_confirmation_covers_it() {
    let a = upstream(Protocol::OpenaiChat, pong()).await;
    let fx = Fx::new();
    let pa = fx.chat_provider("Alpha", a.port);
    base_request(&fx, &pa);
    let mut environment = crate::workspace::Environment::new("env_prod", WS, "Production").unwrap();
    environment.confirm_before_send = true;
    fx.state.store().save_environment(&environment).unwrap();
    let mut guarded = input(vec![entry(&pa, "m-one"), entry(&pa, "m-two")], 3);
    guarded.environment_id = Some("env_prod".to_owned());
    assert!(matches!(
        compare(&fx, guarded.clone(), None).await,
        Err(AppError::ConfirmationRequired { .. })
    ));
    assert_eq!(a.count(), 0);
    guarded.confirmed = Some(true);
    assert_eq!(compare(&fx, guarded, None).await.unwrap().rows.len(), 2);
    assert_eq!(a.count(), 2);
}

fn done_row(index: usize, ttft: u64, micro: Option<i64>, currency: &str) -> AiCompareRow {
    let mut dto = fake_result();
    dto.ttft_ms = Some(ttft);
    dto.cost = micro.map(|micro| Cost {
        micro,
        currency: currency.to_owned(),
        source: PriceSource::Override,
        estimated: false,
    });
    row_of(index, &entry("p", &format!("m{index}")), "P", dto)
}

fn fake_result() -> AiResultDto {
    serde_json::from_value(json!({
        "run_id": "run_1", "status": "done", "http_status": 200, "message": null, "notice": null,
        "stream_error": null, "model": "m", "response_model": null, "protocol": "openai_chat",
        "streamed": true, "reply": "pong", "reasoning": "", "tool_calls": [], "vectors": [],
        "finish_reason": "stop",
        "usage": {"input_tokens": 9, "output_tokens": 2, "cached_tokens": null,
                  "cache_write_tokens": null, "reasoning_tokens": null, "total_tokens": 11, "estimated": false},
        "ttft_ms": 233, "total_ms": 612, "stream_ms": 100, "tokens_per_second": null,
        "timing": {"queue_ms": 1, "attempts": []}, "cost": null, "price": null, "cost_note": null,
        "dropped_headers": [], "response_headers": [],
        "request": {"method": "POST", "url": "u", "headers": [], "body": "{}", "body_bytes": 2, "body_truncated": false},
        "frames": {"frames": [], "total_frames": 0, "truncated": false}, "response_body": null,
        "event_count": 0, "kept_in_log": true, "log_row_id": null
    }))
    .unwrap()
}

#[test]
fn fastest_and_cheapest_are_tagged_only_among_two_or_more_done_rows() {
    let mut rows = vec![
        done_row(0, 233, Some(20), "USD"),
        done_row(1, 520, Some(5), "USD"),
        done_row(2, 38, Some(90), "USD"),
    ];
    tag_best(&mut rows);
    assert_eq!(
        rows.iter().map(|r| r.fastest).collect::<Vec<_>>(),
        [false, false, true]
    );
    assert_eq!(
        rows.iter().map(|r| r.cheapest).collect::<Vec<_>>(),
        [false, true, false]
    );

    let mut alone = vec![
        done_row(0, 10, Some(1), "USD"),
        idle_row(
            1,
            &entry("p", "x"),
            "P",
            AiRunStatus::Skipped,
            "Skipped".to_owned(),
        ),
    ];
    tag_best(&mut alone);
    assert!(alone.iter().all(|r| !r.fastest && !r.cheapest));

    let mut mixed = vec![
        done_row(0, 10, Some(1), "USD"),
        done_row(1, 20, Some(2), "CNY"),
    ];
    tag_best(&mut mixed);
    assert!(mixed[0].fastest);
    assert!(
        mixed.iter().all(|r| !r.cheapest),
        "costs in different currencies are not compared"
    );
}

fn result_with(rows: Vec<AiCompareRow>) -> AiCompareResultDto {
    AiCompareResultDto {
        run_id: "run_9".to_owned(),
        protocol: Protocol::OpenaiChat,
        request_body: r#"{"model":"m","messages":[]}"#.to_owned(),
        rows,
    }
}

fn row_saying(index: usize, reply: &str, cost: Option<(i64, &str)>) -> AiCompareRow {
    let mut dto = fake_result();
    dto.reply = reply.to_owned();
    dto.cost = cost.map(|(micro, currency)| Cost {
        micro,
        currency: currency.to_owned(),
        source: PriceSource::Catalog,
        estimated: true,
    });
    row_of(
        index,
        &entry("p", &format!("model-{index}")),
        "Provider, Inc.",
        dto,
    )
}

#[test]
fn the_json_export_holds_the_request_once_and_one_object_per_model() {
    let result = result_with(vec![
        row_saying(0, "pong", Some((20, "CNY"))),
        row_saying(1, "pang", None),
    ]);
    let file = ai_compare_export_impl(&result, AiCompareExportFormat::Json).unwrap();
    assert!(file.file_name.ends_with(".json"));
    let json: serde_json::Value = serde_json::from_slice(&file.bytes).unwrap();
    assert_eq!(json["request"]["model"], "m");
    let models = json["models"].as_array().unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0]["model"], "model-0");
    assert_eq!(models[0]["status"], "Done");
    assert_eq!(models[0]["reply"], "pong");
    assert_eq!(models[0]["ttft_ms"], 233);
    assert_eq!(models[0]["total_ms"], 612);
    assert_eq!(models[0]["tokens_in"], 9);
    assert_eq!(models[0]["cost"], "0.000020");
    assert_eq!(models[0]["currency"], "CNY");
    assert!(models[1]["cost"].is_null());
}

#[test]
fn the_csv_export_has_a_header_one_row_per_model_and_rfc_4180_quoting() {
    let long = "x".repeat(2500);
    let result = result_with(vec![
        row_saying(0, "plain", Some((20, "USD"))),
        row_saying(1, "line one, \"quoted\"\nline two", None),
        row_saying(2, &long, None),
        row_saying(3, "=SUM(A1)", None),
    ]);
    let file = ai_compare_export_impl(&result, AiCompareExportFormat::Csv).unwrap();
    assert!(file.file_name.ends_with(".csv"));
    let text = String::from_utf8(file.bytes).unwrap();
    assert!(text.starts_with(
        "Model,Provider,Status,TTFT ms,Total ms,tok/s,Tokens in,Tokens out,Cost,Currency,Reply\r\n"
    ));
    assert!(text.ends_with("\r\n"));
    assert!(text.contains("model-0,\"Provider, Inc.\",Done,233,612,,9,2,0.000020,USD,plain\r\n"));
    assert!(text.contains("\"line one, \"\"quoted\"\"\nline two\"\r\n"));
    let long_row = text
        .lines()
        .find(|line| line.starts_with("model-2"))
        .unwrap();
    assert_eq!(
        long_row.matches('x').count(),
        2000,
        "the reply is cut at 2,000 characters"
    );
    assert!(
        text.contains(",'=SUM(A1)\r\n"),
        "a formula is not evaluated by a spreadsheet"
    );
}

fn override_price(provider: &str, model: &str) -> PriceOverride {
    PriceOverride {
        provider_id: provider.to_owned(),
        model_id: model.to_owned(),
        input_per_mtok: 1.0,
        output_per_mtok: 2.0,
        cache_read_per_mtok: None,
        cache_write_per_mtok: None,
        reasoning_per_mtok: None,
        currency: "USD".to_owned(),
        updated_at_ms: 0,
    }
}

#[tokio::test]
async fn the_estimate_is_the_input_estimate_plus_max_tokens_at_the_output_price() {
    let fx = Fx::new();
    let pa = fx.chat_provider("Alpha", 9);
    base_request(&fx, &pa);
    fx.save_price(override_price(&pa, "priced"));
    let estimate = ai_compare_estimate_impl(
        &fx.state,
        &input(vec![entry(&pa, "priced"), entry(&pa, "unpriced")], 3),
    )
    .unwrap();
    assert_eq!(estimate.max_tokens, Some(64));
    let expected_micro = i64::try_from(estimate.input_tokens).unwrap() + 64 * 2;
    assert_eq!(estimate.at_most, [("USD".to_owned(), expected_micro)]);
    assert_eq!(estimate.unpriced_models, 1);

    // Without max tokens there is no cap and no figure.
    fx.provider_request(
        "r1",
        &pa,
        Protocol::OpenaiChat,
        "m",
        r#"{"model":"m","messages":[]}"#,
    );
    let estimate =
        ai_compare_estimate_impl(&fx.state, &input(vec![entry(&pa, "priced")], 3)).unwrap();
    assert_eq!(estimate.max_tokens, None);
    assert!(estimate.at_most.is_empty());
}

#[tokio::test]
async fn detect_runs_the_sniffer_on_the_typed_url_and_creates_no_provider() {
    let models = Fake::start(|_, request| {
        if request.method == "GET" && request.path.ends_with("/models") {
            Reply::Json(
                200,
                json!({"object": "list", "data": [{"id": "gpt-x", "object": "model"}]}),
            )
        } else {
            Reply::Text(404, "text/plain", "not found".to_owned())
        }
    })
    .await;
    let fx = Fx::new();
    let report = ai_detect_impl(
        &fx.state,
        AiDetectInput {
            url: format!("http://127.0.0.1:{}/v1", models.port),
            key: Some("sk-typed-key-0123456789abcdef".to_owned()),
            workspace_id: None,
            environment_id: None,
        },
        None,
        &NoEvents,
    )
    .await
    .unwrap();
    assert!(
        report.models.iter().any(|model| model.id == "gpt-x"),
        "{:?}",
        report.models
    );
    assert!(
        report.requests_used <= 12,
        "{} requests with a key",
        report.requests_used
    );
    assert!(
        fx.state
            .with_llm_connection(crate::llm::store::list_providers)
            .unwrap()
            .is_empty()
    );
    assert!(
        fx.state
            .with_llm_connection(|connection| crate::llm::store::list_keys(connection, None))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn detect_resolves_variables_of_the_key_without_showing_it() {
    let models = Fake::always(Reply::Text(404, "text/plain", "nope".to_owned())).await;
    let fx = Fx::new();
    let mut environment = crate::workspace::Environment::new("env_1", WS, "Local").unwrap();
    environment.variables = vec![
        crate::workspace::EnvironmentVariable::plain("port", models.port.to_string()).unwrap(),
        crate::workspace::EnvironmentVariable::masked("key", "sk-detect-secret-0123456789")
            .unwrap(),
    ];
    fx.state.store().save_environment(&environment).unwrap();
    let input = AiDetectInput {
        url: "http://127.0.0.1:{{port}}".to_owned(),
        key: Some("{{key}}".to_owned()),
        workspace_id: Some(WS.to_owned()),
        environment_id: Some("env_1".to_owned()),
    };
    assert!(!format!("{input:?}").contains("sk-detect"));
    let _ = ai_detect_impl(&fx.state, input, None, &NoEvents).await;
    assert!(models.count() >= 1);
    let seen_key = models.requests().iter().any(|request| {
        request
            .headers
            .values()
            .any(|value| value.contains("sk-detect-secret-0123456789"))
    });
    assert!(seen_key, "the resolved key reaches the scanned origin");
}
