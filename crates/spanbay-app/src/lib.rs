#![forbid(unsafe_code)]

//! The composition root ([`app::AppState`]) and the request, workspace, Flow, Mock, settings and
//! system use-cases in [`app`]. The root re-exports are the module paths this crate's code uses;
//! [`secrets`] adds the SQLite secret store to the base trait.
//!
//! See `docs/architecture.md` §1–§3.

pub mod app;

pub use spanbay_agent::agent;
pub use spanbay_executor::{executor, oauth, scripts};
pub use spanbay_llm::llm;
pub use spanbay_model::{events, settings};
pub use spanbay_workbench::{flows, importers, mock, postman, workspace};

pub use spanbay_base::{cancel, crypto, error, logging, platform, proxy_url, regex};
pub use spanbay_response::{bodies, response_viewer};

pub use spanbay_store::storage;

/// The [`spanbay_base::secrets::SecretStore`] trait plus the SQLite implementation.
pub mod secrets {
    pub use spanbay_base::secrets::*;
    pub use spanbay_store::secrets_sqlite::SqliteSecretStore;
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_upstream;
