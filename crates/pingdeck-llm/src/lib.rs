#![forbid(unsafe_code)]

//! The LLM layer: everything about talking to language-model APIs and about being one.
//!
//! Everything lives under [`llm`] (`llm::client`, `llm::gateway`, `llm::router`, `llm::probe`,
//! …), which also re-exports the protocol codecs, the record types and rules, the store and the
//! operational adapters it builds on.
//!
//! Dependency direction: [`pingdeck_base`], [`pingdeck_model`], [`pingdeck_store`],
//! [`pingdeck_llm_ops`], [`pingdeck_llm_domain`] and [`pingdeck_llm_protocol`]. Nothing above it
//! may be named from here.

pub mod llm;
