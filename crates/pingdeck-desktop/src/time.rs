//! Local-timezone date, time, range-bound, and relative-age formatting helpers for desktop views.

use chrono::{Datelike as _, Local, NaiveDate, TimeZone as _, Utc};
use pingdeck_core::model::settings::Language;

use crate::i18n::tr;

const MS_PER_MIN: i64 = 60_000;
const MS_PER_DAY: i64 = 86_400_000;

/// Returns the local timezone offset east of UTC in minutes (matches `UsageQuery::tz_offset_minutes`).
#[must_use]
pub fn local_tz_offset_minutes() -> i16 {
    let mins = Local::now().offset().local_minus_utc() / 60;
    i16::try_from(mins).unwrap_or(0)
}

fn to_local(ms: i64) -> Option<chrono::DateTime<Local>> {
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.with_timezone(&Local))
}

/// Formats a Unix millisecond timestamp as local `YYYY-MM-DD`.
#[must_use]
pub fn format_local_date(ms: i64) -> String {
    to_local(ms)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "—".to_owned())
}

/// Formats a Unix millisecond timestamp as local `HH:MM:SS`.
#[must_use]
pub fn format_local_time(ms: i64) -> String {
    to_local(ms)
        .map(|dt| dt.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "—".to_owned())
}

/// Formats a Unix millisecond timestamp as local `YYYY-MM-DD HH:MM:SS`.
#[must_use]
pub fn format_local_datetime(ms: i64) -> String {
    to_local(ms)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "—".to_owned())
}

/// Formats a Unix millisecond timestamp as local smart timestamp (Today / Yesterday / date).
#[must_use]
pub fn format_smart_timestamp(ms: i64, now_ms: i64, lang: Language) -> String {
    let (Some(dt), Some(now)) = (to_local(ms), to_local(now_ms)) else {
        return format_local_datetime(ms);
    };
    let c = tr(lang).common();
    let today = now.date_naive();
    let date = dt.date_naive();
    if date == today {
        c.time_today(dt.format("%H:%M:%S"))
    } else if today.signed_duration_since(date).num_days() == 1 {
        c.time_yesterday(dt.format("%H:%M:%S"))
    } else if date.year() == today.year() {
        dt.format("%m-%d %H:%M").to_string()
    } else {
        dt.format("%Y-%m-%d %H:%M").to_string()
    }
}

/// Returns a localized human-readable relative age label.
#[must_use]
pub fn format_relative_age(ms: i64, now_ms: i64, lang: Language) -> String {
    let c = tr(lang).common();
    let diff_ms = now_ms.saturating_sub(ms);
    if diff_ms < 45_000 {
        return c.time_just_now().to_owned();
    }
    if diff_ms < 3_600_000 {
        let mins = (diff_ms / MS_PER_MIN).max(1);
        return c.time_minutes_ago(mins);
    }
    if diff_ms < MS_PER_DAY {
        let hrs = (diff_ms / 3_600_000).max(1);
        return c.time_hours_ago(hrs);
    }
    let days = diff_ms / MS_PER_DAY;
    if days < 7 {
        return c.time_days_ago(days);
    }
    format_local_date(ms)
}

/// Groups a timestamp into `0 = Today`, `1 = Yesterday`, `2 = Earlier`.
#[must_use]
pub fn date_bucket(ms: i64, now_ms: i64) -> u8 {
    let (Some(dt), Some(now)) = (to_local(ms), to_local(now_ms)) else {
        return 2;
    };
    let delta = now
        .date_naive()
        .signed_duration_since(dt.date_naive())
        .num_days();
    if delta <= 0 {
        0
    } else if delta == 1 {
        1
    } else {
        2
    }
}

/// Start of the local calendar day containing `now_ms`, in Unix milliseconds.
#[must_use]
pub fn local_day_start_ms(now_ms: i64) -> i64 {
    let Some(now) = to_local(now_ms) else {
        return now_ms - MS_PER_DAY;
    };
    local_midnight_ms(now.date_naive()).unwrap_or(now_ms - MS_PER_DAY)
}

/// `(start_ms, end_ms)` for the local calendar month containing `now_ms`.
#[must_use]
pub fn local_month_bounds_ms(now_ms: i64) -> (i64, i64) {
    let Some(now) = to_local(now_ms) else {
        return (now_ms - 30 * MS_PER_DAY, now_ms);
    };
    let first = NaiveDate::from_ymd_opt(now.year(), now.month(), 1)
        .and_then(local_midnight_ms)
        .unwrap_or(now_ms - 30 * MS_PER_DAY);
    (first, now_ms)
}

/// `(start_ms, end_ms)` for the previous local calendar month before `now_ms`.
#[must_use]
pub fn local_prev_month_bounds_ms(now_ms: i64) -> (i64, i64) {
    let Some(now) = to_local(now_ms) else {
        return (now_ms - 60 * MS_PER_DAY, now_ms - 30 * MS_PER_DAY);
    };
    let this_first_date = NaiveDate::from_ymd_opt(now.year(), now.month(), 1);
    let (prev_y, prev_m) = if now.month() == 1 {
        (now.year() - 1, 12)
    } else {
        (now.year(), now.month() - 1)
    };
    let prev_first_date = NaiveDate::from_ymd_opt(prev_y, prev_m, 1);
    match (
        prev_first_date.and_then(local_midnight_ms),
        this_first_date.and_then(local_midnight_ms),
    ) {
        (Some(start), Some(end)) => (start, end),
        _ => (now_ms - 60 * MS_PER_DAY, now_ms - 30 * MS_PER_DAY),
    }
}

fn local_midnight_ms(date: NaiveDate) -> Option<i64> {
    let naive = date.and_hms_opt(0, 0, 0)?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .or_else(|| Local.from_local_datetime(&naive).latest())
        .map(|dt| dt.timestamp_millis())
}
