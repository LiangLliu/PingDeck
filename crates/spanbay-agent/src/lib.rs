#![forbid(unsafe_code)]

//! The agent harness: everything between a chat and the tools it calls.
//!
//! Everything lives under [`agent`] (`agent::runner`, `agent::tools`, `agent::permissions`,
//! `agent::mcp`, `agent::untrusted`, …).
//!
//! The tools that are use-cases over SpanBay's own data (send, trace, probe, gateway log,
//! attachments, sub-agents, todo, web search, skill, shell) live in `spanbay-app-agent`: they
//! need the application state, which this crate must not name. The contract they implement, and
//! the tools that need nothing but it (file and process tools), are here.
//!
//! Dependency direction: [`spanbay_base`], [`spanbay_model`], [`spanbay_store`] and
//! [`spanbay_llm`]. Nothing above it may be named from here.

pub mod agent;
