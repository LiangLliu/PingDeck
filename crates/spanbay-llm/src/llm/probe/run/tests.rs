//! Tests of the run orchestrator against fake upstreams on 127.0.0.1.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::llm::probe::listing::ModelKind;
use crate::llm::probe::plan::{CheckSelection, CheckSet, PlanItem, plan_run};
use crate::llm::probe::test_support::{
    Fake, InFlight, OPENAI, Reply, Seen, oa_text, openai_fake, opts, runtime, spawn_counted,
    spawn_fake, target,
};
use crate::llm::wire::Protocol;

#[derive(Default)]
struct Recorder {
    cells: Mutex<Vec<CellRecord>>,
    requests: Mutex<Vec<RequestRecord>>,
    fail: bool,
}

impl Recorder {
    fn cells(&self) -> Vec<CellRecord> {
        self.cells.lock().unwrap().clone()
    }

    fn requests(&self) -> Vec<RequestRecord> {
        self.requests.lock().unwrap().clone()
    }

    fn cell(&self, test: ProbeTest) -> CellRecord {
        self.cells()
            .into_iter()
            .find(|cell| cell.test == test)
            .unwrap_or_else(|| panic!("no cell for {test:?}"))
    }
}

impl ProbePersist for Recorder {
    fn save_cell<'a>(
        &'a self,
        _run_id: &'a str,
        cell: &'a CellRecord,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            self.cells.lock().unwrap().push(cell.clone());
            if self.fail {
                Err(AppError::validation("disk full"))
            } else {
                Ok(())
            }
        })
    }

    fn record_request<'a>(
        &'a self,
        _run_id: &'a str,
        entry: &'a RequestRecord,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(entry.clone());
            if self.fail {
                Err(AppError::validation("disk full"))
            } else {
                Ok(())
            }
        })
    }
}

fn row(
    fake: &Fake,
    provider: &str,
    key: &str,
    model: &str,
    protocols: &[Protocol],
) -> (RunItem, PlanItem) {
    let mut target = target(fake.addr, protocols);
    target.provider_id = provider.to_owned();
    target.key_id = key.to_owned();
    target.model = model.to_owned();
    (
        RunItem {
            target,
            options: opts(),
        },
        PlanItem {
            provider_id: provider.to_owned(),
            key_id: key.to_owned(),
            model_id: model.to_owned(),
            kind: ModelKind::Chat,
            protocols: protocols.to_vec(),
        },
    )
}

struct Outcome {
    summary: RunSummary,
    recorder: Recorder,
    events: Vec<ProbeRunEvent>,
}

async fn go(
    rows: Vec<(RunItem, PlanItem)>,
    selection: &CheckSelection,
    concurrency: u8,
    recorder: Recorder,
    cancel: &CancelToken,
) -> Outcome {
    let (items, plan_items): (Vec<_>, Vec<_>) = rows.into_iter().unzip();
    let plan = plan_run(&plan_items, selection);
    let events = Mutex::new(Vec::new());
    let sink = |event: ProbeRunEvent| {
        events.lock().unwrap().push(event);
        true
    };
    let runtime = runtime();
    let summary = run_plan(&ProbeRun {
        run_id: "run-1",
        runtime: &runtime,
        items: &items,
        plan: &plan,
        settings: RunSettings::new(concurrency, 30),
        persist: &recorder,
        sink: &sink,
        cancel,
    })
    .await
    .unwrap();
    Outcome {
        summary,
        recorder,
        events: events.into_inner().unwrap(),
    }
}

fn standard() -> CheckSelection {
    CheckSelection::Set {
        set: CheckSet::Standard,
    }
}

fn custom(tests: &[ProbeTest]) -> CheckSelection {
    CheckSelection::Custom {
        tests: tests.to_vec(),
    }
}

fn slow_chat(seen: &Seen) -> Reply {
    if seen.method == "GET" {
        Reply::json(200, &json!({"data": [{"id": "m"}]}))
    } else {
        Reply::After(60, Box::new(oa_text("pong")))
    }
}

