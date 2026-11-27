//! The push events of the LLM layer and the hub that numbers and replays them.
//!
//! Single responsibility: define [`LlmEvent`], the one event enum the shell subscribes to for
//! providers, keys, models, aliases, balances, health and the gateway, and the hub constructor.
//! Producers call [`crate::llm::runtime::LlmRuntime::emit`]; consumers subscribe through
//! [`crate::llm::runtime::LlmRuntime::events`] and get replay-without-gaps from [`EventHub`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::llm::balance_state::BalanceReport;
use crate::llm::gateway::{GatewayNoticeCode, GatewayStatus};
use crate::llm::health::{HealthSnapshot, Permit};
use crate::llm::runtime::LlmRuntime;
use crate::llm::store::RequestLogEntry;
use spanbay_model::events::EventHub;

/// Most simultaneous subscribers of the hub.
const MAX_SUBSCRIBERS: usize = 16;
/// Events the replay ring keeps.
const RING_EVENTS: usize = 512;
/// Approximate bytes the replay ring keeps.
const RING_BYTES: usize = 4 * 1024 * 1024;

/// One push event of the LLM layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "PascalCase")]
// Transient push values: a boxed status or batch would only add an allocation per event.
#[allow(clippy::large_enum_variant)]
pub enum LlmEvent {
    /// A provider was added, edited, removed or reordered.
    ProvidersChanged,
    /// A provider's keys changed.
    KeysChanged {
        /// The provider.
        provider_id: String,
    },
    /// A provider's model list changed.
    ModelsChanged {
        /// The provider.
        provider_id: String,
    },
    /// An alias was added, edited or removed.
    AliasesChanged,
    /// A balance query finished.
    BalanceUpdated {
        /// The provider.
        provider_id: String,
        /// The key the balance belongs to.
        key_id: String,
        /// What the provider answered.
        report: BalanceReport,
    },
    /// A circuit changed state (opened, half-opened, closed); carries every pair's figures.
    HealthChanged {
        /// The new figures of every `(key, model)` pair.
        snapshots: Vec<HealthSnapshot>,
    },
    /// The gateway changed state (also carries the one-shot `notice`).
    GatewayStatus {
        /// The status after the change.
        status: GatewayStatus,
    },
    /// Why the gateway changed on its own or needs attention (LAN switched off, a failed start,
    /// an upgrade note). The UI keeps it on screen until the state recovers; the status's
    /// `notice` still carries the same words.
    GatewayNotice {
        /// What the notice is about.
        code: GatewayNoticeCode,
        /// The core's words (English), for logs and an unknown code.
        notice: String,
    },
    /// Request-log rows stored since the previous batch, oldest first (at most one batch per
    /// 100 ms).
    GatewayLogBatch {
        /// The stored rows.
        entries: Vec<RequestLogEntry>,
    },
    /// The model catalog changed (a manual or automatic models.dev sync applied): refetch the
    /// catalog status and the price views.
    CatalogChanged,
    /// A provider's derived status (sniff, probe or balance outcome) changed.
    ProviderStatusChanged {
        /// The provider.
        provider_id: String,
    },
    /// A gateway client changed: created, edited, rotated, revoked, deleted, connected, or its
    /// `last_used_ms`, "Verified" instant or today's request count moved (the last two at most
    /// every 5 s). Carries nothing: refetch `llm_gateway_client_list` and the overview.
    ClientsChanged,
    /// Environments or the globals of a workspace were written (`source`: `ui`, `oauth`,
    /// `sync`). Rides this hub because it is the shell's one always-open change stream; carries
    /// ids only (the workspace id stands for its globals): refetch `environment_list`.
    EnvironmentsChanged {
        /// The workspace written.
        workspace_id: String,
        /// The environments written; the workspace id when the globals were.
        ids: Vec<String>,
        /// Who wrote: `ui`, `oauth` or `sync`.
        source: String,
    },
    /// Events before `first_seq` were evicted from the replay ring: sent first to a resubscriber
    /// whose `from_seq` is older, ahead of the replay; reload persisted state.
    Gap {
        /// The oldest sequence number still replayed.
        first_seq: u64,
    },
}

