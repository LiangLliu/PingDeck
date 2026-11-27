//! A model that streams slowly reaches the run's subscribers as it speaks: thinking and answer deltas leave the run
//! while the upstream is still sending, not together when the call ends.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Value, json};

use super::run::{chat, input, wait_until, world};
use super::upstream::Reply;
use crate::agent::dto::AgentSubscribeTarget;
use crate::agent::model::{AgentEnvelope, AgentEvent};
use crate::settings::ToolApproval;
use crate::test_upstream::sse;

use super::super::{agent_run_start_impl, agent_run_subscribe_impl};

const GAP_MS: u64 = 250;

fn frame(delta: Value, finish: Value) -> String {
    sse(
        &json!({"id": "c1", "object": "chat.completion.chunk", "created": 1, "model": "fake-model-1",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}),
    )
}

/// Silent for one gap, then three thinking pieces and three answer pieces, each a gap apart.
fn slow_reply() -> Reply {
    let mut frames = vec![(
        GAP_MS,
        frame(json!({"role": "assistant", "content": ""}), Value::Null),
    )];
    for piece in ["weigh ", "the ", "greeting"] {
        frames.push((
            GAP_MS,
            frame(json!({"reasoning_content": piece}), Value::Null),
        ));
    }
    for piece in ["Hello", "! How can ", "I help?"] {
        frames.push((GAP_MS, frame(json!({"content": piece}), Value::Null)));
    }
    frames.push((0, frame(json!({}), json!("stop"))));
    frames.push((0, "data: [DONE]\n\n".to_owned()));
    Reply::TimedSse(frames)
}

#[tokio::test(flavor = "multi_thread")]
async fn thinking_and_text_leave_the_run_while_the_upstream_is_still_sending() {
    let world = world(|_, _| slow_reply());
    let session = chat(&world, ToolApproval::AutoReadOnly, None);
    let arrivals: Arc<Mutex<Vec<(Instant, AgentEvent)>>> = Arc::default();
    let sink = {
        let arrivals = Arc::clone(&arrivals);
        move |envelope: AgentEnvelope| {
            arrivals
                .lock()
                .unwrap()
                .push((Instant::now(), envelope.event));
            true
        }
    };
    agent_run_subscribe_impl(
        &world.state,
        &AgentSubscribeTarget::Session {
            session_id: session.id.clone(),
        },
        None,
        Arc::new(sink),
    )
    .unwrap();
    agent_run_start_impl(&world.state, input(&session, "hello"))
        .await
        .unwrap();
    wait_until("the run's end", || {
        arrivals
            .lock()
            .unwrap()
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::Done { .. }))
    })
    .await;

    let seen = arrivals.lock().unwrap();
    let at = |pick: fn(&AgentEvent) -> bool| -> Vec<Instant> {
        seen.iter()
            .filter(|(_, e)| pick(e))
            .map(|(t, _)| *t)
            .collect()
    };
    let reasoning = at(|e| matches!(e, AgentEvent::ReasoningDelta { .. }));
    let text = at(|e| matches!(e, AgentEvent::TextDelta { .. }));
    let done = at(|e| matches!(e, AgentEvent::Done { .. }));
    assert_eq!(
        (reasoning.len(), text.len()),
        (3, 3),
        "every piece is its own event"
    );
    let spread = |times: &[Instant]| times[times.len() - 1].duration_since(times[0]).as_millis();
    // Each kind arrives spread over its upstream gaps (two gaps apart at least, with a wide margin for a slow machine).
    assert!(
        spread(&reasoning) >= u128::from(GAP_MS),
        "thinking arrived in one burst: {} ms",
        spread(&reasoning)
    );
    assert!(
        spread(&text) >= u128::from(GAP_MS),
        "the answer arrived in one burst: {} ms",
        spread(&text)
    );
    // The first piece of each leaves long before the run ends.
    assert!(done[0].duration_since(reasoning[0]).as_millis() >= u128::from(4 * GAP_MS));
    assert!(done[0].duration_since(text[0]).as_millis() >= u128::from(GAP_MS));
}
