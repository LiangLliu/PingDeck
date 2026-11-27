//! Tests of the circuit breaker: windows, doubling cooldowns, the single probe and the figures.

use std::sync::Arc;

use super::*;

fn config(threshold: u32, window_s: u64, open_s: u64, max_s: u64) -> CircuitConfig {
    CircuitConfig {
        failure_threshold: threshold,
        window: Duration::from_secs(window_s),
        open_for: Duration::from_secs(open_s),
        open_max: Duration::from_secs(max_s),
    }
}

const CONFIG: CircuitConfig = CircuitConfig {
    failure_threshold: 2,
    window: Duration::from_secs(30),
    open_for: Duration::from_millis(40),
    open_max: Duration::from_millis(400),
};

fn acquire_and_fail(book: &HealthBook, config: CircuitConfig) {
    assert_eq!(book.acquire("k", "m"), Permit::Allowed);
    book.record("k", "m", Outcome::Failure, Duration::ZERO, config);
}

fn row(book: &HealthBook) -> HealthSnapshot {
    book.snapshot().remove(0)
}

#[test]
fn consecutive_failures_open_the_circuit_and_one_probe_follows() {
    let book = HealthBook::with_fixed_clock();
    acquire_and_fail(&book, CONFIG);
    assert_eq!(book.acquire("k", "m"), Permit::Allowed);
    book.record("k", "m", Outcome::Failure, Duration::ZERO, CONFIG);
    assert_eq!(book.acquire("k", "m"), Permit::Blocked);
    assert!(book.is_blocked("k", "m"));
    book.advance(Duration::from_millis(39));
    assert_eq!(book.acquire("k", "m"), Permit::Blocked);
    book.advance(Duration::from_millis(1));
    assert_eq!(book.acquire("k", "m"), Permit::Allowed);
    // A second caller while the probe is in flight is refused.
    assert_eq!(book.acquire("k", "m"), Permit::Blocked);
    book.record("k", "m", Outcome::Success, Duration::from_millis(5), CONFIG);
    assert_eq!(book.acquire("k", "m"), Permit::Allowed);
    assert_eq!(row(&book).state, CircuitState::Closed);
}

#[test]
fn success_resets_the_streak_and_averages_move() {
    let book = HealthBook::with_fixed_clock();
    acquire_and_fail(&book, CONFIG);
    book.record(
        "k",
        "m",
        Outcome::Success,
        Duration::from_millis(100),
        CONFIG,
    );
    acquire_and_fail(&book, CONFIG);
    assert_eq!(book.acquire("k", "m"), Permit::Allowed, "streak was reset");
    let row = row(&book);
    assert!(row.failure_rate > 0.0 && row.failure_rate < 1.0);
    assert!((row.latency_ms - 100.0).abs() < 1e-6);
}

#[test]
fn latency_average_weights_the_newest_sample_by_alpha() {
    let book = HealthBook::with_fixed_clock();
    for millis in [100, 200] {
        book.record(
            "k",
            "m",
            Outcome::Success,
            Duration::from_millis(millis),
            CONFIG,
        );
    }
    assert!((row(&book).latency_ms - 130.0).abs() < 1e-6);
}

#[test]
fn three_failures_inside_the_window_open_the_circuit() {
    let config = CircuitConfig::default();
    let book = HealthBook::with_fixed_clock();
    for _ in 0..2 {
        acquire_and_fail(&book, config);
        book.advance(Duration::from_secs(10));
    }
    assert_eq!(row(&book).state, CircuitState::Closed);
    assert_eq!(book.acquire("k", "m"), Permit::Allowed);
    acquire_and_fail(&book, config);
    let open = row(&book);
    assert_eq!(
        (open.state, open.consecutive_failures, open.cooldown_s),
        (CircuitState::Open, 3, 30)
    );
}

#[test]
fn the_same_failures_spread_over_forty_seconds_do_not() {
    let config = CircuitConfig::default();
    let book = HealthBook::with_fixed_clock();
    for _ in 0..3 {
        acquire_and_fail(&book, config);
        book.advance(Duration::from_secs(20));
    }
    let row = row(&book);
    assert_eq!(row.state, CircuitState::Closed);
    assert_eq!(row.consecutive_failures, 2, "the first one left the window");
    assert_eq!(book.acquire("k", "m"), Permit::Allowed);
}

