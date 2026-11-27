#![forbid(unsafe_code)]

//! The LLM layer: everything about talking to language-model APIs and about being one.
//!
//! Everything lives under [`llm`] (`llm::client`, `llm::gateway`, `llm::router`, `llm::probe`,
//! …), which also re-exports the protocol codecs, the record types and rules, the store and the
//! operational adapters it builds on.
//!
//! Dependency direction: [`spanbay_base`], [`spanbay_model`], [`spanbay_store`],
//! [`spanbay_llm_ops`], [`spanbay_llm_domain`] and [`spanbay_llm_protocol`]. Nothing above it
//! may be named from here.

pub mod llm;
