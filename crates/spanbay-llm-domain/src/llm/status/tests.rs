//! Tests of the provider status: rule order, reasons and their numbers, latency.

use super::*;
use ProbeStatus as S;
use ProviderStatus as P;

const NOW: i64 = 1_800_000_000_000;

/// A provider that is healthy: one enabled key with a passing Auth, 38 enabled models that
/// passed, a Ready Chat 5 minutes ago and nothing else going on.
fn healthy() -> StatusInputs {
    StatusInputs {
        provider_id: "relay".to_owned(),
        enabled: true,
        auth_none: false,
        key_auth: vec![Some(S::Ready)],
        newest_connectivity: Some(S::Ready),
        newest_connectivity_ms: None,
        newest_passing_test_ms: Some(NOW - 5 * MINUTE_MS),
        newest_ready_model_test_ms: Some(NOW - 5 * MINUTE_MS),
        newest_gateway_success_ms: None,
        recent_gateway: Vec::new(),
        models: vec![
            ModelEvidence {
                newest_test: Some(S::Ready),
                tested_at_ms: None,
                request_refused: false
            };
            38
        ],
        open_circuits: 0,
        balance: BalanceSignal::default(),
        pairs: Vec::new(),
        gateway_ttft_ms: Vec::new(),
        test_ttft_ms: Vec::new(),
    }
}

fn status(inputs: &StatusInputs) -> ProviderStatus {
    provider_status(inputs, NOW).status
}

fn codes(inputs: &StatusInputs) -> Vec<ReasonCode> {
    provider_status(inputs, NOW)
        .reasons
        .iter()
        .map(|reason| reason.code)
        .collect()
}

fn attempt(minutes_ago: i64, outcome: GatewayOutcome) -> GatewayAttempt {
    GatewayAttempt {
        at_ms: NOW - minutes_ago * MINUTE_MS,
        outcome,
    }
}

#[test]
fn the_baseline_is_healthy_and_has_no_reasons() {
    let dto = provider_status(&healthy(), NOW);
    assert_eq!(dto.status, P::Healthy);
    assert!(dto.reasons.is_empty());
    assert_eq!(dto.evidence_at_ms, Some(NOW - 5 * MINUTE_MS));
    assert_eq!(dto.provider_id, "relay");
}

#[test]
fn no_evidence_is_unverified_with_no_model_answered_and_adding_a_key_changes_nothing() {
    let mut inputs = healthy();
    inputs.newest_passing_test_ms = None;
    inputs.newest_ready_model_test_ms = None;
    inputs.newest_connectivity = None;
    inputs.models = vec![
        ModelEvidence {
            newest_test: None,
            tested_at_ms: None,
            request_refused: false
        };
        38
    ];
    inputs.key_auth = vec![None];
    assert_eq!(status(&inputs), P::Unverified);
    assert_eq!(codes(&inputs), [ReasonCode::NoModelAnswered]);
    assert_eq!(provider_status(&inputs, NOW).evidence_at_ms, None);
    inputs.key_auth = vec![None, None];
    assert_eq!(status(&inputs), P::Unverified);
}

#[test]
fn no_key_is_unverified_unless_the_provider_needs_none() {
    let mut inputs = healthy();
    inputs.key_auth.clear();
    let dto = provider_status(&inputs, NOW);
    assert_eq!(dto.status, P::Unverified);
    assert_eq!(dto.reasons[0].code, ReasonCode::NoKey);
    assert_eq!(dto.reasons[0].text, "No key");
    // A local runtime (auth none) without a key is judged by its evidence.
    inputs.auth_none = true;
    assert_eq!(status(&inputs), P::Healthy);
    inputs.newest_passing_test_ms = None;
    inputs.newest_ready_model_test_ms = None;
    assert_eq!(status(&inputs), P::Unverified);
    assert_eq!(codes(&inputs), [ReasonCode::NoModelAnswered]);
}

#[test]
fn an_old_traffic_success_reads_last_verified_with_its_age() {
    let mut inputs = healthy();
    inputs.newest_passing_test_ms = None;
    inputs.newest_ready_model_test_ms = None;
    for (age, text) in [
        (6, "Last verified 6 days ago"),
        (30, "Last verified 30 days ago"),
    ] {
        inputs.newest_gateway_success_ms = Some(NOW - age * DAY_MS);
        let dto = provider_status(&inputs, NOW);
        assert_eq!(dto.status, P::Unverified);
        assert_eq!(dto.reasons[0].code, ReasonCode::LastVerified);
        assert_eq!(dto.reasons[0].text, text);
        assert_eq!(dto.evidence_at_ms, inputs.newest_gateway_success_ms);
    }
}

