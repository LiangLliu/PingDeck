//! Local calendar days from a fixed UTC offset, without a timezone database.
//!
//! Day indices (days since 1970-01-01) map to `chrono::NaiveDate` behind `local_day`,
//! `local_midnight_ms` and the day ranges the usage views need. The offset is the webview's
//! `tz_offset_minutes` for "now"; DST changes inside a range are not modelled
//! (`docs/decisions.md` §6 D-198).

use chrono::{Datelike, NaiveDate};

/// Milliseconds in one minute.
const MS_PER_MINUTE: i64 = 60_000;
/// Milliseconds in one day.
const MS_PER_DAY: i64 = 86_400_000;
/// 1970-01-01 in chrono's day numbering (days from 0001-01-01, which is day 1).
const UNIX_EPOCH_DAYS_FROM_CE: i64 = 719_163;
/// Largest UTC offset in use (Kiribati, UTC+14); offsets beyond are clamped.
pub const MAX_TZ_OFFSET_MINUTES: i16 = 14 * 60;
/// Most days one daily query spans: the longest usage retention (3,650 days) plus the two
/// boundary days the pruning keeps, so "All" never cuts retained days.
pub(crate) const MAX_RANGE_DAYS: usize = 3_652;
/// Days between the first and the last day of a capped range.
const MAX_RANGE_SPAN_DAYS: i64 = MAX_RANGE_DAYS as i64 - 1;
/// Most buckets one minute-rollup query may ask for (`UsageQuery::bucket_minutes`).
pub const MAX_BUCKETS: usize = 3_000;

fn offset_ms(tz_offset_minutes: i16) -> i64 {
    i64::from(tz_offset_minutes.clamp(-MAX_TZ_OFFSET_MINUTES, MAX_TZ_OFFSET_MINUTES))
        * MS_PER_MINUTE
}

/// Local day index (days since 1970-01-01) of an instant.
fn local_day_index(ts_ms: i64, tz_offset_minutes: i16) -> i64 {
    ts_ms
        .saturating_add(offset_ms(tz_offset_minutes))
        .div_euclid(MS_PER_DAY)
}

/// The date of a day index; `None` beyond chrono's range of about +-262,000 years.
fn date_of(index: i64) -> Option<NaiveDate> {
    let days_from_ce = i32::try_from(index.checked_add(UNIX_EPOCH_DAYS_FROM_CE)?).ok()?;
    NaiveDate::from_num_days_from_ce_opt(days_from_ce)
}

/// `YYYY-MM-DD` of a day index; indices beyond chrono's range clamp to its first or last date.
fn format_day(index: i64) -> String {
    date_of(index)
        .unwrap_or(if index < 0 {
            NaiveDate::MIN
        } else {
            NaiveDate::MAX
        })
        .format("%Y-%m-%d")
        .to_string()
}

/// Parses a strict `YYYY-MM-DD` (four-digit year, real calendar date) into a day index.
fn parse_day(day: &str) -> Option<i64> {
    let bytes = day.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = bytes.get(range)?;
        part.iter().all(u8::is_ascii_digit).then(|| {
            part.iter()
                .fold(0, |acc, byte| acc * 10 + u32::from(byte - b'0'))
        })
    };
    let (year, month, date) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    let date = NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, month, date)?;
    Some(i64::from(date.num_days_from_ce()) - UNIX_EPOCH_DAYS_FROM_CE)
}

/// The local calendar day `YYYY-MM-DD` of `ts_ms` (Unix ms) at a fixed UTC offset in minutes
/// (east positive). Offsets beyond +-14 h are clamped.
#[must_use]
pub fn local_day(ts_ms: i64, tz_offset_minutes: i16) -> String {
    format_day(local_day_index(ts_ms, tz_offset_minutes))
}

/// Unix ms of local midnight that starts `day`, `None` when `day` is not a strict `YYYY-MM-DD`.
#[must_use]
pub fn local_midnight_ms(day: &str, tz_offset_minutes: i16) -> Option<i64> {
    let index = parse_day(day)?;
    Some(index * MS_PER_DAY - offset_ms(tz_offset_minutes))
}

