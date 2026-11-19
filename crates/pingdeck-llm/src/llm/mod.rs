//! LLM providers, model testing and the local gateway.
//!
//! Single responsibility: everything about talking to language-model APIs. The [`hub`] format
//! decouples the wire protocols; [`adapters`] convert each protocol to and from it; the
//! gateway and the client API are built on top.

pub use pingdeck_llm_domain::llm::{
    balance_state, capture_record, failure, health, models_dev, presets, providers, sniff_summary,
    status, usage,
};
pub use pingdeck_llm_ops::llm::{balance, ca_bundle, capture, safe_http, sniff};
pub use pingdeck_llm_protocol::llm::{adapters, sse};
pub use pingdeck_model::{capabilities, hub, model_catalog, wire};
pub use pingdeck_store::llm::{catalog, store};

pub mod ai_run;
pub mod client;
pub mod client_watch;
pub mod events;
pub mod failover;
pub mod gateway;
pub mod key_book;
pub mod probe;
pub mod router;
pub mod runtime;
pub mod services;
pub mod upstream;

#[cfg(test)]
mod client_badbody_tests;
#[cfg(test)]
mod client_tests;