#[test]
fn a_failed_connectivity_check_is_unreachable() {
    for connectivity in [S::Unreachable, S::TimedOut] {
        let mut inputs = healthy();
        inputs.newest_connectivity = Some(connectivity);
        assert_eq!(status(&inputs), P::Unreachable);
        assert_eq!(codes(&inputs), [ReasonCode::ConnectivityFailed]);
    }
    // Rejected, Unsupported or Ready is an answer, not an outage.
    for connectivity in [S::Rejected, S::Unsupported, S::Ready] {
        let mut inputs = healthy();
        inputs.newest_connectivity = Some(connectivity);
        assert_ne!(status(&inputs), P::Unreachable, "{connectivity:?}");
    }
}

#[test]
fn three_failed_requests_make_a_provider_unreachable_and_one_success_clears_it() {
    let mut inputs = healthy();
    inputs.recent_gateway = vec![
        attempt(1, GatewayOutcome::TransportFailure),
        attempt(3, GatewayOutcome::TransportFailure),
        attempt(5, GatewayOutcome::TransportFailure),
    ];
    assert_eq!(status(&inputs), P::Unreachable);
    assert_eq!(codes(&inputs), [ReasonCode::RecentFailures]);
    inputs
        .recent_gateway
        .insert(0, attempt(0, GatewayOutcome::Success));
    assert_eq!(status(&inputs), P::Healthy);
}

#[test]
fn the_outage_rule_needs_three_recent_transport_failures() {
    let mut inputs = healthy();
    // Only two requests.
    inputs.recent_gateway = vec![
        attempt(1, GatewayOutcome::TransportFailure),
        attempt(2, GatewayOutcome::TransportFailure),
    ];
    assert_ne!(status(&inputs), P::Unreachable);
    // A 4xx among them is not a transport failure.
    inputs
        .recent_gateway
        .push(attempt(3, GatewayOutcome::OtherFailure));
    assert_ne!(status(&inputs), P::Unreachable);
    // Three failures, but the third is older than 15 minutes.
    inputs.recent_gateway = vec![
        attempt(1, GatewayOutcome::TransportFailure),
        attempt(2, GatewayOutcome::TransportFailure),
        attempt(16, GatewayOutcome::TransportFailure),
    ];
    assert_ne!(status(&inputs), P::Unreachable);
    // The 15-minute boundary is inclusive.
    inputs.recent_gateway[2] = attempt(15, GatewayOutcome::TransportFailure);
    assert_eq!(status(&inputs), P::Unreachable);
}