#[tokio::test]
async fn standard_run_logs_requests_and_redacts_secrets_while_counting_persist_errors() {
    let fake = spawn_fake(openai_fake).await;
    let out = go(
        vec![row(&fake, "p", "k1", "m", OPENAI)],
        &standard(),
        3,
        Recorder {
            fail: true,
            ..Recorder::default()
        },
        &CancelToken::new(),
    )
    .await;
    assert_eq!(
        (out.summary.total, out.summary.done, out.summary.ready),
        (8, 8, 8)
    );
    assert_eq!(out.events.len(), 8);
    assert_eq!(out.recorder.requests().len(), 9);
    assert_eq!(out.summary.persist_errors, 8 + 9);
    assert_eq!(
        out.recorder
            .cell(ProbeTest::Vision)
            .capability
            .unwrap()
            .measurement,
        crate::llm::capabilities::Measurement::Yes
    );
    let stored = format!("{:?}{:?}", out.recorder.cells(), out.recorder.requests());
    assert!(!stored.contains("sk-test"));
}

#[tokio::test]
async fn dependency_skips_on_dead_host_rejected_key_failed_chat_and_model_kinds() {
    // Dead host skips everything after Connectivity.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let dead = Fake {
        addr,
        ..spawn_fake(|_| Reply::Hang).await
    };
    let out = go(
        vec![row(&dead, "p", "k1", "m", OPENAI)],
        &standard(),
        3,
        Recorder::default(),
        &CancelToken::new(),
    )
    .await;
    assert_eq!(
        out.recorder.cell(ProbeTest::Connectivity).status,
        ProbeStatus::Unreachable
    );
    assert_eq!(out.summary.skipped, 7);

    // Rejected key skips everything after Auth.
    let bad_key = spawn_fake(|seen: &Seen| {
        if seen.path.starts_with("/models") {
            Reply::Json(
                401,
                r#"{"error":{"message":"Incorrect API key"}}"#.to_owned(),
            )
        } else {
            Reply::Json(404, "{}".to_owned())
        }
    })
    .await;
    let out = go(
        vec![row(&bad_key, "p", "k1", "m", OPENAI)],
        &standard(),
        3,
        Recorder::default(),
        &CancelToken::new(),
    )
    .await;
    assert_eq!(
        out.recorder.cell(ProbeTest::Auth).status,
        ProbeStatus::Rejected
    );
    assert_eq!(out.summary.skipped, 6);

    // Failed chat skips dependent cells for that model only; embedding/image models skip chat cells.
    let mixed = spawn_fake(|seen: &Seen| {
        if seen.method == "GET" {
            Reply::json(200, &json!({"data": [{"id": "m"}]}))
        } else if seen.body.contains("\"model\":\"bad\"") {
            Reply::Json(403, r#"{"error":{"message":"model not enabled"}}"#.to_owned())
        } else if seen.path.ends_with("/embeddings") {
            Reply::json(
                200,
                &json!({"data": [{"embedding": [0.1, 0.2, 0.3], "index": 0}], "usage": {"prompt_tokens": 5}}),
            )
        } else {
            oa_text("pong")
        }
    })
    .await;
    let (emb_item, mut emb_plan) = row(&mixed, "p", "k1", "e", &[Protocol::Embeddings]);
    emb_plan.kind = ModelKind::Embedding;
    let out = go(
        vec![
            row(&mixed, "p", "k1", "bad", OPENAI),
            row(&mixed, "p", "k1", "m", OPENAI),
            (emb_item, emb_plan),
        ],
        &standard(),
        3,
        Recorder::default(),
        &CancelToken::new(),
    )
    .await;
    assert_eq!(
        out.recorder
            .cell(ProbeTest::Embeddings)
            .capability
            .unwrap()
            .dimension,
        Some(3)
    );
    assert!(out.recorder.cells().iter().any(|c| c.model_id == "bad"
        && c.test == ProbeTest::Stream
        && c.skip == Some(SkipReason::ChatFailed)));
}

