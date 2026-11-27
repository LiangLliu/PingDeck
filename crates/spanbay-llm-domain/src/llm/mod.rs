//! Persisted LLM records and the pure rules on them.
//!
//! Module map:
//! - Providers: [`providers`], [`presets`], [`models_dev`], [`catalog_grouping`].
//! - Health and status: [`failure`], [`health`], [`status`], [`probe`].
//! - Records and usage: [`records`], [`usage`].
//! - Stored shapes of the operations: [`balance_state`] (balance readings and custom queries),
//!   [`capture_record`] (stored captures and timings), [`sniff_summary`] (what a saved provider
//!   keeps of its last scan).

pub(crate) use spanbay_client_config as client_config;
pub(crate) use spanbay_model::{capabilities, hub, wire};

pub mod balance_state;
pub mod capture_record;
pub mod catalog_grouping;
pub mod failure;
pub mod health;
pub mod models_dev;
pub mod presets;
pub mod probe;
pub mod providers;
pub mod records;
pub mod sniff_summary;
pub mod status;
pub mod usage;
