//! Declarative GPUI feature views covering all 10 navigation modules + shell overlays.
//!
//! Strictly isolated from `gpui_kit::component::*` and `pingdeck_core::storage`
//! (verified by `cargo xtask check-arch`).

pub mod chat;
pub mod environments;
pub mod flows;
pub mod gateway;
pub mod history;
pub mod mock;
pub mod providers;
pub mod requests;
pub mod settings;
pub mod shell;
pub mod usage;
pub mod utility;
