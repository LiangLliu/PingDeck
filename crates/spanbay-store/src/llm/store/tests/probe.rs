//! Probe persistence: cells, history pruning, runs, the apply log, probe request rows.

use serde_json::json;

use super::{connection, key, model, raw_text, seeded, single_endpoint_provider};
use crate::llm::capabilities::{Capability, CapabilitySource, Measurement, ModelCapabilities};
use crate::llm::failure::FailureKind;
use crate::llm::probe::apply::{
    ApplyReport, Measured, PreviousState, UNDO_WINDOW_MS, plan_capabilities,
};
use crate::llm::probe::capability::CapabilityUpdate;
use crate::llm::probe::plan::SkipReason;
use crate::llm::probe::run::{CellRecord, RequestRecord};
use crate::llm::probe::{ProbeRequest, ProbeStatus, ProbeTest};
use crate::llm::store::probe_runs::{delete_probe_apply, latest_probe_apply};
use crate::llm::store::{
    HISTORY_RUNS_PER_PAIR, LogPage, MAX_APPLY_LOG, ProbeResult, ResolvedKey, count_log, delete_key,
    finish_probe_run, get_model, get_probe_apply, latest_probe_run, latest_undoable_apply,
    list_log, list_probe_history, list_probe_results, list_probe_runs, log_id_for_request_id,
    new_request_id, persist_cell, probe_run_results, record_probe_apply, record_probe_request,
    save_key, save_model, save_probe_result, save_provider, set_model_capabilities_override,
    start_probe_run, swap_capability_states, undo_bulk,
};
use spanbay_base::error::AppError;

fn result(test: ProbeTest, ok: bool, at: i64) -> ProbeResult {
    ProbeResult {
        key_id: "k1".into(),
        model_id: "m".into(),
        endpoint: "openai_chat".into(),
        test: test.as_str().into(),
        ok,
        status: Some(if ok { 200 } else { 403 }),
        latency_ms: Some(120),
        ttft_ms: Some(40),
        message: if ok { "ok" } else { "forbidden" }.into(),
        detail: json!({"n": 1}),
        failure_kind: (!ok).then_some(FailureKind::Auth),
        checked_at_ms: at,
        ..ProbeResult::default()
    }
}

fn cell(test: ProbeTest, ok: bool, at: i64) -> CellRecord {
    CellRecord {
        provider_id: "p1".into(),
        key_id: "k1".into(),
        model_id: "m".into(),
        test,
        status: if ok {
            ProbeStatus::Ready
        } else {
            ProbeStatus::Rejected
        },
        reason: String::new(),
        skip: None,
        result: Some(result(test, ok, at)),
        capability: None,
    }
}

fn setup() -> rusqlite::Connection {
    let c = connection();
    seeded(&c, "p1", "k1");
    save_model(&c, &model("p1", "m")).unwrap();
    c
}

/// Key A of a relay group sees no image support; the model must still route for key B
/// until the user applies the run, and the undo of that apply puts the matrix back.
#[test]
fn a_failed_vision_check_is_evidence_until_applied_and_the_undo_restores_it() {
    let c = setup();
    let mut blue = cell(ProbeTest::Vision, false, 50);
    blue.status = ProbeStatus::Unsupported;
    blue.capability = Some(CapabilityUpdate {
        capability: Capability::Vision,
        measurement: Measurement::No,
        dimension: None,
    });
    persist_cell(&c, "run-a", &blue, 50).unwrap();
    let matrix = |c: &rusqlite::Connection| get_model(c, "p1", "m").unwrap().unwrap().capabilities;
    assert!(!matrix(&c).known_absent(Capability::Vision));

    let measured = [Measured {
        model_id: "m",
        key_id: "k1",
        update: blue.capability.unwrap(),
        at_ms: 50,
        http_status: Some(200),
    }];
    let changes = plan_capabilities(&measured, &[("m".to_owned(), matrix(&c))], &[]);
    assert_eq!(
        swap_capability_states(&c, "p1", &changes, false).unwrap(),
        ["m"]
    );
    assert!(matrix(&c).known_absent(Capability::Vision));
    let report = ApplyReport {
        enabled: Vec::new(),
        disabled: Vec::new(),
        created: Vec::new(),
        previous: Vec::new(),
        capabilities: changes,
    };
    record_probe_apply(&c, "apply-v", "run-a", "p1", &report, 60).unwrap();
    assert_eq!(undo_bulk(&c, "apply-v").unwrap().restored, ["m"]);
    assert!(!matrix(&c).known_absent(Capability::Vision));
}

