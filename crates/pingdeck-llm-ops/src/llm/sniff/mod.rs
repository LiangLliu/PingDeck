//! The provider sniffer behind the Add-provider wizard and the API client's "Detect".
//!
//! Given one base URL and an optional key, a scan reports the identity, the working protocols
//! with their mounts, the model list, the key verdict and a balance finding, each with redacted
//! evidence. It sends at most 12 budgeted requests with a key, 8 without one and 20 in an
//! extended scan; the opt-in real requests are counted in `requests_used` but sit outside the
//! budget, the balance requests inside it. Nothing is saved here (D-41, D-42).
//!
//! Entry point: [`sniff_provider`]. The phases run on a scan state (`scan.rs`) through a budgeted,
//! redacting executor (`exec.rs`); `report.rs` assembles the result. [`AuthMemory`],
//! [`SniffReportCache`], [`SniffDiff`] and [`duplicate_key`] serve the use-case layer.

mod auth;
mod balance_hook;
mod cache;
mod diff;
mod engine;
mod exec;
mod identify;
mod identify_phase;
mod key_guard;
mod local;
mod models;
mod models_phase;
mod plan;
mod report;
mod route_probe;
mod route_verdict;
mod scan;
mod share;
mod types;
mod url;

#[cfg(test)]
mod engine_tests;
#[cfg(test)]
mod test_support;

pub use auth::AuthMemory;
pub use cache::SniffReportCache;
pub use diff::{AdapterChange, IdentityChange, SniffDiff, StoredProviderView};
pub use engine::sniff_provider;
pub use models::{ListCursor, ListPage, MAX_PAGES, ModelKind, parse_list_page};
pub use share::{CURL_KEY_VARIABLE, duplicate_key};
pub use types::{
    BalanceFinding, EvidenceRefs, IdentityLabel, KeyCheck, KeyCheckStatus, KeyEvidence, PhaseState,
    PhaseSummary, PlannedRow, RouteVerdict, SkippedItem, SniffContext, SniffError, SniffEvent,
    SniffInput, SniffPhase, SniffReport, SniffWarning, SniffedEndpoint, SniffedModel,
};
pub use url::{NormalizedUrl, UrlHostClass, normalize_base_url};
