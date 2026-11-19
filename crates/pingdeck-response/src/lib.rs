#![forbid(unsafe_code)]

//! Response bodies.
//!
//! * [`bodies`]: the bounded in-memory store that keeps a large body behind an opaque handle while
//!   results carry only a preview (`docs/architecture.md` §5.4);
//! * [`response_viewer`]: one body view format and the one JSONPath dialect that the response
//!   filter, request assertions, Flow extractions, Mock rules and balance queries share.
//!
//! Depends only on `pingdeck_base`; every crate from `pingdeck-model` up may depend on it.

pub mod bodies;
pub mod response_viewer;