#[test]
fn cell_persistence_history_cap_and_key_cascade() {
    let c = setup();
    save_probe_result(&c, &result(ProbeTest::Auth, false, 1)).unwrap();
    assert_eq!(
        list_probe_results(&c, Some("k1")).unwrap()[0].status_word,
        "rejected"
    );

    let mut orphan = cell(ProbeTest::Chat, true, 1);
    orphan.key_id = "nope".into();
    orphan.result.as_mut().unwrap().key_id = "nope".into();
    assert!(matches!(
        persist_cell(&c, "run-1", &orphan, 1),
        Err(AppError::Database { .. })
    ));

    let mut matrix = ModelCapabilities::default();
    matrix.apply(Capability::Tools, false, CapabilitySource::UserOverride);
    set_model_capabilities_override(&c, "p1", "m", &matrix).unwrap();

    for (test, cap) in [
        (ProbeTest::Vision, Capability::Vision),
        (ProbeTest::Tools, Capability::Tools),
    ] {
        let mut c_rec = cell(test, true, 1);
        c_rec.capability = Some(CapabilityUpdate {
            capability: cap,
            measurement: Measurement::Yes,
            dimension: None,
        });
        persist_cell(&c, "run-1", &c_rec, 1).unwrap();
    }
    // Probing writes no capability; the override stays.
    let stored = get_model(&c, "p1", "m").unwrap().unwrap();
    assert!(!stored.vision && !stored.capabilities.tools.supported);
    assert_eq!(
        stored.capabilities.tools.source,
        CapabilitySource::UserOverride
    );

    let skipped = CellRecord {
        provider_id: "p1".into(),
        key_id: "k1".into(),
        model_id: "m".into(),
        test: ProbeTest::Vision,
        status: ProbeStatus::Skipped,
        reason: SkipReason::ChatFailed.label().into(),
        skip: Some(SkipReason::ChatFailed),
        result: None,
        capability: None,
    };
    persist_cell(&c, "run-2", &skipped, 777).unwrap();
    let v = list_probe_results(&c, Some("k1"))
        .unwrap()
        .into_iter()
        .find(|r| r.test == "vision")
        .unwrap();
    assert_eq!(
        (v.status_word.as_str(), v.skip_reason.as_deref()),
        ("skipped", Some("chat_failed"))
    );

    persist_cell(&c, "run-3", &cell(ProbeTest::Decision, true, 2), 2).unwrap();
    let mut tests: Vec<String> = list_probe_results(&c, Some("k1"))
        .unwrap()
        .into_iter()
        .map(|r| r.test)
        .collect();
    tests.sort();
    assert_eq!(tests, ["auth", "decision"]);

    // History keeps 25 runs per pair and cascades on key delete.
    for run in 0..=HISTORY_RUNS_PER_PAIR {
        for test in [ProbeTest::Chat, ProbeTest::Tools] {
            persist_cell(
                &c,
                &format!("run-{run:02}"),
                &cell(test, true, run as i64),
                0,
            )
            .unwrap();
        }
    }
    let rows = list_probe_history(&c, "k1", "m", 1_000).unwrap();
    assert_eq!(rows.len(), HISTORY_RUNS_PER_PAIR * 2);
    assert!(!rows.iter().any(|r| r.run_id.as_deref() == Some("run-00")));
    assert_eq!(probe_run_results(&c, "run-25").unwrap().len(), 2);

    delete_key(&c, "k1").unwrap();
    assert!(list_probe_history(&c, "k1", "m", 10).unwrap().is_empty());
}

#[test]
fn a_run_starts_finishes_and_lists_newest_first() {
    let c = setup();
    assert!(matches!(
        finish_probe_run(&c, "nope", 0, 0, &json!({}), 1),
        Err(AppError::NotFound { .. })
    ));
    start_probe_run(&c, "run-1", Some("p1"), "standard", None, 100).unwrap();
    start_probe_run(&c, "run-2", None, "quick", Some("run-1"), 200).unwrap();
    finish_probe_run(&c, "run-1", 12, 38, &json!({"failed": 2}), 500).unwrap();
    assert_eq!(latest_probe_run(&c).unwrap().unwrap().run_id, "run-2");
    assert_eq!(list_probe_runs(&c, Some("p1"), 10).unwrap()[0].passed, 12);
}

