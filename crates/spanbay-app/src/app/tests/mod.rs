//! Application-service test suite: every use-case runs against an in-memory
//! store, so the whole SpanBay core is exercised without a UI shell.

mod builders;
mod environments;
mod execution;
mod history_cookies;
mod import_export;
mod items;
mod oauth;
mod reimport;
mod secret_hosts;
mod settings;
mod support;
mod tree;
mod workspace;
