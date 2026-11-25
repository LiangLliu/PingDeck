#![forbid(unsafe_code)]

//! Desktop runtime assembly and the single dependency of the Tauri shell.
//!
//! * [`lifecycle`] — the post-paint startup phase and shutdown, which span the use-case crates
//!   ([`AppLifecycleExt`]); the composition root itself is `pingdeck_app::app::AppState`;
//! * [`app`] — the use-case entry points the IPC commands call (`*_impl`), merged from
//!   `pingdeck-app`, `pingdeck-app-llm`, `pingdeck-app-agent` and `pingdeck-app-sync`;
//! * every other path re-exports one module of another crate ([`model`] is the whole shared
//!   vocabulary crate), so `src-tauri` reaches every type through one dependency. [`llm`] and
//!   [`agent`] are the facades of their crates: besides their own code they re-export the model,
//!   store and LLM leaf modules they build on (`llm::wire`, `llm::store`, `agent::model`), so a
//!   type owned by `pingdeck-model` can be named through them as well as through [`model`].
//!
//! The cross-crate integration tests (`ipc_contract`, `demo_conformance`, `gateway_latency`,
//! `system`) live in `tests/`. See `docs/architecture.md` §2–§3.

pub mod lifecycle;
pub use lifecycle::{AppLifecycleExt, BootstrapReport};

/// The use-case entry points the IPC commands call, merged from `pingdeck-app`,
/// `pingdeck-app-llm`, `pingdeck-app-agent` and `pingdeck-app-sync`.
pub mod app {
    pub use pingdeck_app::app::*;
    pub use pingdeck_app_agent::*;
    pub use pingdeck_app_llm::*;
    pub use pingdeck_app_sync::{
        SyncFolderInfoDto, SyncFolderKind, SyncRemoveSecretsResult, inspect_sync_folder_impl,
        remove_secrets_from_folder_impl, sync_export_impl, sync_export_with_impl,
        sync_folder_risk_impl, sync_import_impl, sync_preview_impl, sync_remove_secrets_impl,
        sync_status_impl,
    };
}

pub use pingdeck_agent::agent;
pub use pingdeck_app_sync::sync;
pub use pingdeck_base::{error, logging, platform, proxy_url, regex, secrets};
/// AI coding tool config writers.
pub use pingdeck_client_config as client_config;
pub use pingdeck_executor::executor;
pub use pingdeck_llm::llm;
pub use pingdeck_model as model;
pub use pingdeck_response::response_viewer;
pub use pingdeck_store::storage;
pub use pingdeck_workbench::{flows, mock};