#[test]
fn needs_attention_rules_fire_one_by_one() {
    type Case = (&'static str, fn(&mut StatusInputs), ReasonCode);
    let cases: [Case; 7] = [
        (
            "all keys rejected",
            |i| i.key_auth = vec![Some(S::Rejected), Some(S::Rejected)],
            ReasonCode::AllKeysRejected,
        ),
        (
            "a model failed",
            |i| i.models[0].newest_test = Some(S::Rejected),
            ReasonCode::ModelsFailed,
        ),
        (
            "a circuit is open",
            |i| i.open_circuits = 1,
            ReasonCode::CircuitOpen,
        ),
        (
            "balance exhausted",
            |i| i.balance.account_exhausted = true,
            ReasonCode::BalanceExhausted,
        ),
        (
            "balance auth failed",
            |i| i.balance.auth_failed = true,
            ReasonCode::BalanceAuthFailed,
        ),
        (
            "a pair fails over half",
            |i| {
                i.pairs = vec![PairHealth {
                    requests: 10,
                    failure_rate: 0.51,
                }]
            },
            ReasonCode::HighFailureRate,
        ),
        (
            "unsupported chat result",
            |i| i.models[1].newest_test = Some(S::Unsupported),
            ReasonCode::ModelsFailed,
        ),
    ];
    for (name, change, code) in cases {
        let mut inputs = healthy();
        change(&mut inputs);
        assert_eq!(status(&inputs), P::NeedsAttention, "{name}");
        assert_eq!(codes(&inputs), [code], "{name}");
    }
}

#[test]
fn needs_attention_rules_do_not_fire_below_their_thresholds() {
    let mut inputs = healthy();
    // One of two keys rejected is not "all keys".
    inputs.key_auth = vec![Some(S::Rejected), Some(S::Ready)];
    // Untested and Skipped models do not count as failed.
    inputs.models[0].newest_test = None;
    inputs.models[1].newest_test = Some(S::Skipped);
    // Nine requests at 100 % and ten at exactly 50 % are under the rule.
    inputs.pairs = vec![
        PairHealth {
            requests: 9,
            failure_rate: 1.0,
        },
        PairHealth {
            requests: 10,
            failure_rate: 0.5,
        },
    ];
    assert_eq!(status(&inputs), P::Healthy);
}

#[test]
fn the_models_reason_reads_two_of_thirty_eight() {
    let mut inputs = healthy();
    inputs.models[0].newest_test = Some(S::Rejected);
    inputs.models[1].newest_test = Some(S::TimedOut);
    let dto = provider_status(&inputs, NOW);
    assert_eq!(dto.reasons[0].text, "2 of 38 models failed the last test");
    assert_eq!(
        dto.reasons[0].params,
        BTreeMap::from([("failed".to_owned(), 2), ("total".to_owned(), 38)])
    );
}

#[test]
fn several_attention_reasons_are_listed_in_rule_order() {
    let mut inputs = healthy();
    inputs.open_circuits = 2;
    inputs.balance.account_exhausted = true;
    inputs.models[0].newest_test = Some(S::Rejected);
    assert_eq!(
        codes(&inputs),
        [
            ReasonCode::ModelsFailed,
            ReasonCode::CircuitOpen,
            ReasonCode::BalanceExhausted
        ]
    );
    assert_eq!(
        provider_status(&inputs, NOW).reasons[1].text,
        "2 circuits are open"
    );
    assert_eq!(
        provider_status(&inputs, NOW).reasons[1].params,
        BTreeMap::from([("count".to_owned(), 2)])
    );
}

#[test]
fn old_evidence_leaves_the_provider_unverified_with_its_age() {
    let mut inputs = healthy();
    inputs.newest_passing_test_ms = Some(NOW - 3 * DAY_MS - HOUR_MS);
    inputs.newest_ready_model_test_ms = Some(NOW - 3 * DAY_MS - HOUR_MS);
    let dto = provider_status(&inputs, NOW);
    assert_eq!(dto.status, P::Unverified);
    assert_eq!(dto.reasons[0].text, "Last verified 3 days ago");
    assert_eq!(dto.reasons[0].code, ReasonCode::LastVerified);
    assert_eq!(
        dto.reasons[0].params,
        BTreeMap::from([("days".to_owned(), 3)])
    );
}

#[test]
fn a_passing_connectivity_test_alone_is_evidence_but_not_health() {
    // A fresh sniff: its Connectivity and Auth results count as measured evidence; untested
    // models are no failures.
    let mut inputs = healthy();
    inputs.newest_ready_model_test_ms = None;
    inputs.newest_passing_test_ms = Some(NOW - 2 * HOUR_MS);
    inputs.models = vec![
        ModelEvidence {
            newest_test: None,
            tested_at_ms: None,
            request_refused: false
        };
        12
    ];
    let dto = provider_status(&inputs, NOW);
    assert_eq!(dto.status, P::Unverified);
    assert_eq!(dto.reasons[0].code, ReasonCode::NoModelAnswered);
    assert_eq!(dto.reasons[0].text, "No model has answered yet");
    assert!(dto.reasons[0].params.is_empty());
}

#[test]
fn a_fresh_sniff_with_a_rejected_key_needs_attention() {
    let mut inputs = healthy();
    inputs.newest_passing_test_ms = Some(NOW - MINUTE_MS);
    inputs.newest_ready_model_test_ms = None;
    inputs.key_auth = vec![Some(S::Rejected)];
    inputs.models = Vec::new();
    let dto = provider_status(&inputs, NOW);
    assert_eq!(dto.status, P::NeedsAttention);
    assert_eq!(dto.reasons[0].code, ReasonCode::AllKeysRejected);
    assert_eq!(dto.reasons[0].text, "Key rejected");
}

#[test]
fn key_scope_exhaustion_counts_only_when_every_enabled_key_is_exhausted() {
    let mut inputs = healthy();
    inputs.key_auth = vec![Some(S::Ready), Some(S::Ready)];
    inputs.balance.exhausted_keys = 1;
    assert_eq!(status(&inputs), P::Healthy);
    inputs.balance.exhausted_keys = 2;
    let dto = provider_status(&inputs, NOW);
    assert_eq!(dto.status, P::NeedsAttention);
    assert_eq!(dto.reasons[0].code, ReasonCode::AllKeysExhausted);
    assert_eq!(dto.reasons[0].text, "All keys exhausted");
    // No enabled key: nothing to be exhausted.
    inputs.auth_none = true;
    inputs.key_auth.clear();
    assert_eq!(status(&inputs), P::Healthy);
}

#[test]
fn rate_limited_and_refused_chat_results_are_not_failures() {
    let mut inputs = healthy();
    inputs.models[0].newest_test = Some(S::RateLimited);
    inputs.models[1].newest_test = Some(S::InvalidResponse);
    inputs.models[1].request_refused = true;
    assert_eq!(status(&inputs), P::Healthy);
    // An invalid response that is not a refusal still is.
    inputs.models[1].request_refused = false;
    assert_eq!(status(&inputs), P::NeedsAttention);
}

#[test]
fn fresh_traffic_alone_makes_a_provider_healthy() {
    let mut inputs = healthy();
    inputs.newest_passing_test_ms = None;
    inputs.newest_ready_model_test_ms = None;
    inputs.newest_gateway_success_ms = Some(NOW - 23 * HOUR_MS);
    assert_eq!(status(&inputs), P::Healthy);
    inputs.newest_gateway_success_ms = Some(NOW - 25 * HOUR_MS);
    assert_eq!(status(&inputs), P::Unverified);
}

#[test]
fn age_texts() {
    assert_eq!(age_text(-5), "just now");
    assert_eq!(age_text(59_999), "just now");
    assert_eq!(age_text(MINUTE_MS), "1 min ago");
    assert_eq!(age_text(59 * MINUTE_MS), "59 min ago");
    assert_eq!(age_text(HOUR_MS), "1 h ago");
    assert_eq!(age_text(DAY_MS - 1), "23 h ago");
    assert_eq!(age_text(DAY_MS), "1 day ago");
    assert_eq!(age_text(10 * DAY_MS), "10 days ago");
}

/// Fourteen combined cases: only the first matching rule decides.
#[test]
fn rule_order_over_fourteen_combined_cases() {
    type Case = (&'static str, fn(&mut StatusInputs), ProviderStatus);
    let cases: [Case; 14] = [
        // Disabled beats Unverified, Unreachable and Needs attention.
        (
            "disabled + no key + unreachable + attention",
            |i| {
                i.enabled = false;
                i.key_auth.clear();
                i.newest_connectivity = Some(S::Unreachable);
                i.open_circuits = 1;
            },
            P::Disabled,
        ),
        (
            "disabled + healthy evidence",
            |i| i.enabled = false,
            P::Disabled,
        ),
        // Unverified (no key) beats Unreachable and Needs attention.
        (
            "no key + unreachable",
            |i| {
                i.key_auth.clear();
                i.newest_connectivity = Some(S::Unreachable);
            },
            P::Unverified,
        ),
        (
            "no key + attention",
            |i| {
                i.key_auth.clear();
                i.balance.account_exhausted = true;
            },
            P::Unverified,
        ),
        // Without model evidence a failing host or circuit still decides.
        (
            "no model evidence + failed connectivity",
            |i| {
                i.newest_ready_model_test_ms = None;
                i.newest_connectivity = Some(S::TimedOut);
            },
            P::Unreachable,
        ),
        (
            "no model evidence + open circuit",
            |i| {
                i.newest_ready_model_test_ms = None;
                i.open_circuits = 4;
            },
            P::NeedsAttention,
        ),
        // A fresh sniff: key accepted, no model tested yet.
        (
            "sniff accepted, 0 model tests",
            |i| {
                i.newest_ready_model_test_ms = None;
                i.models = Vec::new();
                i.key_auth = vec![Some(S::Ready)];
            },
            P::Unverified,
        ),
        // A fresh sniff with a rejected key.
        (
            "sniff rejected, 0 model tests",
            |i| {
                i.newest_ready_model_test_ms = None;
                i.models = Vec::new();
                i.key_auth = vec![Some(S::Rejected)];
            },
            P::NeedsAttention,
        ),
        // Unreachable beats Needs attention.
        (
            "unreachable + circuit open",
            |i| {
                i.newest_connectivity = Some(S::Unreachable);
                i.open_circuits = 1;
            },
            P::Unreachable,
        ),
        (
            "outage + all keys rejected",
            |i| {
                i.recent_gateway = vec![
                    attempt(1, GatewayOutcome::TransportFailure),
                    attempt(2, GatewayOutcome::TransportFailure),
                    attempt(3, GatewayOutcome::TransportFailure),
                ];
                i.key_auth = vec![Some(S::Rejected)];
            },
            P::Unreachable,
        ),
        // Needs attention beats Healthy.
        (
            "fresh evidence + failed model",
            |i| i.models[0].newest_test = Some(S::InvalidResponse),
            P::NeedsAttention,
        ),
        (
            "fresh traffic + balance exhausted",
            |i| {
                i.newest_gateway_success_ms = Some(NOW - MINUTE_MS);
                i.balance.account_exhausted = true;
            },
            P::NeedsAttention,
        ),
        // Needs attention beats the stale-evidence Unverified.
        (
            "stale evidence + circuit open",
            |i| {
                i.newest_ready_model_test_ms = Some(NOW - 5 * DAY_MS);
                i.open_circuits = 1;
            },
            P::NeedsAttention,
        ),
        // Nothing fires: Healthy.
        ("nothing fires", |_| {}, P::Healthy),
    ];
    for (name, change, expected) in cases {
        let mut inputs = healthy();
        change(&mut inputs);
        let dto = provider_status(&inputs, NOW);
        assert_eq!(dto.status, expected, "{name}");
        if expected == P::Disabled {
            assert!(dto.reasons.is_empty(), "{name}");
        }
    }
}

#[test]
fn latency_is_the_traffic_median_with_three_or_more_requests() {
    let traffic: Vec<u64> = (0..20).map(|n| 390 + n).collect();
    assert_eq!(
        latency(&traffic, &[100]),
        Some((400, LatencySource::Traffic))
    );
    // An even count takes the midpoint, rounded up.
    assert_eq!(
        latency(&[1, 2, 3, 4], &[]),
        Some((3, LatencySource::Traffic))
    );
    // An odd count takes the middle value.
    assert_eq!(
        latency(&[300, 100, 200], &[]),
        Some((200, LatencySource::Traffic))
    );
    // More than 20 requests: only the first 20 (the newest) count.
    let mut many = vec![1_000; 20];
    many.extend([1; 30]);
    assert_eq!(latency(&many, &[]), Some((1_000, LatencySource::Traffic)));
}

#[test]
fn latency_falls_back_to_the_test_run_and_then_to_nothing() {
    assert_eq!(
        latency(&[10, 20], &[300, 500]),
        Some((400, LatencySource::TestRun))
    );
    assert_eq!(latency(&[], &[]), None);
    assert_eq!(latency(&[10, 20], &[]), None);
}

#[test]
fn the_dto_carries_the_latency_and_serialises_snake_case() {
    let mut inputs = healthy();
    inputs.gateway_ttft_ms = vec![412; 20];
    let dto = provider_status(&inputs, NOW);
    assert_eq!(dto.latency_ms, Some(412));
    assert_eq!(dto.latency_source, Some(LatencySource::Traffic));
    let value = serde_json::to_value(&dto).unwrap();
    assert_eq!(value["status"], "healthy");
    assert_eq!(value["latency_source"], "traffic");
    assert_eq!(value["provider_id"], "relay");
    let needs = serde_json::to_value(P::NeedsAttention).unwrap();
    assert_eq!(needs, "needs_attention");
}

#[test]
fn reason_texts_and_params() {
    type Case = (
        fn(&mut StatusInputs),
        ReasonCode,
        &'static str,
        &'static [(&'static str, u64)],
    );
    let cases: [Case; 8] = [
        (
            |i| i.key_auth = vec![Some(S::Rejected)],
            ReasonCode::AllKeysRejected,
            "Key rejected",
            &[("keys", 1)],
        ),
        (
            |i| i.key_auth = vec![Some(S::Rejected); 3],
            ReasonCode::AllKeysRejected,
            "All 3 keys rejected",
            &[("keys", 3)],
        ),
        (
            |i| i.open_circuits = 1,
            ReasonCode::CircuitOpen,
            "1 circuit is open",
            &[("count", 1)],
        ),
        (
            |i| i.open_circuits = 2,
            ReasonCode::CircuitOpen,
            "2 circuits are open",
            &[("count", 2)],
        ),
        (
            |i| {
                i.recent_gateway = vec![
                    attempt(1, GatewayOutcome::TransportFailure),
                    attempt(2, GatewayOutcome::TransportFailure),
                    attempt(3, GatewayOutcome::TransportFailure),
                ];
            },
            ReasonCode::RecentFailures,
            "The last 3 requests failed",
            &[("count", 3)],
        ),
        (
            |i| i.balance.account_exhausted = true,
            ReasonCode::BalanceExhausted,
            "Balance exhausted",
            &[],
        ),
        (
            |i| {
                i.models[0].newest_test = Some(S::Rejected);
                i.models[1].newest_test = Some(S::TimedOut);
            },
            ReasonCode::ModelsFailed,
            "2 of 38 models failed the last test",
            &[("failed", 2), ("total", 38)],
        ),
        (
            |i| i.balance.auth_failed = true,
            ReasonCode::BalanceAuthFailed,
            "The balance could not be read: the key was rejected",
            &[],
        ),
    ];
    for (change, code, text, params) in cases {
        let mut inputs = healthy();
        change(&mut inputs);
        let dto = provider_status(&inputs, NOW);
        assert_eq!(dto.reasons.len(), 1, "{text}");
        let reason = &dto.reasons[0];
        assert_eq!((reason.code, reason.text.as_str()), (code, text));
        let expected: BTreeMap<String, u64> = params
            .iter()
            .map(|(name, value)| ((*name).to_owned(), *value))
            .collect();
        assert_eq!(reason.params, expected, "{text}");
    }
}

