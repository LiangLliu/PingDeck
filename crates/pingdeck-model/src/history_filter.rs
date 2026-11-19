//! History v2 query types: the filter, the keyset cursor and the run summaries a page carries.
//!
//! Single responsibility: describe *which* history rows a query selects and *where* a page
//! resumes, independent of SQL and of the shell. The storage layer turns a [`HistoryFilter`] into
//! one `WHERE` clause shared by paging, counting and clearing, so "Clear filtered" deletes exactly
//! the rows the list shows.

use serde::{Deserialize, Serialize};

use crate::models::{HistorySource, HttpMethod};

/// Width `executed_at` (Unix milliseconds as text) is compared at, so text order is time order.
const TIMESTAMP_WIDTH: usize = 13;

/// Which history rows a query selects. Every field narrows the result;
/// the default selects the whole workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryFilter {
    /// Free text matched (case-insensitive substring) against name, method, URLs, status, attempts,
    /// error, source name and environment name; blank means no text filter.
    #[serde(default)]
    pub query: String,
    /// Keep only these sources; empty keeps all.
    #[serde(default)]
    pub sources: Vec<HistorySource>,
    /// Keep rows whose status is in these classes (`2..=5` for 2xx..5xx); classes outside the
    /// range are ignored. Combined with `include_failed` as "any of".
    #[serde(default)]
    pub status_classes: Vec<u8>,
    /// Also keep failed rows: a transport failure, an HTTP error (400 and above) or a failed
    /// check. With no `status_classes` this keeps only them.
    #[serde(default)]
    pub include_failed: bool,
    /// Keep rows executed at or after this Unix-millisecond instant.
    #[serde(default)]
    pub since_ms: Option<i64>,
    /// Keep rows executed strictly before this Unix-millisecond instant.
    #[serde(default)]
    pub until_ms: Option<i64>,
    /// Keep rows recorded under one of these environment ids; empty keeps all.
    #[serde(default)]
    pub environment_ids: Vec<String>,
    /// Keep rows sent with one of these methods; empty keeps all.
    #[serde(default)]
    pub methods: Vec<HttpMethod>,
    /// Keep rows of this saved request (the "History of this request" link).
    #[serde(default)]
    pub request_id: Option<String>,
    /// Keep rows of this collection run or flow run.
    #[serde(default)]
    pub run_id: Option<String>,
    /// Keep rows that took at least this many milliseconds.
    #[serde(default)]
    pub min_elapsed_ms: Option<u64>,
}

impl HistoryFilter {
    /// Whether the filter selects every row (nothing narrows the result).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.query.trim().is_empty()
            && self.sources.is_empty()
            && self.valid_status_classes().next().is_none()
            && !self.include_failed
            && self.since_ms.is_none()
            && self.until_ms.is_none()
            && self.environment_ids.is_empty()
            && self.methods.is_empty()
            && self.request_id.is_none()
            && self.run_id.is_none()
            && self.min_elapsed_ms.is_none()
    }

    /// The status classes that map to a status range (`2..=5`), deduplicated and ordered.
    pub fn valid_status_classes(&self) -> impl Iterator<Item = u8> + '_ {
        (2_u8..=5).filter(|class| self.status_classes.contains(class))
    }
}

/// Renders a Unix-millisecond instant as the fixed-width text `executed_at` is stored and
/// compared as; negative instants clamp to zero.
#[must_use]
pub fn executed_at_bound(ms: i64) -> String {
    format!("{:0width$}", ms.max(0), width = TIMESTAMP_WIDTH)
}

/// Where a keyset page resumes: the `(executed_at, id)` of the last row of the previous page.
///
/// Opaque to shells (`encode`/`decode`); inserts while paging cannot repeat or skip a row because
/// the position is a value, not an offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryCursor {
    /// `executed_at` of the last returned row.
    pub executed_at: String,
    /// `id` of the last returned row.
    pub id: String,
}

impl HistoryCursor {
    /// `<len of executed_at>.<executed_at><id>`: unambiguous whatever characters the id holds.
    #[must_use]
    pub fn encode(&self) -> String {
        format!("{}.{}{}", self.executed_at.len(), self.executed_at, self.id)
    }

    /// Parses [`HistoryCursor::encode`] output; `None` for anything else.
    #[must_use]
    pub fn decode(text: &str) -> Option<Self> {
        let (length, rest) = text.split_once('.')?;
        let length: usize = length.parse().ok()?;
        let executed_at = rest.get(..length)?;
        let id = rest.get(length..)?;
        if executed_at.is_empty() || id.is_empty() {
            return None;
        }
        Some(Self {
            executed_at: executed_at.to_owned(),
            id: id.to_owned(),
        })
    }
}

/// Totals of one run (collection run or flow run) over the rows a filter keeps:
/// the shell collapses a page's rows with the same `run_id` under one run row and reads the
/// counts from here, so a run split across pages still shows its true size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRunSummary {
    /// The run's id (shared by its rows).
    pub run_id: String,
    /// `Runner` or `Flow`.
    pub source: HistorySource,
    /// Collection or flow name at record time.
    pub source_name: String,
    /// Collection or flow id.
    pub source_id: String,
    /// Rows of the run that match the filter.
    pub total: u64,
    /// Of those, rows that failed: a transport failure, a status of 400 and above or a failed
    /// check, the same rule as `HistoryFilter::include_failed`.
    pub failed: u64,
    /// `executed_at` of the run's oldest matching row.
    pub first_executed_at: String,
    /// `executed_at` of the run's newest matching row.
    pub last_executed_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trips_ids_with_any_characters() {
        for id in ["hist_1", "a.b.c", "9.12345", "with space/and.dots"] {
            let cursor = HistoryCursor {
                executed_at: "1700000000000".to_owned(),
                id: id.to_owned(),
            };
            assert_eq!(HistoryCursor::decode(&cursor.encode()), Some(cursor));
        }
    }

    #[test]
    fn cursor_decode_rejects_malformed_text() {
        for text in ["", "x", "5.abc", "3.abc", "0.id", "-1.abc", "2.é"] {
            assert_eq!(HistoryCursor::decode(text), None, "{text:?}");
        }
    }

    #[test]
    fn filter_emptiness_ignores_blank_text_and_unknown_classes() {
        let mut filter = HistoryFilter::default();
        assert!(filter.is_empty());
        filter.query = "  ".to_owned();
        filter.status_classes = vec![1, 9];
        assert!(filter.is_empty());
        filter.status_classes = vec![4];
        assert!(!filter.is_empty());
    }

    #[test]
    fn executed_at_bound_pads_to_the_stored_width() {
        assert_eq!(executed_at_bound(1_700_000_000_000), "1700000000000");
        assert_eq!(executed_at_bound(5), "0000000000005");
        assert_eq!(executed_at_bound(-3), "0000000000000");
    }
}
