#![forbid(unsafe_code)]

//! The LLM wire-protocol codecs: the adapters between each chat and embeddings protocol and the
//! hub format, and the bounded SSE parser and frame writers.
//!
//! Invariants:
//! - Depends only on `pingdeck-base` and `pingdeck-model` among workspace crates.
//! - No network, file or async runtime: every codec is a pure transform over bytes and values.
//!
//! Everything lives under [`llm`] so paths line up with `pingdeck_llm::llm::*`, which re-exports
//! both modules: code moving between the crates keeps its `crate::llm::...` paths.

pub mod llm;