#[tokio::test]
async fn concurrency_limits_rate_limit_exclusivity_and_cancellation() {
    // Per-provider limit (2) and global limit (3) across providers.
    let single = spawn_fake(slow_chat).await;
    let single_rows = (0..4)
        .map(|n| row(&single, "p", "k1", &format!("m{n}"), OPENAI))
        .collect();
    let out = go(
        single_rows,
        &custom(&[ProbeTest::Chat]),
        8,
        Recorder::default(),
        &CancelToken::new(),
    )
    .await;
    assert_eq!((out.summary.ready, single.inflight.max()), (4, 2));

    let shared = Arc::new(InFlight::default());
    let mut rows = Vec::new();
    for provider in ["a", "b", "c"] {
        let fake = spawn_counted(slow_chat, Arc::clone(&shared)).await;
        for n in 0..2 {
            rows.push(row(&fake, provider, provider, &format!("m{n}"), OPENAI));
        }
    }
    let out = go(
        rows,
        &custom(&[ProbeTest::Chat]),
        3,
        Recorder::default(),
        &CancelToken::new(),
    )
    .await;
    assert_eq!((out.summary.ready, shared.max()), (6, 3));

    // A 429 throttles the key to 1 in-flight request and leaves dependent cells NotTested.
    let first_429 = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&first_429);
    let limited_fake = spawn_fake(move |seen: &Seen| {
        if seen.method == "GET" {
            Reply::json(200, &json!({"data": []}))
        } else if flag.swap(true, Ordering::SeqCst) {
            Reply::After(50, Box::new(oa_text("pong")))
        } else {
            Reply::RetryAfter(429, 30, r#"{"error":{"message":"slow down"}}"#.to_owned())
        }
    })
    .await;
    let out = go(
        (0..4)
            .map(|n| row(&limited_fake, "p", "k1", &format!("m{n}"), OPENAI))
            .collect(),
        &custom(&[ProbeTest::Chat, ProbeTest::Stream]),
        8,
        Recorder::default(),
        &CancelToken::new(),
    )
    .await;
    assert_eq!(out.summary.rate_limited, 1);
    assert_eq!(out.summary.not_tested, 1);
    let arrivals = limited_fake.inflight_at_arrival.lock().unwrap().clone();
    assert!(arrivals[2..].iter().all(|inflight| *inflight == 1));

    // Stop marks the in-flight check as Stopped and leaves later checks NotTested.
    let hang = spawn_fake(|_| Reply::Hang).await;
    let cancel = CancelToken::new();
    let stopper = async {
        hang.arrived.notified().await;
        cancel.cancel();
    };
    let sel = custom(&[ProbeTest::Chat, ProbeTest::Stream]);
    let (out, ()) = tokio::join!(
        async {
            tokio::time::timeout(
                Duration::from_secs(10),
                go(
                    vec![row(&hang, "p", "k1", "m", OPENAI)],
                    &sel,
                    3,
                    Recorder::default(),
                    &cancel,
                ),
            )
            .await
            .unwrap()
        },
        stopper
    );
    assert!(out.summary.cancelled);
    assert_eq!(
        out.recorder.cell(ProbeTest::Chat).skip,
        Some(SkipReason::Stopped)
    );
    assert_eq!(out.recorder.cells().len(), 1);
}

#[tokio::test]
async fn throttle_window_settings_clamping_interrupted_summary_and_missing_row_validation() {
    let throttle = KeyThrottle::new();
    assert!(!throttle.limited(0));
    throttle.mark(1_000);
    assert!(throttle.limited(60_999) && !throttle.limited(61_000));

    assert_eq!(RunSettings::new(0, 30).concurrency, 1);
    assert_eq!(RunSettings::new(200, 30).concurrency, 16);

    let item = PlanItem {
        provider_id: "p".to_owned(),
        key_id: "k".to_owned(),
        model_id: "m0".to_owned(),
        kind: ModelKind::Chat,
        protocols: vec![Protocol::OpenaiChat],
    };
    let plan = plan_run(std::slice::from_ref(&item), &standard());
    let summary = mark_interrupted(&plan, &[]);
    assert!(summary.cancelled && summary.interrupted);
    assert_eq!(summary.note(), Some(interrupted_note()));
    assert_eq!(RunSummary::default().note(), None);

    let fake = spawn_fake(openai_fake).await;
    let (run_item, plan_item) = row(&fake, "p", "k1", "m", OPENAI);
    let bad_plan = plan_run(&[plan_item.clone(), plan_item], &standard());
    let err = run_plan(&ProbeRun {
        run_id: "r",
        runtime: &runtime(),
        items: &[run_item],
        plan: &bad_plan,
        settings: RunSettings::new(3, 30),
        persist: &Recorder::default(),
        sink: &spanbay_model::events::NoEvents,
        cancel: &CancelToken::new(),
    })
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::Validation { .. }));
}