/// The hub of [`LlmEvent`]s.
pub type LlmEventHub = EventHub<LlmEvent>;

/// Rough size of one event, so the replay ring stays bounded when log batches are large.
fn size_of(event: &LlmEvent) -> usize {
    match event {
        LlmEvent::GatewayLogBatch { entries } => 256 + entries.len() * 1024,
        LlmEvent::HealthChanged { snapshots } => 256 + snapshots.len() * 192,
        _ => 512,
    }
}

/// A fresh hub with the layer's subscriber and replay limits.
#[must_use]
pub fn new_hub() -> LlmEventHub {
    EventHub::with_limits(MAX_SUBSCRIBERS, RING_EVENTS, RING_BYTES, size_of, None)
}

/// Takes the permit for `(key, model)` from the health book and announces a circuit that the call
/// turned half-open.
pub(crate) fn acquire_announcing(runtime: &LlmRuntime, key_id: &str, model_id: &str) -> Permit {
    let (permit, half_opened) = runtime.health.acquire_noting(key_id, model_id);
    if half_opened {
        announce_health(runtime);
    }
    permit
}

/// Pushes [`LlmEvent::HealthChanged`] with the figures of every pair. Callers do it on a real
/// circuit transition only (open, half-open, close), which keeps it rare.
pub(crate) fn announce_health(runtime: &LlmRuntime) {
    runtime.emit(LlmEvent::HealthChanged {
        snapshots: runtime.health.snapshot(),
    });
}

/// How often `KeysChanged` may be announced for one provider while keys cool down and recover.
const KEYS_CHANGED_WINDOW: Duration = Duration::from_secs(1);

/// What [`KeysChangedThrottle::poke`] tells the caller to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Poke {
    /// Announce now.
    Now,
    /// Announce after this delay (then call [`KeysChangedThrottle::fire`]).
    Later(Duration),
    /// An announcement is already scheduled; it will carry this change.
    Merged,
}

#[derive(Debug, Default)]
struct Slot {
    last: Option<Instant>,
    trailing: bool,
}

/// At most one `KeysChanged` per provider per second, and the last change is always announced:
/// run-state changes of keys (cooldowns starting and ending) come in bursts.
#[derive(Debug, Default)]
pub struct KeysChangedThrottle {
    slots: Mutex<HashMap<String, Slot>>,
}

impl KeysChangedThrottle {
    /// Notes a change of `provider_id`'s keys at `now`.
    pub fn poke(&self, provider_id: &str, now: Instant) -> Poke {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = slots.entry(provider_id.to_owned()).or_default();
        if slot.trailing {
            return Poke::Merged;
        }
        match slot.last.map(|last| now.saturating_duration_since(last)) {
            Some(since) if since < KEYS_CHANGED_WINDOW => {
                slot.trailing = true;
                Poke::Later(KEYS_CHANGED_WINDOW.saturating_sub(since))
            }
            _ => {
                slot.last = Some(now);
                Poke::Now
            }
        }
    }

    /// The scheduled announcement of `provider_id` goes out at `now`.
    pub fn fire(&self, provider_id: &str, now: Instant) {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = slots.entry(provider_id.to_owned()).or_default();
        slot.trailing = false;
        slot.last = Some(now);
    }
}

