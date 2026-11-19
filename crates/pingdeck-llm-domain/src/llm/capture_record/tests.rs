//! The timing model: the `PhaseValue` wire form and the timing arithmetic over the PRD bible
//! rows.

use serde_json::json;

use super::{AttemptTiming, PhaseValue, TimingJson};

fn https_attempt(connect: u64, headers: u64, first_token: PhaseValue, end: u64) -> AttemptTiming {
    AttemptTiming {
        dns: PhaseValue::NotNeeded,
        connect: PhaseValue::Ms(connect),
        tls: PhaseValue::NotMeasured,
        connect_includes_tls: true,
        headers_ms: PhaseValue::Ms(headers),
        first_token_ms: first_token,
        end_ms: PhaseValue::Ms(end),
        reused: false,
    }
}

#[test]
fn bible_row_one_a_pooled_stream_adds_up() {
    // TTFT 412 = queue 12 + headers 233 + first token 167; total 880.
    let timing = TimingJson {
        queue_ms: 12,
        attempts: vec![AttemptTiming::on_reused_connection(
            233,
            PhaseValue::Ms(167),
            868,
        )],
    };
    assert!(timing.check_total(880));
    assert!(timing.check_total(881), "within 1 ms");
    assert!(!timing.check_total(883));
    assert_eq!(timing.attempts[0].receive_ms(), Some(468));
}

#[test]
fn a_new_https_connection_row_adds_up() {
    // queue 18 + connect 18 + wait 233 + receive 326 = 595.
    let timing = TimingJson {
        queue_ms: 18,
        attempts: vec![https_attempt(18, 233, PhaseValue::NotNeeded, 577)],
    };
    assert!(timing.check_total(595));
    assert_eq!(timing.attempts[0].setup_ms(), 18);
    assert_eq!(timing.attempts[0].receive_ms(), Some(326));
}

#[test]
fn a_failover_row_counts_the_failed_attempt_after_the_queue() {
    let failed = https_attempt(20, 100, PhaseValue::NotNeeded, 150);
    let served = AttemptTiming::on_reused_connection(200, PhaseValue::Ms(90), 480);
    let timing = TimingJson {
        queue_ms: 20,
        attempts: vec![failed, served],
    };
    assert!(timing.check_total(650));
    assert!(!timing.check_total(500));
}

#[test]
fn unmeasured_or_inconsistent_timings_fail_the_check() {
    assert!(!TimingJson::default().check_total(0), "no attempt");
    let mut unmeasured = https_attempt(18, 233, PhaseValue::NotNeeded, 577);
    unmeasured.end_ms = PhaseValue::NotMeasured;
    let timing = TimingJson {
        queue_ms: 18,
        attempts: vec![unmeasured],
    };
    assert!(!timing.check_total(595));
    // Phases longer than the attempt: the receive phase would be negative.
    let impossible = TimingJson {
        queue_ms: 0,
        attempts: vec![AttemptTiming::on_reused_connection(
            300,
            PhaseValue::NotNeeded,
            200,
        )],
    };
    assert!(!impossible.check_total(200));
    assert_eq!(impossible.attempts[0].receive_ms(), None);
}

#[test]
fn reused_and_unmeasured_phases_add_nothing_to_setup() {
    let attempt = AttemptTiming::on_reused_connection(10, PhaseValue::NotNeeded, 50);
    assert_eq!(attempt.setup_ms(), 0);
    let mut mixed = https_attempt(18, 10, PhaseValue::NotNeeded, 50);
    mixed.dns = PhaseValue::Ms(4);
    assert_eq!(mixed.setup_ms(), 22);
    assert!(mixed.connect_includes_tls);
    assert_eq!(mixed.tls, PhaseValue::NotMeasured);
}

#[test]
fn phase_value_wire_form_is_a_number_or_a_word() {
    let attempt = AttemptTiming::on_reused_connection(233, PhaseValue::NotMeasured, 612);
    let value = serde_json::to_value(&attempt).unwrap();
    assert_eq!(value["dns_ms"], "reused");
    assert_eq!(value["connect_ms"], "reused");
    assert_eq!(value["tls_ms"], "reused");
    assert_eq!(value["headers_ms"], 233);
    assert_eq!(value["first_token_ms"], "not_measured");
    assert_eq!(value["reused"], true);
    assert_eq!(
        serde_json::from_value::<AttemptTiming>(value).unwrap(),
        attempt
    );
}

#[test]
fn unknown_is_never_zero() {
    assert_eq!(PhaseValue::default(), PhaseValue::NotMeasured);
    assert_eq!(PhaseValue::NotMeasured.ms(), None);
    assert_eq!(PhaseValue::Reused.ms(), None);
    assert_eq!(PhaseValue::Ms(0).ms(), Some(0));
    assert_eq!(
        serde_json::to_value(PhaseValue::NotNeeded).unwrap(),
        json!("not_needed")
    );
}

#[test]
fn timing_json_round_trips_with_a_new_https_attempt() {
    let timing = TimingJson {
        queue_ms: 12,
        attempts: vec![AttemptTiming {
            dns: PhaseValue::NotNeeded,
            connect: PhaseValue::Ms(49),
            tls: PhaseValue::NotMeasured,
            connect_includes_tls: true,
            headers_ms: PhaseValue::Ms(233),
            first_token_ms: PhaseValue::Ms(150),
            end_ms: PhaseValue::Ms(900),
            reused: false,
        }],
    };
    let text = serde_json::to_string(&timing).unwrap();
    assert!(text.contains("\"connect_includes_tls\":true"));
    assert_eq!(serde_json::from_str::<TimingJson>(&text).unwrap(), timing);
}
