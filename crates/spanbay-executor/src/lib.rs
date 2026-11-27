#![forbid(unsafe_code)]

//! The request pipeline and everything it needs to run one request:
//!
//! * [`executor`] — render, auth, body, client, cookies, retry, redirect, trace, cancel and
//!   the [`executor::ExecutionResult`] every caller stores;
//! * [`scripts`] — the Boa sandbox and the Postman `pm.*` API (its `pm.sendRequest` bridge
//!   calls back into this crate, which is why the two live together);
//! * [`assertions`] — response assertions;
//! * [`oauth`] — the OAuth2 token flow and its cache.
//!
//! Dependency direction: [`spanbay_base`], [`spanbay_response`] and [`spanbay_model`].
//! Nothing above it may be named from here.

pub mod assertions;
mod error_convert;
pub mod executor;
mod json_schema;
pub mod oauth;
pub mod scripts;
#[cfg(any(test, feature = "test-support"))]
pub mod test_server;