#[test]
fn the_apply_log_is_capped_undoable_for_a_day_and_per_provider() {
    let c = setup();
    let report = ApplyReport {
        enabled: Vec::new(),
        disabled: vec!["a".into()],
        created: Vec::new(),
        previous: vec![PreviousState {
            model_id: "a".into(),
            previous_enabled: true,
            applied_enabled: false,
        }],
        capabilities: Vec::new(),
    };
    for n in 0..(MAX_APPLY_LOG + 2) {
        record_probe_apply(
            &c,
            &format!("apply-{n}"),
            "run-1",
            "p1",
            &report,
            (n as i64 + 1) * 1_000,
        )
        .unwrap();
    }
    assert!(get_probe_apply(&c, "apply-1").unwrap().is_none());
    assert_eq!(latest_probe_apply(&c).unwrap().unwrap().apply_id, "apply-6");
    assert!(
        latest_undoable_apply(&c, 7_000 + UNDO_WINDOW_MS - 1, None)
            .unwrap()
            .is_some()
    );
    assert!(
        latest_undoable_apply(&c, 7_000 + UNDO_WINDOW_MS, None)
            .unwrap()
            .is_none()
    );
    // Undo is per provider: a newer apply on p2 and a bulk switch on p1 do not hide p1's apply.
    record_probe_apply(&c, "apply-p2", "run-9", "p2", &report, 8_000).unwrap();
    record_probe_apply(&c, "bulk_x", "bulk:bulk_x", "p1", &report, 8_500).unwrap();
    let mine = |provider| {
        latest_undoable_apply(&c, 9_000, Some(provider))
            .unwrap()
            .map(|record| record.apply_id)
    };
    assert_eq!(
        (mine("p1"), mine("p2")),
        (Some("apply-6".to_owned()), Some("apply-p2".to_owned()))
    );
    delete_probe_apply(&c, "apply-6").unwrap();
    assert_eq!(get_probe_apply(&c, "apply-6").unwrap(), None);
}

#[test]
fn probe_requests_are_logged_linked_and_rolled_up_as_probe() {
    let c = setup();
    let req = |status: Option<u16>, kind: Option<FailureKind>| RequestRecord {
        provider_id: "p1".into(),
        key_id: "k1".into(),
        model_id: "m".into(),
        request: ProbeRequest {
            endpoint: "openai_chat".into(),
            started_ms: 1_700_000_000_000,
            latency_ms: 321,
            ttft_ms: Some(120),
            status,
            input_tokens: Some(11),
            output_tokens: Some(22),
            cached_input_tokens: Some(3),
            failure_kind: kind,
            error: kind.map(|_| "boom".to_owned()),
            stream: true,
            request_id: new_request_id(),
        },
    };
    let ok_rec = req(Some(200), None);
    let req_id = ok_rec.request.request_id.clone();
    let row_id = record_probe_request(&c, "run-9", &ok_rec, 0).unwrap();
    assert_eq!(log_id_for_request_id(&c, &req_id).unwrap(), Some(row_id));

    let mut chat = cell(ProbeTest::Chat, true, 10);
    chat.result.as_mut().unwrap().request_id = Some(req_id.clone());
    persist_cell(&c, "run-9", &chat, 10).unwrap();
    let stored = &list_probe_results(&c, None).unwrap()[0];
    assert_eq!(
        (stored.request_id.as_deref(), &stored.detail),
        (Some(req_id.as_str()), &json!({"n": 1}))
    );

    record_probe_request(&c, "run-9", &req(Some(403), Some(FailureKind::Auth)), 0).unwrap();
    record_probe_request(&c, "run-9", &req(None, Some(FailureKind::Cancelled)), 0).unwrap();
    let rows = list_log(&c, &LogPage::default()).unwrap();
    assert_eq!(count_log(&c, &Default::default()).unwrap(), 3);
    assert_eq!(
        (
            rows[0].outcome.as_str(),
            rows[1].outcome.as_str(),
            rows[2].source.as_str()
        ),
        ("canceled", "failure", "probe")
    );
    assert_eq!(
        raw_text(
            &c,
            "SELECT group_concat(DISTINCT source) FROM llm_usage_daily"
        ),
        "probe"
    );
}

