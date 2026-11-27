#![forbid(unsafe_code)]

//! Config sync use-cases: the sync file format, merge rules, folder export and import, and
//! secret clean-up.
//!
//! Code reaches shared modules as `crate::<module>`; the aliases below name the crate each one is
//! reached through. `workspace` is the workbench import path over model, executor and store, and
//! `llm` and `agent` are the facades of their crates. The sync runtime `AppState` holds lives in `spanbay_app::app::sync_runtime`;
//! [`attach_auto_export`] installs the export job into it.

pub mod sync;
pub mod sync_service;

pub use sync::attach_auto_export;
pub use sync_service::*;

pub(crate) use spanbay_agent::agent;
pub(crate) use spanbay_app::app;
pub(crate) use spanbay_base::{crypto, error, platform, secrets};
pub(crate) use spanbay_llm::llm;
#[cfg(test)]
pub(crate) use spanbay_model::events;
pub(crate) use spanbay_model::settings;
pub(crate) use spanbay_workbench::workspace;