#[test]
fn failed_probes_double_the_cooldown_up_to_the_cap() {
    let config = CircuitConfig::default();
    let book = HealthBook::with_fixed_clock();
    for _ in 0..3 {
        acquire_and_fail(&book, config);
    }
    let mut seen = vec![row(&book).cooldown_s];
    for _ in 0..5 {
        book.advance(Duration::from_secs(u64::from(seen[seen.len() - 1])));
        assert_eq!(book.acquire("k", "m"), Permit::Allowed);
        assert_eq!(row(&book).state, CircuitState::HalfOpen);
        book.record("k", "m", Outcome::Failure, Duration::ZERO, config);
        seen.push(row(&book).cooldown_s);
    }
    assert_eq!(seen, [30, 60, 120, 240, 300, 300]);
}

#[test]
fn a_probe_success_closes_and_resets_cooldown_and_failures() {
    let config = CircuitConfig::default();
    let book = HealthBook::with_fixed_clock();
    for _ in 0..3 {
        acquire_and_fail(&book, config);
    }
    for _ in 0..2 {
        book.advance(Duration::from_secs(300));
        assert_eq!(book.acquire("k", "m"), Permit::Allowed);
        book.record("k", "m", Outcome::Failure, Duration::ZERO, config);
    }
    assert_eq!(row(&book).cooldown_s, 120);
    book.advance(Duration::from_secs(300));
    assert_eq!(book.acquire("k", "m"), Permit::Allowed);
    book.record("k", "m", Outcome::Success, Duration::from_millis(9), config);
    let closed = row(&book);
    assert_eq!(
        (
            closed.state,
            closed.cooldown_s,
            closed.consecutive_failures,
            closed.open_until_ms,
            closed.retry_in_s
        ),
        (CircuitState::Closed, 30, 0, None, None)
    );
    // Opening again starts from 30 s.
    for _ in 0..3 {
        acquire_and_fail(&book, config);
    }
    assert_eq!(row(&book).cooldown_s, 30);
}

#[test]
fn the_cooldown_ceiling_is_never_below_the_first_cooldown() {
    let config = config(1, 30, 60, 5);
    let book = HealthBook::with_fixed_clock();
    acquire_and_fail(&book, config);
    assert_eq!(row(&book).cooldown_s, 60);
    book.advance(Duration::from_secs(60));
    assert_eq!(book.acquire("k", "m"), Permit::Allowed);
    book.record("k", "m", Outcome::Failure, Duration::ZERO, config);
    assert_eq!(row(&book).cooldown_s, 60);
}

