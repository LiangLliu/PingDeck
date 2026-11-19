#![forbid(unsafe_code)]

//! Persisted LLM vocabulary and the pure rules on it: gateway and provider records, provider
//! validation, presets, probe planning and apply rules, health, failure and status rules, usage
//! pricing, catalog grouping, models.dev parsing, and the stored shapes of balance readings,
//! captures and scan summaries. Both the store and the LLM operations are written against it.
//!
//! Invariants:
//! - Depends only on `pingdeck-base`, `pingdeck-model`, `pingdeck-response` and
//!   `pingdeck-client-config` among workspace crates.
//! - No SQLite: the store persists these types.
//! - Network I/O happens in one place only: `models_dev::download`, which fetches the fixed
//!   models.dev catalog with a caller-supplied `reqwest::Client`.
//!
//! Everything lives under [`llm`] so `crate::llm::*` paths line up with `pingdeck-llm-ops` and the
//! `pingdeck_llm::llm` facade.

pub mod llm;
