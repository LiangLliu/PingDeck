#![forbid(unsafe_code)]

//! Foundation of SpanBay.
//!
//! A module lives here only when all three hold:
//!
//! 1. it needs no other workspace crate;
//! 2. it carries no product feature: it is a contract every layer must agree on, or a mechanism
//!    that knows no workspace, request, provider or agent concept;
//! 3. at least two other workspace crates call it, and no crate that all of its callers depend on
//!    owns the concern.
//!
//! What is here:
//!
//! * contracts: [`error`], [`secrets`];
//! * primitives: [`cancel`], [`crypto`], [`id`], [`time`];
//! * platform: [`platform`], [`logging`];
//! * network: [`net_guard`], [`proxy_url`], [`url_encode`];
//! * text: [`regex`], [`yaml`], [`diff`].
//!
//! I/O stays at the platform edge: owner-only files and the log, DNS resolution in the SSRF guard,
//! and the login-shell `PATH` probe in [`platform`].

pub mod cancel;
pub mod crypto;
pub mod diff;
pub mod error;
pub mod id;
pub mod logging;
pub mod net_guard;
pub mod platform;
pub mod proxy_url;
pub mod regex;
pub mod secrets;
pub mod time;
pub mod url_encode;
pub mod yaml;
