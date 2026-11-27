#![forbid(unsafe_code)]

//! Agent harness use-cases, agent tools over SpanBay data, and the aggregated AI config read.

pub mod agent_service;
pub mod agent_tools;
pub mod ai_config_service;

pub use agent_service::*;
pub use agent_tools::shell::{remove_session_temp_dir, sweep_session_temp_dirs, warm_login_path};
pub use ai_config_service::*;

pub(crate) use spanbay_agent::agent;
#[cfg(test)]
pub(crate) use spanbay_app::test_upstream;
pub(crate) use spanbay_app::{app, secrets, workspace};
pub(crate) use spanbay_base::{cancel, crypto, error, net_guard, platform};
pub(crate) use spanbay_executor::executor;
pub(crate) use spanbay_llm::llm;
pub(crate) use spanbay_model::{events, settings};

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
