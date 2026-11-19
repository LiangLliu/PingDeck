#![forbid(unsafe_code)]

//! Agent harness use-cases, agent tools over PingDeck data, and the aggregated AI config read.

pub mod agent_service;
pub mod agent_tools;
pub mod ai_config_service;

pub use agent_service::*;
pub use agent_tools::shell::{remove_session_temp_dir, sweep_session_temp_dirs, warm_login_path};
pub use ai_config_service::*;

pub(crate) use pingdeck_agent::agent;
#[cfg(test)]
pub(crate) use pingdeck_app::test_upstream;
pub(crate) use pingdeck_app::{app, secrets, workspace};
pub(crate) use pingdeck_base::{cancel, crypto, error, net_guard, platform};
pub(crate) use pingdeck_executor::executor;
pub(crate) use pingdeck_llm::llm;
pub(crate) use pingdeck_model::{events, settings};

/// The desktop shell reports whether the main window has focus (a core function, not an IPC
/// command). The core only keeps the state; readers decide what "nobody is looking" changes.
pub fn set_window_focused(state: &app::AppState, focused: bool) {
    state.agent.set_window_focused(focused);
}

/// Whether the main window has focus; `true` until the shell reported otherwise.
#[must_use]
pub fn window_focused(state: &app::AppState) -> bool {
    state.agent.window_focused()
}

/// The title of a live chat, for a system notification; `None` when it is unknown or deleted.
#[must_use]
pub fn agent_session_title_impl(state: &app::AppState, session_id: &str) -> Option<String> {
    state
        .store()
        .get_agent_session(session_id)
        .ok()
        .flatten()
        .filter(|session| session.deleted_at_ms.is_none())
        .map(|session| session.title)
}
