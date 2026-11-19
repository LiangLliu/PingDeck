use std::time::Duration;

use super::*;

fn status(code: u16, retry_after_ms: Option<u64>) -> FailureObservation {
    FailureObservation {
        status: Some(code),
        retry_after_ms,
        ..FailureObservation::default()
    }
}

fn rest_s(book: &KeyBook, key: &str) -> Option<i64> {
    let now = book.now_ms();
    book.cooling_until(key, "m")
        .map(|until| (until - now) / 1000)
}

#[test]
fn a_429_rests_for_retry_after_capped_at_ten_minutes() {
    let book = KeyBook::new();
    book.failed("k", "m", &status(429, Some(45_000)), "429".into(), true);
    assert_eq!(rest_s(&book, "k"), Some(45));
    book.failed("j", "m", &status(429, Some(3_600_000)), "429".into(), true);
    assert_eq!(rest_s(&book, "j"), Some(600));
}

#[test]
fn a_429_without_retry_after_doubles_up_to_ten_minutes_and_recovers() {
    let book = KeyBook::new();
    let mut seen = Vec::new();
    for _ in 0..7 {
        book.failed("k", "m", &status(429, None), "429".into(), true);
        seen.push(rest_s(&book, "k").unwrap());
    }
    assert_eq!(seen, vec![30, 60, 120, 240, 480, 600, 600]);
    book.advance(Duration::from_secs(601));
    assert_eq!(
        book.cooling_until("k", "m"),
        None,
        "the rest ends on its own"
    );
    // Ten minutes after the last failure the streak starts over.
    book.failed("k", "m", &status(429, None), "429".into(), true);
    assert_eq!(rest_s(&book, "k"), Some(30));
    book.used("k", "m", true);
    assert_eq!(
        book.cooling_until("k", "m"),
        None,
        "a success clears the rest"
    );
}

#[test]
fn rejections_rest_thirty_minutes_and_clear_on_demand() {
    let book = KeyBook::new();
    for code in [401, 402, 403] {
        let key = format!("k{code}");
        book.failed(&key, "m", &status(code, None), "rejected".into(), true);
        assert_eq!(rest_s(&book, &key), Some(1800));
    }
    book.clear_cooldown("k401");
    assert_eq!(book.cooling_until("k401", "m"), None);
}

#[test]
fn upstream_trouble_and_a_switched_off_cooldown_rest_nothing_but_record_the_error() {
    let book = KeyBook::new();
    for code in [500, 503, 408, 400] {
        book.failed("k", "m", &status(code, None), format!("{code}"), true);
        assert_eq!(book.cooling_until("k", "m"), None, "{code}");
    }
    let timeout = FailureObservation {
        transport: Some(crate::llm::failover::Transport::Timeout),
        ..FailureObservation::default()
    };
    book.failed("k", "m", &timeout, "the upstream timed out".into(), true);
    assert_eq!(book.cooling_until("k", "m"), None);
    let state = book.state("k");
    assert_eq!(state.last_error_status, None);
    assert_eq!(state.last_error.as_deref(), Some("the upstream timed out"));
    assert!(state.last_used_ms.is_some() && state.last_error_ms.is_some());

    book.failed("off", "m", &status(429, None), "429".into(), false);
    let state = book.state("off");
    assert_eq!(state.cooldown_until_ms, None);
    assert_eq!(state.last_error_status, Some(429));
}

#[test]
fn strategies_pick_their_start() {
    let book = KeyBook::new();
    assert_eq!(book.start("p", KeyStrategy::Primary, 3, None), 0);
    let starts: Vec<usize> = (0..4)
        .map(|_| book.start("p", KeyStrategy::RoundRobin, 3, None))
        .collect();
    assert_eq!(starts, vec![0, 1, 2, 0]);
    assert_eq!(book.start("q", KeyStrategy::RoundRobin, 1, None), 0);
    for _ in 0..50 {
        assert!(book.start("p", KeyStrategy::Random, 3, None) < 3);
    }
}

#[test]
fn a_conversation_stays_on_the_key_it_started_on() {
    let book = KeyBook::new();
    let first = book.start("p", KeyStrategy::RoundRobin, 3, Some(7));
    // Other traffic moves the cursor; the conversation does not follow it.
    let _ = book.start("p", KeyStrategy::RoundRobin, 3, None);
    let _ = book.start("p", KeyStrategy::RoundRobin, 3, Some(8));
    for _ in 0..3 {
        assert_eq!(book.start("p", KeyStrategy::RoundRobin, 3, Some(7)), first);
    }
    assert_eq!(
        book.peek_start("p", KeyStrategy::RoundRobin, 3, Some(7)),
        first
    );
}

#[test]
fn a_rate_limit_rests_the_model_and_an_account_failure_the_whole_key() {
    let book = KeyBook::new();
    assert!(
        book.failed("k", "a", &status(429, Some(60_000)), "429".into(), true),
        "the model's rest is a run-state change the UI hears about"
    );
    assert!(book.cooling_until("k", "a").is_some());
    assert_eq!(
        book.cooling_until("k", "b"),
        None,
        "model b is not held back"
    );
    let state = book.state("k");
    assert_eq!(state.cooldown_until_ms, None);
    assert_eq!(
        state.model_rest.map(|(model, _)| model).as_deref(),
        Some("a"),
        "the key reports which model rests"
    );
    // Clearing only a model's rest still reports a change.
    assert!(book.clear_cooldown("k"));
    assert_eq!(book.cooling_until("k", "a"), None);
    assert_eq!(book.state("k").model_rest, None);
    book.failed("k", "a", &status(429, Some(60_000)), "429".into(), true);
    book.used("k", "b", true);
    assert!(
        book.cooling_until("k", "a").is_some(),
        "b's success leaves a's rest"
    );
    assert!(book.failed("k", "a", &status(402, None), "402".into(), true));
    assert!(
        book.cooling_until("k", "b").is_some(),
        "quota rests every model"
    );
    assert!(book.state("k").cooldown_until_ms.is_some());
    assert!(book.clear_cooldown("k"));
    assert_eq!(book.cooling_until("k", "a"), None);
}

#[test]
fn the_error_line_never_holds_the_key() {
    let secret = "sk-live-abcdefghijklmnop1234";
    let error = AttemptError::Status {
        status: 401,
        body: format!("{{\"error\":{{\"message\":\"Incorrect API key {secret}\"}}}}\nmore"),
        retry_after_ms: None,
    };
    let line = error_line(&error, secret);
    assert!(!line.contains(secret), "{line}");
    assert!(line.starts_with("upstream returned HTTP 401"), "{line}");
}
