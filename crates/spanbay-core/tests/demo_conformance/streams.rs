//! The LLM event stream: what a resume from an evicted position yields, and the subscriber cap.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use spanbay_core::app::{llm_subscribe_impl, llm_unsubscribe_impl};
use spanbay_core::llm::events::LlmEvent;
use spanbay_core::model::events::{EventSink, Sequenced};

use crate::{outcome, verify, world};

type Seen = Arc<Mutex<Vec<Value>>>;

fn sink(seen: &Seen) -> Arc<dyn EventSink<Sequenced<LlmEvent>>> {
    let seen = Arc::clone(seen);
    Arc::new(move |event: Sequenced<LlmEvent>| {
        seen.lock()
            .unwrap()
            .push(serde_json::to_value(&event).unwrap());
        true
    })
}

/// What a subscriber received: the first envelope (seq and event) and the replayed seq span.
fn summary(seen: &Seen) -> Value {
    let seen = seen.lock().unwrap();
    let replayed: Vec<&Value> = seen
        .iter()
        .filter(|e| e["event"]["kind"] != "Gap")
        .collect();
    json!({
        "gap": seen.first().filter(|e| e["event"]["kind"] == "Gap").map(|e| json!({ "seq": e["seq"], "first_seq": e["event"]["first_seq"] })),
        "replayed": replayed.len(),
        "first_replayed_seq": replayed.first().map(|e| e["seq"].clone()),
        "last_replayed_seq": replayed.last().map(|e| e["seq"].clone()),
    })
}

#[test]
fn subscribe_resume() {
    let emitted = 520_u64;
    let mut cases = Vec::new();
    for (name, from) in [
        ("live only", None),
        ("from an evicted position", Some(0_u64)),
        ("from inside the ring", Some(emitted - 10)),
    ] {
        let w = world("resume");
        for _ in 0..emitted {
            w.state.llm().emit(LlmEvent::ProvidersChanged);
        }
        let seen = Seen::default();
        let id = llm_subscribe_impl(&w.state, sink(&seen), from).unwrap();
        llm_unsubscribe_impl(&w.state, id);
        cases.push(json!({ "name": name, "from": from, "expected": summary(&seen) }));
    }
    let note = "Emit `emitted` events (seq 1 upwards) on a fresh stream, then subscribe with `from` (null = live only): an evicted position first yields a Gap envelope, then the remembered ring.";
    verify(
        "llm-subscribe-resume",
        note,
        json!({ "emitted": emitted }),
        cases,
    );
}

#[test]
fn subscribe_cap() {
    let w = world("cap");
    let seen = Seen::default();
    let mut ids = Vec::new();
    let mut cases = Vec::new();
    let mut accepted = 0;
    loop {
        match llm_subscribe_impl(&w.state, sink(&seen), None) {
            Ok(id) => {
                ids.push(id);
                accepted += 1;
            }
            Err(error) => {
                cases.push(json!({ "name": "live subscribers until refused", "expected": { "accepted": accepted, "refused": outcome::<()>(Err(error), |()| Value::Null) } }));
                break;
            }
        }
        assert!(accepted <= 1024, "the stream has no subscriber cap");
    }
    llm_unsubscribe_impl(&w.state, ids[0]);
    let again = llm_subscribe_impl(&w.state, sink(&seen), None);
    cases.push(json!({ "name": "an unsubscribe frees a slot", "expected": outcome(again, |_| json!("subscribed")) }));
    verify(
        "llm-subscribe-cap",
        "Subscribe live until the stream refuses, then unsubscribe one and subscribe again.",
        Value::Null,
        cases,
    );
}
