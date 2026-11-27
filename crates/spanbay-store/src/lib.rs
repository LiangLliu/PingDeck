#![forbid(unsafe_code)]

//! The one crate that owns SQLite.
//!
//! * [`storage`] — connection pool, baseline DDL, workspace/agent accessors;
//! * [`llm`] — providers, keys, models, catalog, pricing, usage, request logs, probes;
//! * [`secrets_sqlite`] — [`spanbay_base::secrets::SecretStore`] over the `secrets` table;
//! * [`attachments`] — content-addressed blob store under the data directory.

/// The SQLite connection handle passed across repository boundaries. Other crates name the
/// connection type through this alias, because only `spanbay-store` depends on `rusqlite`.
pub type DbConnection = rusqlite::Connection;

pub mod attachments;
mod error_convert;
pub mod llm;
pub mod secrets_sqlite;
mod sqlite;
pub mod storage;
