//! The capture policy truth table.

use crate::llm::capture::{CaptureMode, CaptureSource, LogOutcome, should_capture};

const SOURCES: [CaptureSource; 4] = [
    CaptureSource::Gateway,
    CaptureSource::Agent,
    CaptureSource::Probe,
    CaptureSource::Api,
];
const MODES: [CaptureMode; 3] = [CaptureMode::Off, CaptureMode::ErrorsOnly, CaptureMode::All];

/// A gateway-style call with `keep_api_exchange` on (the option only matters for `api` rows).
fn capture(
    mode: CaptureMode,
    source: CaptureSource,
    status: u16,
    outcome: LogOutcome,
    attempt_failed: bool,
) -> bool {
    should_capture(mode, source, status, outcome, attempt_failed, true)
}

#[test]
fn errors_only_keeps_a_429_and_skips_a_200() {
    let mode = CaptureMode::ErrorsOnly;
    let gateway = CaptureSource::Gateway;
    assert!(!capture(mode, gateway, 200, LogOutcome::Success, false));
    assert!(capture(mode, gateway, 429, LogOutcome::Failure, false));
}

#[test]
fn the_truth_table_matches_the_spec_for_every_combination() {
    for mode in MODES {
        for source in SOURCES {
            for status in [200, 399, 400, 500, 503] {
                for outcome in [
                    LogOutcome::Success,
                    LogOutcome::Failure,
                    LogOutcome::Canceled,
                ] {
                    for attempt_failed in [false, true] {
                        for keep in [false, true] {
                            let failed =
                                status >= 400 || outcome != LogOutcome::Success || attempt_failed;
                            let expected = match (source, mode) {
                                (CaptureSource::Api, _) => keep,
                                (_, CaptureMode::Off) => false,
                                (_, CaptureMode::All) => true,
                                (_, CaptureMode::ErrorsOnly) => failed,
                            };
                            assert_eq!(
                                should_capture(mode, source, status, outcome, attempt_failed, keep),
                                expected,
                                "{mode:?} {source:?} {status} {outcome:?} {attempt_failed} {keep}"
                            );
                        }
                    }
                }
            }
        }
    }
}