#[test]
fn a_later_success_or_an_old_check_clears_a_failed_connectivity() {
    let mut inputs = healthy();
    inputs.newest_connectivity = Some(S::Unreachable);
    inputs.newest_connectivity_ms = Some(NOW - 2 * HOUR_MS);
    inputs.newest_passing_test_ms = Some(NOW - 3 * HOUR_MS);
    inputs.newest_ready_model_test_ms = Some(NOW - 3 * HOUR_MS);
    assert_eq!(status(&inputs), P::Unreachable);
    // A forwarded success after the failed check proves the host answers.
    inputs.newest_gateway_success_ms = Some(NOW - HOUR_MS);
    assert_eq!(status(&inputs), P::Healthy);
    // So does a later passing check.
    inputs.newest_gateway_success_ms = None;
    inputs.newest_passing_test_ms = Some(NOW - HOUR_MS);
    assert_ne!(status(&inputs), P::Unreachable);
    // A failure older than a week no longer decides.
    inputs.newest_passing_test_ms = None;
    inputs.newest_connectivity_ms = Some(NOW - FAILURE_EVIDENCE_MS - 1);
    assert_ne!(status(&inputs), P::Unreachable);
}

#[test]
fn a_model_failure_older_than_a_week_and_not_observed_are_no_failures() {
    let mut inputs = healthy();
    inputs.models[0].newest_test = Some(S::InvalidResponse);
    inputs.models[0].tested_at_ms = Some(NOW - FAILURE_EVIDENCE_MS - 1);
    inputs.models[1].newest_test = Some(S::NotObserved);
    assert_eq!(status(&inputs), P::Healthy);
    inputs.models[0].tested_at_ms = Some(NOW - DAY_MS);
    assert_eq!(codes(&inputs), [ReasonCode::ModelsFailed]);
}

#[test]
fn a_success_after_an_exhausted_reading_means_it_was_topped_up() {
    let mut inputs = healthy();
    inputs.balance.account_exhausted = true;
    inputs.balance.exhausted_at_ms = Some(NOW - 2 * HOUR_MS);
    assert_eq!(codes(&inputs), [ReasonCode::BalanceExhausted]);
    inputs.newest_gateway_success_ms = Some(NOW - HOUR_MS);
    assert_eq!(status(&inputs), P::Healthy);
    // A success before the reading does not contradict it.
    inputs.newest_gateway_success_ms = Some(NOW - 3 * HOUR_MS);
    assert_eq!(codes(&inputs), [ReasonCode::BalanceExhausted]);
}
