//! The report cache behind `llm_provider_create_from_sniff`.
//!
//! It keeps the last 8 reports for 10 minutes by `report_id`, so the webview can only create a
//! provider from a report core produced itself. Pure: the clock is injected.

use std::{
    collections::VecDeque,
    sync::{Mutex, PoisonError},
};

use super::types::SniffReport;

/// Reports kept.
const MAX_REPORTS: usize = 8;

/// How long a report stays valid.
const REPORT_TTL_MS: i64 = 10 * 60 * 1000;

/// The last reports, newest last.
#[derive(Debug, Default)]
pub struct SniffReportCache {
    entries: Mutex<VecDeque<(i64, SniffReport)>>,
}

impl SniffReportCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Drops the reports older than `REPORT_TTL_MS` at `now_ms`.
    fn prune(entries: &mut VecDeque<(i64, SniffReport)>, now_ms: i64) {
        entries.retain(|(stored_at, _)| now_ms.saturating_sub(*stored_at) < REPORT_TTL_MS);
    }

    /// Stores `report` (replacing one with the same id) and drops the oldest beyond
    /// `MAX_REPORTS`.
    pub fn insert(&self, report: SniffReport, now_ms: i64) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        Self::prune(&mut entries, now_ms);
        entries.retain(|(_, stored)| stored.report_id != report.report_id);
        entries.push_back((now_ms, report));
        while entries.len() > MAX_REPORTS {
            entries.pop_front();
        }
    }

    /// The report with `report_id`, when it is younger than `REPORT_TTL_MS`.
    #[must_use]
    pub fn get(&self, report_id: &str, now_ms: i64) -> Option<SniffReport> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        Self::prune(&mut entries, now_ms);
        entries
            .iter()
            .find(|(_, report)| report.report_id == report_id)
            .map(|(_, report)| report.clone())
    }

    /// Forgets a report (a cancelled or discarded scan, or one that was used).
    pub fn remove(&self, report_id: &str) -> bool {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let before = entries.len();
        entries.retain(|(_, report)| report.report_id != report_id);
        entries.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::sniff::test_support::blank_report;

    #[test]
    fn keeps_eight_reports_for_ten_minutes() {
        let cache = SniffReportCache::new();
        for index in 0..10 {
            cache.insert(blank_report(&format!("r{index}")), 1_000 + index);
        }
        assert!(cache.get("r0", 2_000).is_none() && cache.get("r1", 2_000).is_none());
        assert!(cache.get("r2", 2_000).is_some() && cache.get("r9", 2_000).is_some());
        assert!(cache.get("r9", 1_009 + REPORT_TTL_MS - 1).is_some());
        assert!(cache.get("r9", 1_009 + REPORT_TTL_MS).is_none());
        assert!(cache.get("never-issued", 2_000).is_none());
    }

    #[test]
    fn remove_and_replace() {
        let cache = SniffReportCache::new();
        cache.insert(blank_report("a"), 0);
        let mut changed = blank_report("a");
        changed.requests_used = 5;
        cache.insert(changed, 1);
        assert_eq!(cache.get("a", 2).map(|r| r.requests_used), Some(5));
        assert!(cache.remove("a"));
        assert!(!cache.remove("a"));
        assert!(cache.get("a", 2).is_none());
    }
}