/// `day` moved by `delta` days, `None` when `day` is not a strict `YYYY-MM-DD` or the result
/// leaves years 0000..=9999.
#[must_use]
pub fn add_days(day: &str, delta: i64) -> Option<String> {
    let date = date_of(parse_day(day)?.checked_add(delta)?)?;
    (0..=9999)
        .contains(&date.year())
        .then(|| date.format("%Y-%m-%d").to_string())
}

/// First and last local day index (inclusive) touched by `[from_ms, to_ms)`, capped at
/// [`MAX_RANGE_DAYS`] days (the newest are kept); `None` when the range is empty.
fn day_index_span(from_ms: i64, to_ms: i64, tz_offset_minutes: i16) -> Option<(i64, i64)> {
    if to_ms <= from_ms {
        return None;
    }
    let last = local_day_index(to_ms - 1, tz_offset_minutes);
    let first =
        local_day_index(from_ms, tz_offset_minutes).max(last.saturating_sub(MAX_RANGE_SPAN_DAYS));
    Some((first, last))
}

/// Every local day touched by `[from_ms, to_ms)`, oldest first; empty when the range is empty.
/// Capped at [`MAX_RANGE_DAYS`] (the newest days are kept).
pub(crate) fn day_range(from_ms: i64, to_ms: i64, tz_offset_minutes: i16) -> Vec<String> {
    day_index_span(from_ms, to_ms, tz_offset_minutes)
        .map(|(first, last)| (first..=last).map(format_day).collect())
        .unwrap_or_default()
}

/// The first and last day of [`day_range`] without building the days in between.
pub(crate) fn day_window_bounds(
    from_ms: i64,
    to_ms: i64,
    tz_offset_minutes: i16,
) -> Option<(String, String)> {
    day_index_span(from_ms, to_ms, tz_offset_minutes)
        .map(|(first, last)| (format_day(first), format_day(last)))
}

/// The local wall-clock minute `YYYY-MM-DDTHH:MM` of `ts_ms` at a fixed UTC offset: the label of
/// a minute-rollup bucket (SQLite's `strftime('%Y-%m-%dT%H:%M', …)` of the shifted instant).
pub(crate) fn local_minute(ts_ms: i64, tz_offset_minutes: i16) -> String {
    let local = ts_ms.saturating_add(offset_ms(tz_offset_minutes));
    let minute_of_day = local.rem_euclid(MS_PER_DAY) / MS_PER_MINUTE;
    format!(
        "{}T{:02}:{:02}",
        format_day(local.div_euclid(MS_PER_DAY)),
        minute_of_day / 60,
        minute_of_day % 60
    )
}

/// Unix ms where the local bucket of `bucket_minutes` holding `ts_ms` starts; buckets are cut
/// from local midnight, so 60 gives whole local hours and 1440 local days. Saturates at the ends
/// of `i64` (an instant there gets the nearest representable start).
#[must_use]
pub fn bucket_start_ms(ts_ms: i64, tz_offset_minutes: i16, bucket_minutes: u32) -> i64 {
    let width = i64::from(bucket_minutes.max(1)) * MS_PER_MINUTE;
    let shift = offset_ms(tz_offset_minutes);
    // A true start never follows `ts_ms`; the final `min` only bites where saturation broke that.
    ts_ms
        .saturating_add(shift)
        .div_euclid(width)
        .saturating_mul(width)
        .saturating_sub(shift)
        .min(ts_ms)
}

/// Labels (`local_minute` of the start) of every bucket whose start lies in
/// `[bucket_start(from_ms), to_ms)`, oldest first; at most [`MAX_BUCKETS`] (the newest kept).
#[must_use]
pub fn bucket_range(
    from_ms: i64,
    to_ms: i64,
    tz_offset_minutes: i16,
    bucket_minutes: u32,
) -> Vec<String> {
    let width = i64::from(bucket_minutes.max(1)) * MS_PER_MINUTE;
    let first = bucket_start_ms(from_ms, tz_offset_minutes, bucket_minutes);
    let span = to_ms.saturating_sub(first).max(0);
    let count = usize::try_from(span.saturating_add(width - 1) / width).unwrap_or(0);
    let skip = count.saturating_sub(MAX_BUCKETS);
    (skip..count)
        .map(|n| {
            let start = first.saturating_add(i64::try_from(n).unwrap_or(0).saturating_mul(width));
            local_minute(start, tz_offset_minutes)
        })
        .collect()
}
