//! The agent tools that are use-cases over PingDeck's own data: they need the whole
//! [`AppState`], so they live above the agent engine and reach it through the context's
//! type-erased host handle.

use crate::app::AppState;

pub mod attachment;
pub mod pingdeck;
pub mod read_output;
pub mod shell;
pub mod skill;
pub mod subagent;
pub mod todo;
pub mod web;

/// The application handle the context carries (always set by `RunHost::context`).
#[allow(clippy::expect_used)] // the host is always set for these tools; a Result would ripple into every call site for a case that cannot happen
pub(crate) fn app_state<'a>(ctx: &'a crate::agent::tools::ToolContext<'_>) -> &'a AppState {
    ctx.host
        .and_then(|host| host.downcast_ref::<AppState>())
        .expect("a tool that needs the app state is only called from a run that has one")
}
