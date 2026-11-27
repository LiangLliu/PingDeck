//! OS notifications while the main window is away from focus, decoupled from any GUI framework.
//!
//! Preserves the privacy invariant: titles name only category + chat name, and bodies name only
//! tool category + risk level — never commands, paths, or arguments.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use spanbay_core::{
    agent::attention::{AgentAttention, GateSignal, PendingApprovalSummary},
    agent::model::AgentRunStatus,
    app::{
        AppState, agent_attention_subscribe_impl, agent_session_title_impl, set_window_focused,
        settings_get_impl, window_focused,
    },
    logging::{LogLevel, log_line},
    model::events::{EventSink, Sequenced},
    model::settings::{Language, NotifyWhenAway},
};

use crate::i18n::tr;
use crate::runtime::DesktopRuntime;
use crate::services::ports::HostWindow;

const COALESCE_MS: u128 = 30_000;
const MAX_AWAITED: usize = 8;

static AWAITED: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlertKind {
    Approval { tool: ToolKind, risk: Risk },
    Plan,
    Question,
    Cost,
    Finished { failed: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Command,
    Files,
    Network,
    Mcp,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub session_id: String,
    pub kind: AlertKind,
}

fn tool_kind(tool: &str) -> ToolKind {
    match tool {
        t if t.starts_with("shell") => ToolKind::Command,
        "write_file" | "edit_file" | "delete_file" | "move_file" => ToolKind::Files,
        t if t.starts_with("mcp__") => ToolKind::Mcp,
        "fetch_url" | "web_search" | "send_request" => ToolKind::Network,
        _ => ToolKind::Other,
    }
}

fn risk(approval: &PendingApprovalSummary) -> Risk {
    match approval.risk.as_str() {
        "low" => Risk::Low,
        "high" => Risk::High,
        _ => Risk::Medium,
    }
}

/// Computes the notifications to raise for one snapshot (at most one per chat).
#[must_use]
pub fn plan(
    next: &AgentAttention,
    previous: Option<&AgentAttention>,
    focused: bool,
    mode: NotifyWhenAway,
) -> Vec<Alert> {
    let Some(previous) = previous else {
        return Vec::new();
    };
    if focused || mode == NotifyWhenAway::Off {
        return Vec::new();
    }
    let mut alerts: Vec<Alert> = Vec::new();
    let mut push = |session_id: &str, kind: AlertKind| {
        if !alerts.iter().any(|a| a.session_id == session_id) {
            alerts.push(Alert {
                session_id: session_id.to_owned(),
                kind,
            });
        }
    };
    for approval in next.approvals_since(previous) {
        let kind = AlertKind::Approval {
            tool: tool_kind(&approval.tool),
            risk: risk(approval),
        };
        push(&approval.session_id, kind);
    }
    for (session_id, signal) in next.gates_since(previous) {
        let kind = match signal {
            GateSignal::Plan => AlertKind::Plan,
            GateSignal::Question => AlertKind::Question,
            GateSignal::Cost => AlertKind::Cost,
        };
        push(session_id, kind);
    }
    if mode == NotifyWhenAway::GatesAndFinished {
        for run in next.long_runs_ended_since(previous) {
            let failed = run.status == AgentRunStatus::Failed;
            push(&run.session_id, AlertKind::Finished { failed });
        }
    }
    alerts
}

/// Keeps only alerts whose chat had no notification in the last 30 seconds.
pub fn coalesce(
    alerts: Vec<Alert>,
    sent: &mut HashMap<String, Instant>,
    now: Instant,
) -> Vec<Alert> {
    sent.retain(|_, at| now.duration_since(*at).as_millis() < COALESCE_MS);
    alerts
        .into_iter()
        .filter(|alert| {
            let fresh = !sent.contains_key(&alert.session_id);
            if fresh {
                sent.insert(alert.session_id.clone(), now);
            }
            fresh
        })
        .collect()
}

/// Formats `(title, body)` carrying only categories and chat name (never commands or paths).
#[must_use]
pub fn format_text(kind: &AlertKind, chat: &str, language: Language) -> (String, String) {
    let sh = tr(language).shell();
    match kind {
        AlertKind::Approval { tool, risk } => {
            let tool = match tool {
                ToolKind::Command => sh.notify_tool_command(),
                ToolKind::Files => sh.notify_tool_files(),
                ToolKind::Network => sh.notify_tool_network(),
                ToolKind::Mcp => sh.notify_tool_mcp(),
                ToolKind::Other => sh.notify_tool_other(),
            };
            let risk = match risk {
                Risk::Low => sh.notify_risk_low(),
                Risk::Medium => sh.notify_risk_medium(),
                Risk::High => sh.notify_risk_high(),
            };
            (
                sh.notify_approval_title(chat),
                sh.notify_approval_body(tool, risk),
            )
        }
        AlertKind::Plan => (sh.notify_plan_title(chat), sh.notify_plan_body().to_owned()),
        AlertKind::Question => (
            sh.notify_question_title(chat),
            sh.notify_question_body().to_owned(),
        ),
        AlertKind::Cost => (sh.notify_cost_title(chat), sh.notify_cost_body().to_owned()),
        AlertKind::Finished { failed } => (
            sh.notify_finished_title(chat),
            if *failed {
                sh.notify_finished_failed()
            } else {
                sh.notify_finished_completed()
            }
            .to_owned(),
        ),
    }
}

#[derive(Default)]
struct SinkState {
    previous: Option<AgentAttention>,
    sent: HashMap<String, Instant>,
}

struct NotifySink {
    state: AppState,
    window: Arc<dyn HostWindow>,
    inner: Mutex<SinkState>,
}

impl EventSink<Sequenced<AgentAttention>> for NotifySink {
    fn emit(&self, event: Sequenced<AgentAttention>) -> bool {
        let next = event.event;
        let focused = window_focused(&self.state);
        let mode = settings_get_impl(&self.state).general.notify_when_away;
        let alerts = {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            let planned = plan(&next, inner.previous.as_ref(), focused, mode);
            let coalesced = coalesce(planned, &mut inner.sent, Instant::now());
            inner.previous = Some(next);
            coalesced
        };
        for alert in alerts {
            let app_state = self.state.clone();
            let window = Arc::clone(&self.window);
            let spawned = std::thread::Builder::new()
                .name("spanbay-notify".to_owned())
                .spawn(move || deliver(&app_state, &window, &alert));
            if let Err(error) = spawned {
                log_line(
                    LogLevel::Warn,
                    "notify",
                    &format!("notification not shown: {error}"),
                );
            }
        }
        true
    }
}

fn deliver(state: &AppState, window: &Arc<dyn HostWindow>, alert: &Alert) {
    let language = settings_get_impl(state).general.language;
    let chat = agent_session_title_impl(state, &alert.session_id)
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| "SpanBay".to_owned());
    let (title, body) = format_text(&alert.kind, &chat, language);
    let mut notification = notify_rust::Notification::new();
    notification.summary(&title).body(&body);
    #[cfg(not(target_os = "macos"))]
    notification.action("default", "");
    let handle = match notification.show() {
        Ok(handle) => handle,
        Err(error) => {
            log_line(
                LogLevel::Warn,
                "notify",
                &format!("notification not shown: {error}"),
            );
            return;
        }
    };
    if AWAITED.fetch_add(1, Ordering::SeqCst) >= MAX_AWAITED {
        AWAITED.fetch_sub(1, Ordering::SeqCst);
        drop(handle);
        return;
    }
    handle.wait_for_action(|action| {
        if action == "default" {
            window.show_and_focus();
            window.request_open_chat(&alert.session_id);
        }
    });
    AWAITED.fetch_sub(1, Ordering::SeqCst);
}

