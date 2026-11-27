//! Executor behaviours: the budget ledger, the 429 back-off, redirects and the stop rules.

use std::sync::Mutex;

use super::*;
use crate::llm::safe_http::scripted::{Reply, ScriptedHttp};

const ORIGIN: &str = "https://x.test";

struct Rig {
    http: ScriptedHttp,
    cancel: CancelToken,
    events: Mutex<Vec<SniffEvent>>,
}

impl Rig {
    fn new(http: ScriptedHttp) -> Self {
        Self {
            http,
            cancel: CancelToken::new(),
            events: Mutex::new(Vec::new()),
        }
    }

    fn exec(&self, budget: u32, limits: Limits) -> Exec<'_> {
        Exec::new(ExecConfig {
            origin: ORIGIN.to_owned(),
            typed_mount: "/v1".to_owned(),
            allow_private: false,
            budget,
            limits,
            base_headers: Vec::new(),
            secrets: Vec::new(),
            proxy_dns: false,
            http: &self.http,
            cancel: &self.cancel,
            sink: self,
        })
    }

    fn events(&self) -> Vec<SniffEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl EventSink<SniffEvent> for Rig {
    fn emit(&self, event: SniffEvent) -> bool {
        self.events.lock().unwrap().push(event);
        true
    }
}

fn get(path: &str) -> Call {
    Call::new(
        Purpose::Models,
        "Models",
        SafeMethod::Get,
        path,
        Duration::from_secs(5),
        Spend::Free,
    )
}

fn rig() -> Rig {
    Rig::new(ScriptedHttp::new())
}

#[test]
fn a_full_budget_gives_the_highest_lower_priority_slot_away() {
    let rig = rig();
    let ex = rig.exec(3, Limits::PRODUCTION);
    assert_eq!(ex.hold(4), Some(1));
    assert_eq!(ex.hold(7), Some(2));
    assert_eq!(ex.hold(5), Some(3));
    assert_eq!(ex.hold(2), Some(4));
    assert_eq!(ex.holds(), [(1, 4), (3, 5), (4, 2)]);
}

#[test]
fn of_equal_victims_the_last_reserved_one_goes() {
    let rig = rig();
    let ex = rig.exec(2, Limits::PRODUCTION);
    assert_eq!(ex.hold(5), Some(1));
    assert_eq!(ex.hold(5), Some(2));
    assert_eq!(ex.hold(3), Some(3));
    assert_eq!(ex.holds(), [(1, 5), (3, 3)]);
}

#[test]
fn a_retry_tier_never_takes_a_route_slot() {
    let rig = rig();
    let ex = rig.exec(1, Limits::PRODUCTION);
    assert_eq!(ex.hold(4), Some(1));
    assert_eq!(ex.hold(8), None);
    assert_eq!(ex.holds(), [(1, 4)]);
}

#[test]
fn reserve_uses_its_own_slot_or_competes_like_a_new_hold() {
    let rig = rig();
    let ex = rig.exec(1, Limits::PRODUCTION);
    let own = ex.hold(4);
    assert!(ex.reserve(Spend::held(4, own)));
    assert_eq!((ex.used(), ex.remaining()), (1, 0));
    assert!(ex.holds().is_empty());

    // A slot given back earlier is stale: the request competes for room like any other.
    let ex = rig.exec(2, Limits::PRODUCTION);
    let stale = ex.hold(4);
    ex.release_all_except(None);
    assert!(ex.reserve(Spend::held(4, stale)));
    assert_eq!((ex.used(), ex.remaining()), (1, 1));

    let ex = rig.exec(1, Limits::PRODUCTION);
    assert_eq!(ex.hold(2), Some(1));
    assert!(!ex.reserve(Spend::held(5, Some(99))));
    assert_eq!(ex.used(), 0);
    assert!(
        ex.reserve(Spend::held(1, Some(99))),
        "tier 1 takes the tier-2 slot"
    );
    assert!(ex.holds().is_empty());
}

#[test]
fn retries_keep_tiers_one_to_three_and_drop_the_rest_to_eight() {
    for tier in 1..=3 {
        assert_eq!(
            Spend::held(tier, Some(7)).retry(),
            Spend::Budgeted { tier, hold: None }
        );
    }
    for tier in 4..=7 {
        assert_eq!(
            Spend::held(tier, Some(7)).retry(),
            Spend::Budgeted {
                tier: 8,
                hold: None
            }
        );
    }
    assert_eq!(Spend::Free.retry(), Spend::Free);
    assert_eq!(Spend::Verify.retry(), Spend::Verify);
}

