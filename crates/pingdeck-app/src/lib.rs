#![forbid(unsafe_code)]

//! The composition root ([`app::AppState`]) and the request, workspace, Flow, Mock, settings and
//! system use-cases in [`app`]. The root re-exports are the module paths this crate's code uses;
//! [`secrets`] adds the SQLite secret store to the base trait.
//!
//! See `docs/architecture.md` §1–§3.

pub mod app;

pub use pingdeck_agent::agent;
pub use pingdeck_executor::{executor, oauth, scripts};
pub use pingdeck_llm::llm;
pub use pingdeck_model::{events, settings};
pub use pingdeck_workbench::{flows, importers, mock, postman, workspace};

pub use pingdeck_base::{cancel, crypto, error, logging, platform, proxy_url, regex};
pub use pingdeck_response::{bodies, response_viewer};

pub use pingdeck_store::storage;

/// The [`pingdeck_base::secrets::SecretStore`] trait plus the SQLite implementation.
pub mod secrets {
    pub use pingdeck_base::secrets::*;
    pub use pingdeck_store::secrets_sqlite::SqliteSecretStore;
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_upstream;
