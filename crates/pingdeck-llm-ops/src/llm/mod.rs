//! LLM provider operations: sniffing, balance adapters, capture redaction and timing, and the
//! guarded transport.
//!
//! Module map:
//! - Protocols: [`adapters`] and [`sse`] come from `pingdeck-llm-protocol`.
//! - Providers: [`sniff`], [`balance`]; the provider records, presets, probe rules, health,
//!   failure and status rules, usage pricing and persisted record types come from
//!   `pingdeck-llm-domain`.
//! - Captures: [`capture`].
//! - Transport: [`safe_http`], [`timing`], [`ca_bundle`].

pub(crate) use pingdeck_llm_domain::llm::{failure, health, presets, probe, providers};
pub(crate) use pingdeck_llm_protocol::llm::{adapters, sse};
pub(crate) use pingdeck_model::{capabilities, hub, wire};

pub mod balance;
pub mod ca_bundle;
pub mod capture;
pub mod safe_http;
pub mod sniff;
pub mod timing;