#[test]
fn verification_is_counted_but_never_budgeted() {
    let rig = rig();
    let ex = rig.exec(2, Limits::PRODUCTION);
    assert!(ex.reserve(Spend::Verify));
    assert_eq!((ex.used(), ex.verified(), ex.remaining()), (1, 1, 2));
    assert!(ex.reserve(Spend::tier(4)));
    assert_eq!((ex.used(), ex.verified(), ex.remaining()), (2, 1, 1));
    assert!(ex.reserve(Spend::Free));
    assert_eq!(ex.used(), 2);
}

async fn waiting_seconds(retry_after: &str) -> Vec<u32> {
    let rig = Rig::new(ScriptedHttp::new().route(
        SafeMethod::Get,
        "/v1/models",
        vec![
            Reply::json(429, &serde_json::json!({})).with_header("retry-after", retry_after),
            Reply::json(200, &serde_json::json!({})),
        ],
    ));
    let ex = rig.exec(4, Limits::PRODUCTION);
    ex.call(get("/v1/models")).await;
    assert_eq!(rig.http.count(), 2);
    rig.events()
        .into_iter()
        .filter_map(|event| match event {
            SniffEvent::Waiting { seconds } => Some(seconds),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_429_waits_retry_after_seconds_up_to_the_cap() {
    assert_eq!(waiting_seconds("2").await, [2]);
    assert_eq!(waiting_seconds("99").await, [5]);
    assert_eq!(
        waiting_seconds("soon").await,
        [1],
        "no number: the default wait"
    );
}

async fn redirect(location: Option<&str>, requested: &str) -> (Option<String>, Vec<SniffWarning>) {
    let mut reply = Reply::text(301, "text/plain", "");
    if let Some(location) = location {
        reply = reply.with_header("location", location);
    }
    let rig = Rig::new(ScriptedHttp::new().route(SafeMethod::Get, "/v1/models", vec![reply]));
    let ex = rig.exec(4, Limits::PRODUCTION);
    ex.call(get(requested)).await;
    let collected = ex.collect();
    (collected.suggested_url, collected.warnings)
}

#[tokio::test]
async fn a_same_host_redirect_suggests_the_typed_mount_behind_the_new_prefix() {
    let (url, warnings) = redirect(Some("http://x.test:8080/api/v1/models"), "/v1/models").await;
    assert_eq!(url.as_deref(), Some("http://x.test:8080/api/v1"));
    assert_eq!(
        warnings,
        [SniffWarning::RedirectIgnored {
            location_host: Some("x.test".to_owned())
        }]
    );
    let (url, _) = redirect(Some("/v1/models/"), "/v1/models?x=1").await;
    assert_eq!(url.as_deref(), Some("https://x.test/v1"));
}

#[tokio::test]
async fn foreign_or_unmappable_redirects_suggest_nothing() {
    let (url, warnings) = redirect(Some("https://evil.test/v1/models"), "/v1/models").await;
    assert_eq!(url, None);
    assert_eq!(
        warnings,
        [SniffWarning::RedirectIgnored {
            location_host: Some("evil.test".to_owned())
        }]
    );
    assert_eq!(redirect(Some("/login"), "/v1/models").await.0, None);
    let (url, warnings) = redirect(None, "/v1/models").await;
    assert_eq!(url, None);
    assert_eq!(
        warnings,
        [SniffWarning::RedirectIgnored {
            location_host: None
        }]
    );
}

#[tokio::test]
async fn a_root_request_redirected_elsewhere_suggests_nothing() {
    // Regression guard (an empty requested path used to match any target as a suffix
    // and suggested `https://x.test/login/v1`).
    for requested in ["/", "/?x=1"] {
        let rig = Rig::new(ScriptedHttp::new().route(
            SafeMethod::Get,
            "/",
            vec![Reply::text(302, "text/plain", "").with_header("location", "/login")],
        ));
        let ex = rig.exec(4, Limits::PRODUCTION);
        ex.call(get(requested)).await;
        let collected = ex.collect();
        assert_eq!(collected.suggested_url, None, "{requested}");
        assert_eq!(
            collected.warnings,
            [SniffWarning::RedirectIgnored {
                location_host: Some("x.test".to_owned())
            }]
        );
    }
}

#[tokio::test]
async fn truncation_is_warned_once_and_a_challenge_stops_later_requests() {
    let truncated = Reply::Response {
        status: 200,
        headers: vec![("content-type".into(), "application/json".into())],
        body: b"{\"data\":[".to_vec(),
        delay: Duration::ZERO,
    };
    let rig = Rig::new(
        ScriptedHttp::new()
            .route(SafeMethod::Get, "/big", vec![truncated])
            .route(
                SafeMethod::Get,
                "/gate",
                vec![Reply::text(403, "text/plain", "no").with_header("cf-mitigated", "challenge")],
            ),
    );
    let ex = rig.exec(8, Limits::PRODUCTION);
    for _ in 0..2 {
        let mut call = get("/big");
        call.max_body = 4;
        ex.call(call).await;
    }
    ex.call(get("/gate")).await;
    assert!(matches!(
        ex.call(get("/after")).await,
        Sent::Skipped(SkipReason::Stopped)
    ));
    let html_ok = Call {
        html_ok: true,
        ..get("/after")
    };
    assert!(matches!(ex.call(html_ok).await, Sent::Done { .. }));
    assert_eq!(ex.stop_reason(), Some(StopReason::Challenge));
    let finished: Vec<String> = rig
        .events()
        .into_iter()
        .filter_map(|event| match event {
            SniffEvent::RequestFinished {
                status: None, note, ..
            } => Some(note),
            _ => None,
        })
        .collect();
    assert_eq!(finished, ["skipped (stopped)"]);
    let collected = ex.collect();
    assert_eq!(
        collected.warnings,
        [SniffWarning::BodyTruncated, SniffWarning::ChallengePage]
    );
    assert_eq!(
        collected.skipped,
        [SkippedItem {
            what: "models".to_owned(),
            reason: "stopped".to_owned()
        }]
    );
}

#[tokio::test]
async fn challenge_and_rate_limit_texts_in_a_2xx_json_answer_are_data() {
    // Regression guard (this list used to stop the scan and be retried as a 429).
    let listed = serde_json::json!({
        "data": [{"id": "m", "description": "Attention Required: 请求过于频繁"}]
    });
    let rig = Rig::new(
        ScriptedHttp::new()
            .route(
                SafeMethod::Get,
                "/v1/models",
                vec![Reply::json(200, &listed)],
            )
            .route(
                SafeMethod::Get,
                "/page",
                vec![Reply::text(200, "text/plain", "Just a moment...")],
            ),
    );
    let ex = rig.exec(8, Limits::scaled(100));
    assert!(ex.call(get("/v1/models")).await.response().is_some());
    assert_eq!(rig.http.count(), 1, "no rate-limit retry");
    assert_eq!(ex.stop_reason(), None);
    // The same texts outside a JSON answer still mark a challenge page.
    ex.call(get("/page")).await;
    assert_eq!(ex.stop_reason(), Some(StopReason::Challenge));
}

#[test]
fn every_purpose_names_its_skip_phase_and_conclusion() {
    let table = [
        (Purpose::Local, "local", "local", "identity"),
        (Purpose::Identify, "identify", "identify", "identity"),
        (Purpose::Models, "models", "models", "models"),
        (
            Purpose::Route(Protocol::Gemini),
            "route:gemini",
            "routes",
            "route:gemini",
        ),
        (
            Purpose::Verify(Protocol::OpenaiChat),
            "verify",
            "verify",
            "route:openai_chat",
        ),
        (Purpose::Balance, "balance", "balance", "balance"),
    ];
    for (purpose, what, phase, conclusion) in table {
        assert_eq!(
            (
                purpose.what().as_ref(),
                purpose.phase(),
                purpose.conclusion().as_str()
            ),
            (what, phase, conclusion),
            "{purpose:?}"
        );
    }
}

#[test]
fn ceil_seconds_rounds_up_and_saturates() {
    use std::time::Duration;
    assert_eq!(ceil_seconds(Duration::ZERO), 0);
    assert_eq!(ceil_seconds(Duration::from_secs(2)), 2);
    assert_eq!(ceil_seconds(Duration::from_millis(2_001)), 3);
    assert_eq!(
        ceil_seconds(Duration::from_secs(u64::from(u32::MAX) + 1)),
        u32::MAX
    );
    // `Duration::MAX` has non-zero nanos: the round-up must not overflow the `u64` seconds.
    assert_eq!(ceil_seconds(Duration::MAX), u32::MAX);
}
