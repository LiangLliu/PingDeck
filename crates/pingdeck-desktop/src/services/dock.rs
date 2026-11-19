//! Dock approval badge and attention bounce planner, decoupled from any GUI framework.

use std::sync::{Arc, Mutex, PoisonError};

use pingdeck_core::{
    agent::attention::AgentAttention,
    app::{AppState, agent_attention_subscribe_impl, settings_get_impl},
    logging::{LogLevel, log_line},
    model::events::{EventSink, Sequenced},
    model::settings::{GeneralSettings, NotifyWhenAway},
};

use crate::runtime::DesktopRuntime;
use crate::services::ports::{HostDock, HostWindow};

/// What to apply to the OS dock for a single attention snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DockPlan {
    pub badge: Option<i64>,
    pub set_badge: bool,
    pub bounces: usize,
}

/// Maps a snapshot to a [`DockPlan`]. The first snapshot (`previous` is `None`) establishes a
/// quiet baseline: it sets the badge without bouncing.
#[must_use]
pub fn plan(
    next: &AgentAttention,
    previous: Option<&AgentAttention>,
    focused: bool,
    bounce_enabled: bool,
) -> DockPlan {
    let bounces = match previous {
        Some(previous) if !focused && bounce_enabled => next.signals_since(previous).len(),
        _ => 0,
    };
    DockPlan {
        badge: (next.waiting_approval > 0).then_some(i64::from(next.waiting_approval)),
        set_badge: previous.is_none_or(|p| p.waiting_approval != next.waiting_approval),
        bounces,
    }
}

/// `notify_when_away = off` silences dock bounces as well.
#[must_use]
pub fn bounce_allowed(general: &GeneralSettings) -> bool {
    general.dock_bounce && general.notify_when_away != NotifyWhenAway::Off
}

struct DockSink {
    state: AppState,
    window: Arc<dyn HostWindow>,
    dock: Arc<dyn HostDock>,
    previous: Mutex<Option<AgentAttention>>,
}

impl EventSink<Sequenced<AgentAttention>> for DockSink {
    fn emit(&self, event: Sequenced<AgentAttention>) -> bool {
        let next = event.event;
        let mut previous = self.previous.lock().unwrap_or_else(PoisonError::into_inner);
        let bounce_enabled = bounce_allowed(&settings_get_impl(&self.state).general);
        let dock_plan = plan(
            &next,
            previous.as_ref(),
            self.window.is_focused(),
            bounce_enabled,
        );
        if dock_plan.set_badge {
            self.dock.set_badge_count(dock_plan.badge);
        }
        for _ in 0..dock_plan.bounces {
            self.dock.request_informational_attention();
        }
        *previous = Some(next);
        true
    }
}

/// Subscribes the OS dock controller to the core's `AgentAttention` feed. Never fatal.
pub fn start_dock(runtime: &DesktopRuntime, window: Arc<dyn HostWindow>, dock: Arc<dyn HostDock>) {
    let state = runtime.state_cloned();
    runtime.spawn_async(async move {
        let sink = Arc::new(DockSink {
            state: state.clone(),
            window,
            dock,
            previous: Mutex::new(None),
        });
        if let Err(error) = agent_attention_subscribe_impl(&state, None, sink) {
            log_line(
                LogLevel::Error,
                "dock",
                &format!("dock attention subscription failed: {}", error.message()),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use pingdeck_core::agent::attention::PendingApprovalSummary;

    use super::*;

    fn approval(id: &str) -> PendingApprovalSummary {
        PendingApprovalSummary {
            run_id: "r1".into(),
            session_id: "s1".into(),
            call_id: id.into(),
            tool: "shell".into(),
            reason: String::new(),
            preview: None,
            requested_ms: 1,
            risk: "medium".into(),
            summary: "cmd".into(),
            full_text: None,
            card: None,
        }
    }

    #[test]
    fn baseline_sets_badge_quietly_and_new_signal_bounces_when_away() {
        let one = AgentAttention::assemble(vec![approval("c1")], Vec::new(), 1, 0, Vec::new());
        let first = plan(&one, None, false, true);
        assert_eq!(first.badge, Some(1));
        assert!(first.set_badge);
        assert_eq!(first.bounces, 0);

        let two = AgentAttention::assemble(
            vec![approval("c1"), approval("c2")],
            Vec::new(),
            1,
            0,
            Vec::new(),
        );
        let away = plan(&two, Some(&one), false, true);
        assert_eq!(away.badge, Some(2));
        assert_eq!(away.bounces, 1);

        let focused = plan(&two, Some(&one), true, true);
        assert_eq!(focused.bounces, 0);
    }
}