#[test]
fn half_open_admits_exactly_one_request_under_concurrency() {
    let config = CircuitConfig::default();
    let book = Arc::new(HealthBook::with_fixed_clock());
    for _ in 0..3 {
        acquire_and_fail(&book, config);
    }
    book.advance(Duration::from_secs(30));
    let admitted: usize = (0..32)
        .map(|_| {
            let book = Arc::clone(&book);
            std::thread::spawn(move || usize::from(book.acquire("k", "m") == Permit::Allowed))
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .sum();
    assert_eq!(admitted, 1);
    assert_eq!(row(&book).state, CircuitState::HalfOpen);
}

#[test]
fn canceling_the_half_open_probe_frees_its_slot_without_counting() {
    let config = CircuitConfig::default();
    let book = HealthBook::with_fixed_clock();
    for _ in 0..3 {
        acquire_and_fail(&book, config);
    }
    let total = row(&book).total;
    book.advance(Duration::from_secs(30));
    assert_eq!(book.acquire("k", "m"), Permit::Allowed);
    assert_eq!(book.acquire("k", "m"), Permit::Blocked);
    book.record("k", "m", Outcome::Canceled, Duration::ZERO, config);
    assert_eq!(row(&book).total, total);
    assert_eq!(book.acquire("k", "m"), Permit::Allowed);
}

#[test]
fn a_late_failure_while_open_does_not_extend_the_cooldown() {
    let config = CircuitConfig::default();
    let book = HealthBook::with_fixed_clock();
    for _ in 0..3 {
        acquire_and_fail(&book, config);
    }
    book.advance(Duration::from_secs(20));
    book.record("k", "m", Outcome::Failure, Duration::ZERO, config);
    book.advance(Duration::from_secs(10));
    assert_eq!(book.acquire("k", "m"), Permit::Allowed);
}

#[test]
fn snapshots_carry_the_open_time_and_the_ui_words() {
    let config = CircuitConfig::default();
    let book = HealthBook::with_fixed_clock();
    assert!(book.snapshot().is_empty());
    acquire_and_fail(&book, config);
    let closed = row(&book);
    assert_eq!(
        (
            closed.label.as_str(),
            closed.open_until_ms,
            closed.retry_in_s
        ),
        ("Healthy", None, None)
    );
    acquire_and_fail(&book, config);
    acquire_and_fail(&book, config);
    book.advance(Duration::from_millis(5_500));
    let open = row(&book);
    assert_eq!(open.state, CircuitState::Open);
    assert_eq!(open.retry_in_s, Some(25));
    assert_eq!(open.label, "Circuit open, retry in 25 s");
    let now = book.wall_ms();
    let until = open.open_until_ms.unwrap();
    assert!((until - now - 24_500).abs() < 200, "{until} {now}");
    let opened_for = open.opened_for_ms.unwrap();
    assert!((5_500..5_700).contains(&opened_for), "{opened_for}");
    book.advance(Duration::from_secs(24));
    assert_eq!(row(&book).retry_in_s, Some(1), "never below one second");
    book.advance(Duration::from_secs(1));
    assert_eq!(book.acquire("k", "m"), Permit::Allowed);
    let half = row(&book);
    assert_eq!(
        (half.label.as_str(), half.open_until_ms, half.opened_for_ms),
        ("Recovering", None, None)
    );
}

#[test]
fn blocked_for_does_not_take_the_probe_slot() {
    let config = CircuitConfig::default();
    let book = HealthBook::with_fixed_clock();
    assert_eq!(book.blocked_for("k", "m"), None);
    for _ in 0..3 {
        acquire_and_fail(&book, config);
    }
    assert!(book.blocked_for("k", "m").unwrap() > Duration::from_secs(29));
    book.advance(Duration::from_secs(30));
    assert_eq!(book.blocked_for("k", "m"), None);
    assert_eq!(book.blocked_for("k", "m"), None);
    assert_eq!(book.acquire("k", "m"), Permit::Allowed);
    assert_eq!(
        book.blocked_for("k", "m"),
        Some(Duration::from_secs(1)),
        "the probe is in flight"
    );
}

fn open_pair(book: &HealthBook, key: &str, model: &str, config: CircuitConfig) {
    for _ in 0..config.failure_threshold {
        book.record(key, model, Outcome::Failure, Duration::ZERO, config);
    }
}

#[test]
fn earliest_retry_is_the_smallest_remaining_cooldown_in_whole_seconds() {
    let book = HealthBook::with_fixed_clock();
    let slow = config(1, 30, 60, 300);
    let quick = config(1, 30, 8, 300);
    open_pair(&book, "a", "m", slow);
    open_pair(&book, "b", "m", quick);
    book.advance(Duration::from_millis(1_500));
    assert_eq!(
        book.earliest_retry_s(&[("a", "m"), ("b", "m"), ("c", "m")]),
        Some(7)
    );
    assert_eq!(book.earliest_retry_s(&[("a", "m")]), Some(59));
    assert_eq!(book.earliest_retry_s(&[("c", "m")]), None);
    assert_eq!(book.earliest_retry_s(&[]), None);
    book.advance(Duration::from_millis(6_000));
    assert_eq!(book.earliest_retry_s(&[("b", "m")]), Some(1));
}

#[test]
fn open_circuits_lists_only_open_pairs_and_can_filter_by_key() {
    let book = HealthBook::with_fixed_clock();
    let one = config(1, 30, 30, 300);
    open_pair(&book, "a", "m1", one);
    open_pair(&book, "a", "m2", one);
    open_pair(&book, "b", "m1", one);
    book.record(
        "c",
        "m1",
        Outcome::Failure,
        Duration::ZERO,
        CircuitConfig::default(),
    );
    assert_eq!(book.open_circuits(None).len(), 3);
    let only_a = |key: &str| key == "a";
    let rows = book.open_circuits(Some(&only_a));
    assert_eq!(
        rows.iter().map(|r| r.model_id.as_str()).collect::<Vec<_>>(),
        ["m1", "m2"]
    );
    book.advance(Duration::from_secs(30));
    assert_eq!(book.acquire("a", "m1"), Permit::Allowed);
    assert_eq!(
        book.open_circuits(Some(&only_a)).len(),
        1,
        "a half-open pair is not listed as open"
    );
}

#[test]
fn reset_none_clears_every_pair_and_reset_one_key_only_its_own() {
    let book = HealthBook::with_fixed_clock();
    let one = config(1, 30, 30, 300);
    open_pair(&book, "a", "m", one);
    open_pair(&book, "b", "m", one);
    book.reset(Some("a"));
    assert_eq!(book.snapshot().len(), 1);
    book.reset(None);
    assert!(book.snapshot().is_empty());
    assert_eq!(book.acquire("b", "m"), Permit::Allowed);
}

#[test]
fn failure_rate_orders_keys_and_decays_with_successes() {
    let book = HealthBook::with_fixed_clock();
    let config = CircuitConfig::default();
    book.record("bad", "m", Outcome::Failure, Duration::ZERO, config);
    book.record("good", "m", Outcome::Success, Duration::ZERO, config);
    assert!(book.failure_rate("bad", "m") > book.failure_rate("good", "m"));
    let before = book.failure_rate("bad", "m");
    book.record("bad", "m", Outcome::Success, Duration::ZERO, config);
    assert!(book.failure_rate("bad", "m") < before);
    assert!(book.failure_rate("nobody", "m").abs() < f64::EPSILON);
}

#[test]
fn a_pair_is_independent_of_other_models_of_the_same_key() {
    let book = HealthBook::with_fixed_clock();
    let one = config(1, 30, 30, 300);
    open_pair(&book, "k", "m1", one);
    assert_eq!(book.acquire("k", "m1"), Permit::Blocked);
    assert_eq!(book.acquire("k", "m2"), Permit::Allowed);
}

#[test]
fn only_real_circuit_transitions_are_reported() {
    let book = HealthBook::with_fixed_clock();
    let record = |outcome| book.record("k", "m", outcome, Duration::ZERO, CONFIG);
    assert!(
        !record(Outcome::Failure),
        "one failure of two opens nothing"
    );
    assert!(
        record(Outcome::Failure),
        "the second failure opens the circuit"
    );
    assert!(
        !record(Outcome::Failure),
        "a late failure of an open circuit changes nothing"
    );
    assert_eq!(book.acquire_noting("k", "m"), (Permit::Blocked, false));
    book.advance(Duration::from_millis(40));
    assert_eq!(
        book.acquire_noting("k", "m"),
        (Permit::Allowed, true),
        "the probe turns it half-open"
    );
    assert!(
        !record(Outcome::Canceled),
        "a cancelled probe changes nothing"
    );
    assert_eq!(
        book.acquire_noting("k", "m"),
        (Permit::Allowed, false),
        "the next probe finds it half-open already"
    );
    assert!(record(Outcome::Failure), "a failed probe reopens it");
    book.advance(Duration::from_millis(40));
    assert_eq!(
        book.acquire_noting("k", "m"),
        (Permit::Blocked, false),
        "the cooldown doubled to 80 ms"
    );
    book.advance(Duration::from_millis(40));
    assert_eq!(book.acquire_noting("k", "m"), (Permit::Allowed, true));
    assert!(record(Outcome::Success), "a successful probe closes it");
    assert!(
        !record(Outcome::Success),
        "a success of a closed circuit changes nothing"
    );
}

// A first attempt that was cancelled used to leave a ghost
// "Healthy" row with no requests.
#[test]
fn canceling_an_unknown_pair_leaves_no_row() {
    let book = HealthBook::with_fixed_clock();
    assert!(!book.record(
        "k",
        "m",
        Outcome::Canceled,
        Duration::ZERO,
        CircuitConfig::default()
    ));
    assert!(book.snapshot().is_empty());
    // A known pair still frees its probe slot (see
    // `canceling_the_half_open_probe_frees_its_slot_without_counting`), and a cancelled attempt
    // of another model of a known key adds no row either.
    acquire_and_fail(&book, CircuitConfig::default());
    book.record(
        "k",
        "other",
        Outcome::Canceled,
        Duration::ZERO,
        CircuitConfig::default(),
    );
    assert_eq!(book.snapshot().len(), 1);
}