/// Announces that `provider_id`'s keys changed run state, throttled by the runtime's
/// [`KeysChangedThrottle`]. Without an ambient Tokio runtime a late announcement goes out now.
pub(crate) fn keys_changed_soon(runtime: &Arc<LlmRuntime>, provider_id: &str) {
    let emit = |runtime: &LlmRuntime, provider_id: String| {
        runtime.emit(LlmEvent::KeysChanged { provider_id });
    };
    match runtime.key_events.poke(provider_id, Instant::now()) {
        Poke::Now => emit(runtime, provider_id.to_owned()),
        Poke::Merged => {}
        Poke::Later(delay) => {
            let (runtime, provider_id) = (Arc::clone(runtime), provider_id.to_owned());
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        tokio::time::sleep(delay).await;
                        runtime.key_events.fire(&provider_id, Instant::now());
                        emit(&runtime, provider_id);
                    });
                }
                Err(_) => {
                    runtime.key_events.fire(&provider_id, Instant::now());
                    emit(&runtime, provider_id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use spanbay_model::events::{EventSink, Sequenced};

    #[test]
    fn a_late_subscriber_replays_the_ring_in_order_and_then_hears_live_events() {
        let hub = new_hub();
        hub.emit(LlmEvent::ProvidersChanged);
        hub.emit(LlmEvent::AliasesChanged);
        let seen: Arc<Mutex<Vec<Sequenced<LlmEvent>>>> = Arc::default();
        let sink_seen = Arc::clone(&seen);
        let sink: Arc<dyn EventSink<Sequenced<LlmEvent>>> =
            Arc::new(move |event: Sequenced<LlmEvent>| {
                sink_seen.lock().unwrap().push(event);
                true
            });
        let id = hub.subscribe(Some(1), sink).unwrap();
        hub.emit(LlmEvent::ProvidersChanged);
        let seen = seen.lock().unwrap();
        let kinds: Vec<(u64, &LlmEvent)> = seen.iter().map(|e| (e.seq, &e.event)).collect();
        assert_eq!(
            kinds,
            [
                (2, &LlmEvent::AliasesChanged),
                (3, &LlmEvent::ProvidersChanged)
            ]
        );
        assert!(hub.unsubscribe(id));
    }

    #[test]
    fn a_gateway_notice_is_numbered_like_any_event_and_keeps_its_wire_shape() {
        let hub = new_hub();
        hub.emit(LlmEvent::ProvidersChanged);
        let notice = LlmEvent::GatewayNotice {
            code: GatewayNoticeCode::LanOff,
            notice: "Local network turned off: no active clients".to_owned(),
        };
        hub.emit(notice.clone());
        let seen: Arc<Mutex<Vec<Sequenced<LlmEvent>>>> = Arc::default();
        let sink_seen = Arc::clone(&seen);
        let sink: Arc<dyn EventSink<Sequenced<LlmEvent>>> =
            Arc::new(move |event: Sequenced<LlmEvent>| {
                sink_seen.lock().unwrap().push(event);
                true
            });
        hub.subscribe(Some(1), sink).unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!((seen[0].seq, &seen[0].event), (2, &notice));
        assert_eq!(
            serde_json::to_value(&notice).unwrap(),
            serde_json::json!({"kind": "GatewayNotice", "code": "lan_off", "notice": "Local network turned off: no active clients"})
        );
    }

    #[test]
    fn ten_key_changes_within_a_second_announce_twice_and_the_last_one_is_kept() {
        let throttle = KeysChangedThrottle::default();
        let start = Instant::now();
        let mut now_count = 0;
        let mut later = Vec::new();
        for step in 0..10_u64 {
            match throttle.poke("p", start + Duration::from_millis(step * 90)) {
                Poke::Now => now_count += 1,
                Poke::Later(delay) => later.push(delay),
                Poke::Merged => {}
            }
        }
        assert_eq!((now_count, later.len()), (1, 1));
        assert_eq!(later[0], Duration::from_millis(910));
        // The trailing announcement fires; the next change waits out a fresh window.
        throttle.fire("p", start + Duration::from_secs(1));
        assert_eq!(
            throttle.poke("p", start + Duration::from_millis(1_500)),
            Poke::Later(Duration::from_millis(500))
        );
        assert_eq!(
            throttle.poke("q", start),
            Poke::Now,
            "providers are throttled apart"
        );
    }
}
