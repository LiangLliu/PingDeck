//! Tests of the in-process client when an upstream answers 2xx with a body it cannot read (a relay
//! or captive portal serving HTML, a garbage first stream frame): the call fails over to the next
//! target and the breaker counts it. Also the circuit's `HealthChanged` events.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;

use crate::llm::client::{ChatTarget, LlmClient};
use crate::llm::events::LlmEvent;
use crate::llm::gateway::tests::{Canned, Fake, Harness, Reply, Step, canned_json, canned_sse};
use crate::llm::health::{CircuitState, HealthSnapshot};
use crate::llm::hub::{HubEvent, HubRequest, collect_events};
use crate::llm::wire::Protocol;

const CHAT: Protocol = Protocol::OpenaiChat;

fn hello() -> HubRequest {
    HubRequest::user_prompt("ignored-model", "hi")
}

/// A named target; `fo` brings the chain its alias has as a gateway rule (an in-app caller
/// brings its own).
fn named(name: &str) -> ChatTarget {
    ChatTarget::Named {
        name: name.into(),
        fallbacks: if name == "fo" {
            vec!["b/mb".to_owned()]
        } else {
            Vec::new()
        },
    }
}

/// An upstream whose JSON answer is `<html>` and whose stream starts with a frame that is not
/// JSON, both `200`.
async fn portal() -> Fake {
    Fake::start(|_, request| {
        if request.body["stream"] == true {
            Reply::Sse(vec![Step::Frame("data: <html>login</html>\n\n".to_owned())])
        } else {
            Reply::Text(200, "text/html", "<html>login</html>".to_owned())
        }
    })
    .await
}

async fn good() -> Fake {
    Fake::start(|_, request| {
        let canned = Canned::text(&["from b"]);
        if request.body["stream"] == true {
            Reply::Sse(canned_sse(CHAT, "up", &canned))
        } else {
            Reply::Json(200, canned_json(CHAT, "up", &canned))
        }
    })
    .await
}

fn health_events(h: &Harness) -> Vec<Vec<HealthSnapshot>> {
    h.events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            LlmEvent::HealthChanged { snapshots } => Some(snapshots.clone()),
            _ => None,
        })
        .collect()
}

fn state_of(snapshots: &[HealthSnapshot], key: &str) -> CircuitState {
    snapshots
        .iter()
        .find(|row| row.key_id == key)
        .unwrap()
        .state
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreadable_2xx_fails_over_and_counts_against_the_breaker() {
    for stream in [false, true] {
        let a = portal().await;
        let b = good().await;
        let h = Harness::build(|_| {});
        h.add_provider("a", a.port, &[CHAT], &["ka"], &["ma"]);
        h.add_provider("b", b.port, &[CHAT], &["kb"], &["mb"]);
        h.add_alias("fo", &[("a", None, "ma"), ("b", None, "mb")]);
        let client = LlmClient::new(Arc::clone(&h.llm));
        let text = if stream {
            let events: Vec<HubEvent> = client
                .stream(named("fo"), hello(), None)
                .map(|item| item.unwrap())
                .collect()
                .await;
            collect_events(&events).unwrap().text()
        } else {
            client
                .complete(&named("fo"), hello(), None)
                .await
                .unwrap()
                .text()
        };
        assert_eq!(text, "from b", "stream={stream}");
        assert_eq!((a.count(), b.count()), (1, 1), "stream={stream}");
        assert_eq!(
            h.health("ka", "ma").unwrap().consecutive_failures,
            1,
            "stream={stream}: the portal's 200 is a failure, not a success"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_circuit_that_opens_half_opens_and_closes_is_announced() {
    let a = Fake::start(|number, _| {
        if number < 2 {
            Reply::Text(200, "text/html", "<html>login</html>".to_owned())
        } else {
            Reply::Json(200, canned_json(CHAT, "up", &Canned::text(&["back"])))
        }
    })
    .await;
    let h = Harness::build(|g| {
        g.circuit_failure_threshold = 2;
        g.circuit_open_s = 30;
    });
    h.add_provider("a", a.port, &[CHAT], &["ka"], &["ma"]);
    let client = LlmClient::new(Arc::clone(&h.llm));
    assert!(
        client
            .complete(&named("a/ma"), hello(), None)
            .await
            .is_err()
    );
    assert!(
        health_events(&h).is_empty(),
        "one failure changes no circuit"
    );
    assert!(
        client
            .complete(&named("a/ma"), hello(), None)
            .await
            .is_err()
    );
    let events = health_events(&h);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(state_of(&events[0], "ka"), CircuitState::Open);

    h.llm.health.advance(Duration::from_secs(30));
    assert_eq!(
        client
            .complete(&named("a/ma"), hello(), None)
            .await
            .unwrap()
            .text(),
        "back"
    );
    let states: Vec<CircuitState> = health_events(&h)
        .iter()
        .map(|snapshots| state_of(snapshots, "ka"))
        .collect();
    assert_eq!(
        states,
        [
            CircuitState::Open,
            CircuitState::HalfOpen,
            CircuitState::Closed
        ]
    );
}
