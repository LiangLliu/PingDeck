#![forbid(unsafe_code)]

//! Pure-Rust GPUI rendering shell for PingDeck.
//!
//! Strictly isolated from core business logic and persistence (`crates/pingdeck-core`,
//! `crates/pingdeck-desktop`), with `gpui-kit` components quarantined inside [`ui`].

pub mod app;
pub mod bridge;
pub mod platform;
pub mod ui;
pub mod views;

#[cfg(all(test, feature = "test-support"))]
mod ui_tests;