/// Subscribes OS away-from-window notifications to the core attention feed. Never fatal.
pub fn start_notifications(
    runtime: &DesktopRuntime,
    window: Arc<dyn HostWindow>,
    bundle_identifier: &str,
) {
    #[cfg(target_os = "macos")]
    {
        let id = if cfg!(debug_assertions) {
            "com.apple.Terminal"
        } else {
            bundle_identifier
        };
        let _ = notify_rust::set_application(id);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = bundle_identifier;

    set_window_focused(runtime.state(), window.is_focused());
    let state = runtime.state_cloned();
    runtime.spawn_async(async move {
        let sink = Arc::new(NotifySink {
            state: state.clone(),
            window,
            inner: Mutex::new(SinkState::default()),
        });
        if let Err(error) = agent_attention_subscribe_impl(&state, None, sink) {
            log_line(
                LogLevel::Error,
                "notify",
                &format!("notifications did not start: {}", error.message()),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn approval(session: &str, call: &str) -> PendingApprovalSummary {
        PendingApprovalSummary {
            run_id: format!("run_{session}"),
            session_id: session.into(),
            call_id: call.into(),
            tool: "shell".into(),
            reason: String::new(),
            preview: None,
            requested_ms: 1,
            risk: "medium".into(),
            summary: "Run: rm -rf ./build".into(),
            full_text: Some("rm -rf ./build".into()),
            card: None,
        }
    }

    #[test]
    fn notification_text_never_leaks_command_or_paths() {
        let after =
            AgentAttention::assemble(vec![approval("chat_a", "c1")], Vec::new(), 1, 0, Vec::new());
        let alerts = plan(
            &after,
            Some(&AgentAttention::default()),
            false,
            NotifyWhenAway::Gates,
        );
        assert_eq!(alerts.len(), 1);
        for lang in [Language::ZhCn, Language::En] {
            let (title, body) = format_text(&alerts[0].kind, "排查 402", lang);
            assert!(!title.contains("rm -rf") && !body.contains("rm -rf"));
            assert!(!title.contains("./build") && !body.contains("./build"));
        }
    }

    #[test]
    fn coalesces_per_chat_for_thirty_seconds() {
        let mut sent = HashMap::new();
        let start = Instant::now();
        let alert = |s: &str| Alert {
            session_id: s.into(),
            kind: AlertKind::Finished { failed: false },
        };
        assert_eq!(coalesce(vec![alert("a")], &mut sent, start).len(), 1);
        assert_eq!(
            coalesce(
                vec![alert("a"), alert("b")],
                &mut sent,
                start + Duration::from_secs(29)
            )
            .len(),
            1
        );
        assert_eq!(
            coalesce(vec![alert("a")], &mut sent, start + Duration::from_secs(30)).len(),
            1
        );
    }
}
