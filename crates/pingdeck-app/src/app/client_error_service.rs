//! Webview errors into the diagnostic log (docs/architecture.md §12).
//!
//! Single responsibility: take what the page reports about its own failures (a render crash
//! caught by an error boundary, an uncaught exception, an unhandled promise rejection) and append
//! it to the log as one redacted record. The page is not trusted: the source is checked against
//! a fixed list, the text is cut to a bounded length, and a page stuck in an error loop cannot
//! flood the log (at most [`MAX_REPORTS_PER_MINUTE`] records a minute; the next accepted record
//! says how many were dropped).

use std::sync::{Mutex, PoisonError};

use crate::logging::{LogLevel, log_line};

use super::current_time_ms;

/// Longest message kept (characters); the log itself also bounds a record.
const MAX_MESSAGE_CHARS: usize = 2_000;
/// Longest stack or component trace kept (characters).
const MAX_DETAIL_CHARS: usize = 4_000;
/// Records accepted per wall-clock minute.
pub const MAX_REPORTS_PER_MINUTE: u32 = 20;

/// Where a page error came from; anything else is logged as `other`.
const SOURCES: [&str; 3] = ["render", "error", "rejection"];

/// The minute being counted, records accepted in it, records dropped since the last accepted one.
static BUDGET: Mutex<(i64, u32, u64)> = Mutex::new((0, 0, 0));

fn cut(text: &str, max: usize) -> &str {
    text.char_indices()
        .nth(max)
        .map_or(text, |(at, _)| &text[..at])
}

/// Whether a record may be written at `now_ms`; the second value is how many were dropped
/// before it.
fn admit(now_ms: i64) -> Option<u64> {
    let mut budget = BUDGET.lock().unwrap_or_else(PoisonError::into_inner);
    let minute = now_ms.div_euclid(60_000);
    if budget.0 != minute {
        budget.0 = minute;
        budget.1 = 0;
    }
    if budget.1 >= MAX_REPORTS_PER_MINUTE {
        budget.2 = budget.2.saturating_add(1);
        return None;
    }
    budget.1 += 1;
    Some(std::mem::take(&mut budget.2))
}

/// The record for one report: `<source>: <message>` plus the bounded detail.
fn record(source: &str, message: &str, detail: Option<&str>, dropped: u64) -> (String, String) {
    let source = SOURCES
        .iter()
        .find(|known| **known == source)
        .copied()
        .unwrap_or("other");
    let mut text = cut(message.trim(), MAX_MESSAGE_CHARS).to_owned();
    if let Some(detail) = detail.map(str::trim).filter(|detail| !detail.is_empty()) {
        text.push_str(" | ");
        text.push_str(cut(detail, MAX_DETAIL_CHARS));
    }
    if dropped > 0 {
        text.push_str(&format!(" ({dropped} earlier page errors not logged)"));
    }
    (format!("webview.{source}"), text)
}

/// Appends a page error to the diagnostic log (redacted, bounded, rate-limited). Never fails:
/// a report over the budget is dropped and counted.
pub fn app_report_client_error_impl(source: &str, message: &str, detail: Option<&str>) {
    let Some(dropped) = admit(current_time_ms()) else {
        return;
    };
    let (target, text) = record(source, message, detail, dropped);
    log_line(LogLevel::Error, &target, &text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_source_and_oversized_text_are_bounded() {
        let long = "x".repeat(MAX_MESSAGE_CHARS * 3);
        let (target, text) = record("evil\nERROR forged", &long, Some(&long), 0);
        assert_eq!(target, "webview.other");
        assert!(text.chars().count() <= MAX_MESSAGE_CHARS + MAX_DETAIL_CHARS + 3);
        let (target, text) = record("render", " boom ", Some("  "), 4);
        assert_eq!(target, "webview.render");
        assert_eq!(text, "boom (4 earlier page errors not logged)");
    }

    #[test]
    fn a_page_in_an_error_loop_is_held_to_the_minute_budget() {
        // A minute of its own, far from the wall clock other tests may use.
        let minute = 7_000_000 * 60_000;
        let admitted = (0..MAX_REPORTS_PER_MINUTE + 5)
            .filter(|_| admit(minute).is_some())
            .count();
        assert_eq!(admitted, MAX_REPORTS_PER_MINUTE as usize);
        assert_eq!(admit(minute + 60_000), Some(5));
    }
}
