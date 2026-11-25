#![forbid(unsafe_code)]

//! UI-framework-agnostic desktop host and headless presenter layer for PingDeck.
//!
//! Strictly isolated from any concrete GUI toolkit (no `gpui*`, no `tauri*`, no `objc2*`):
//! - [`runtime`]: data-directory locking, logging, SQLite open/set-aside, shared Tokio runtime,
//!   post-paint background startup, and graceful shutdown;
//! - [`streams`]: Tokio-scheduled delta coalescing and auto-disconnecting typed `EventSink`
//!   channels covering every push stream exposed by `pingdeck-core`;
//! - [`services`]: OS host trait ports (`HostWindow`, `HostDock`, `HostDialog`) plus pure-Rust
//!   service engines (`ImportGrants`, `DockSink`, `NotifySink`, `tray`, `clipboard`, `dialog`);
//! - [`theme`]: OKLCH gamut-mapped color compiler loading `tokens/tokens.json` into 16 eye-care
//!   [`theme::ResolvedPalette`] variants;
//! - [`i18n`]: compile-time embedded bilingual (`zh-CN` / `en`) catalogue with interpolation and
//!   pluralisation;
//! - [`persistence`]: atomic `0600` local UI-state and chat-draft storage under `DataDirs::root`;
//! - [`state`]: headless ViewModels, navigation routes, command palette engine, and module
//!   controllers consumed by any thin UI renderer.

pub use pingdeck_core as core;

pub mod i18n;
pub mod persistence;
pub mod runtime;
pub mod services;
pub mod state;
pub mod streams;
pub mod theme;
pub mod time;
