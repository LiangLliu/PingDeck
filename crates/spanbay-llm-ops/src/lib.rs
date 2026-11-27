#![forbid(unsafe_code)]

//! The LLM operations that need neither SQLite, the executor nor the gateway runtime: provider
//! sniffing, balance adapters, capture redaction, the SSRF-guarded HTTP transport and the timing
//! resolver. The persisted vocabulary and the pure rules on it come from `spanbay-llm-domain`.
//!
//! Invariants:
//! - Depends only on `spanbay-base`, `spanbay-model`, `spanbay-response`,
//!   `spanbay-llm-protocol` and `spanbay-llm-domain` among workspace crates.
//! - Network I/O happens in two places only: `safe_http` (the SSRF-guarded transport for
//!   user-typed hosts, injectable as a `SafeHttp` double) and `timing::TimedResolver` (a timing
//!   DNS resolver for clients the caller builds).
//! - File I/O happens only in `ca_bundle`.
//!
//! Everything lives under [`llm`] so paths line up with `spanbay_llm::llm::*`, which re-exports
//! most modules: code moving between the two crates keeps its `crate::llm::...` paths.

pub mod llm;