// A probe of a priced model is booked like a gateway row: the price's currency, a
// cost above zero, on the local day of the offset passed in.
#[test]
fn a_priced_probe_is_booked_on_the_local_day() {
    use crate::llm::store::save_price_override;
    use crate::llm::usage::PriceOverride;
    let c = connection();
    seeded(&c, "p1", "k1");
    save_price_override(
        &c,
        PriceOverride {
            provider_id: "p1".into(),
            model_id: "m".into(),
            input_per_mtok: 2.0,
            output_per_mtok: 8.0,
            cache_read_per_mtok: None,
            cache_write_per_mtok: None,
            reasoning_per_mtok: None,
            currency: "CNY".into(),
            updated_at_ms: 0,
        },
        1,
    )
    .unwrap();
    // 2026-11-07 20:00 UTC is 2026-11-08 in UTC+8.
    let started_ms = 1_794_081_600_000;
    let record = RequestRecord {
        provider_id: "p1".into(),
        key_id: "k1".into(),
        model_id: "m".into(),
        request: ProbeRequest {
            endpoint: "openai_chat".into(),
            started_ms,
            latency_ms: 100,
            ttft_ms: None,
            status: Some(200),
            input_tokens: Some(1_000),
            output_tokens: Some(500),
            cached_input_tokens: None,
            failure_kind: None,
            error: None,
            stream: false,
            request_id: new_request_id(),
        },
    };
    record_probe_request(&c, "run-1", &record, 480).unwrap();
    let row = &list_log(&c, &LogPage::default()).unwrap()[0];
    assert_eq!(row.cost_currency, "CNY");
    assert!(
        row.cost_micro_usd.unwrap_or(0) > 0,
        "the probe row has a cost"
    );
    let (day, currency, cost): (String, String, i64) = c
        .query_row(
            "SELECT day, currency, cost_micro_usd FROM llm_usage_daily WHERE source = 'probe'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((day.as_str(), currency.as_str()), ("2026-11-08", "CNY"));
    assert_eq!(cost, row.cost_micro_usd.unwrap_or(0));
    let minute_cost: i64 = c
        .query_row(
            "SELECT SUM(cost_micro_usd) FROM llm_usage_minute WHERE source = 'probe'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(minute_cost, cost);
}

#[test]
fn probe_results_replace_per_test() {
    let c = connection();
    save_provider(&c, &single_endpoint_provider("p1"), 1).unwrap();
    save_key(&c, &key("k1", "p1"), Some("sk-test"), 1).unwrap();
    let mut result = ProbeResult {
        key_id: "k1".into(),
        model_id: "m".into(),
        endpoint: "openai_chat".into(),
        test: "chat".into(),
        ok: false,
        status: Some(401),
        latency_ms: Some(12),
        ttft_ms: None,
        tps: None,
        input_tokens: None,
        output_tokens: None,
        failure_kind: Some(FailureKind::Auth),
        message: "bad key".into(),
        detail: serde_json::json!({"a": 1}),
        checked_at_ms: 5,
        ..Default::default()
    };
    save_probe_result(&c, &result).unwrap();
    result.ok = true;
    result.failure_kind = None;
    result.checked_at_ms = 6;
    save_probe_result(&c, &result).unwrap();
    let stored = list_probe_results(&c, Some("k1")).unwrap();
    // The store derives the status word of a result that carries none.
    result.status_word = "ready".into();
    assert_eq!(stored, vec![result]);
}

#[test]
fn an_embeddings_cell_drops_chat_family_results() {
    let c = setup();
    for test in [ProbeTest::Auth, ProbeTest::Chat, ProbeTest::Tools] {
        persist_cell(&c, "run-1", &cell(test, true, 1), 1).unwrap();
    }
    persist_cell(&c, "run-2", &cell(ProbeTest::Embeddings, true, 2), 2).unwrap();
    let mut tests: Vec<String> = list_probe_results(&c, Some("k1"))
        .unwrap()
        .into_iter()
        .map(|r| r.test)
        .collect();
    tests.sort();
    assert_eq!(tests, ["auth", "embeddings"]);
}

#[test]
fn a_keyless_cell_is_not_persisted() {
    let c = setup();
    let keyless_id = ResolvedKey::keyless("p1").key.id;
    let mut keyless = cell(ProbeTest::Chat, true, 1);
    keyless.key_id.clone_from(&keyless_id);
    keyless.result.as_mut().unwrap().key_id = keyless_id;
    persist_cell(&c, "run-1", &keyless, 1).unwrap();
    assert!(list_probe_results(&c, None).unwrap().is_empty());
    assert_eq!(raw_text(&c, "SELECT COUNT(*) FROM llm_probe_history"), "0");
}
