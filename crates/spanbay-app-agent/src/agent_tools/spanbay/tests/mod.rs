//! Tests of the SpanBay-native agent tools: schemas, classification, redaction, scope, sends,
//! traces, listings, probes, the gateway log and request creation. No network: every upstream is
//! a local server on 127.0.0.1:0.

mod create;
mod read;
mod scoped;
mod send;
mod support;
