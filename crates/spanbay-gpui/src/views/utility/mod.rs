//! Focused shell-level overlays outside the 10 primary navigation modules:
//! - [`ImportExportModal`]: unified centered workspace Import / Export dialog (8 formats,
//!   4 conflict strategies, preview, sanitized & plaintext export);
//! - [`DiagnosticsSheet`]: native right-hand slide-over sheet for live runtime overview,
//!   redacted problem stream, searchable `spanbay.log` tail, and dev-only GPU benchmark.

pub mod bench;
pub mod diagnostics;
pub mod import_export;
mod import_export_actions;

pub use diagnostics::{ConsoleMode, DiagnosticsHandoff, DiagnosticsSheet};
pub use import_export::{ImportExportModal, ImportExportOutcome, IoTab};
