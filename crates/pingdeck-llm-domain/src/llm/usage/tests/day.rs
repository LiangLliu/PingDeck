//! Local days, day ranges and minute buckets.

use super::{DAY_MS, ms};
use crate::llm::usage::day::{MAX_RANGE_DAYS, day_range, local_minute};
use crate::llm::usage::{
    MAX_BUCKETS, add_days, bucket_range, bucket_start_ms, local_day, local_midnight_ms,
};

#[test]
fn local_day_follows_the_offset() {
    assert_eq!(local_day(0, 0), "1970-01-01");
    assert_eq!(local_day(-1, 0), "1969-12-31");
    assert_eq!(local_day(DAY_MS - 1, 0), "1970-01-01");
    assert_eq!(local_day(DAY_MS, 0), "1970-01-02");

    let base = ms("2026-11-06", 0);
    assert_eq!(local_day(base + (15 * 60 + 50) * 60_000, 480), "2026-11-06");
    assert_eq!(local_day(base + (16 * 60 + 10) * 60_000, 480), "2026-11-07");
    assert_eq!(local_day(base + (16 * 60 + 10) * 60_000, 0), "2026-11-06");

    let midnight_utc = ms("2026-03-01", 0);
    assert_eq!(local_day(midnight_utc, -300), "2026-02-28");
    assert_eq!(local_day(midnight_utc + 5 * 3_600_000, -300), "2026-03-01");
}

#[test]
fn local_day_knows_leap_years() {
    for (d, want) in [
        ("2024-02-28", "2024-02-29"),
        ("2024-02-29", "2024-03-01"),
        ("2000-02-28", "2000-02-29"),
        ("2100-02-28", "2100-03-01"),
        ("1900-02-28", "1900-03-01"),
    ] {
        assert_eq!(local_day(ms(d, 0) + DAY_MS, 0), want, "{d}");
    }
}

#[test]
fn local_day_clamps_offsets_beyond_14h() {
    assert_eq!(local_day(0, 20 * 60), local_day(0, 14 * 60));
    assert_eq!(local_day(0, -20 * 60), local_day(0, -14 * 60));
    assert!(!local_day(i64::MAX, i16::MAX).is_empty());
    assert!(!local_day(i64::MIN, i16::MIN).is_empty());
}

#[test]
fn local_midnight_starts_the_local_day() {
    for tz in [-720, -300, 0, 330, 480, 840] {
        let start = ms("2026-11-07", tz);
        assert_eq!(local_day(start, tz), "2026-11-07");
        assert_eq!(local_day(start - 1, tz), "2026-11-06");
        assert_eq!(local_day(start + DAY_MS - 1, tz), "2026-11-07");
    }
    assert_eq!(ms("2026-11-07", 480), ms("2026-11-07", 0) - 8 * 3_600_000);
    assert!(local_midnight_ms("2024-02-29", 0).is_some());
}

#[test]
fn local_midnight_rejects_non_strict_dates() {
    for bad in [
        "",
        "2026-1-01",
        "2026-13-01",
        "2026-00-10",
        "2026-04-31",
        "2023-02-29",
        "2100-02-29",
        "2026/11/07",
        "abcd-ef-gh",
        "+026-11-07",
        "2026-11-070",
        "2026-11-0",
        "２０２６-11-07",
    ] {
        assert_eq!(local_midnight_ms(bad, 0), None, "{bad}");
    }
}

#[test]
fn add_days_moves_across_months_and_years() {
    assert_eq!(add_days("2026-11-07", 1).as_deref(), Some("2026-11-08"));
    assert_eq!(add_days("2026-12-31", 1).as_deref(), Some("2027-01-01"));
    assert_eq!(add_days("2024-02-28", 2).as_deref(), Some("2024-03-01"));
    assert_eq!(add_days("2024-03-01", -1).as_deref(), Some("2024-02-29"));
    assert_eq!(add_days("2026-11-07", 0).as_deref(), Some("2026-11-07"));
}

#[test]
fn add_days_stays_within_years_0000_to_9999() {
    for (d, n) in [
        ("9999-12-31", 1),
        ("0000-01-01", -1),
        ("nope", 1),
        ("2026-11-07", i64::MAX),
    ] {
        assert_eq!(add_days(d, n), None);
    }
}

#[test]
fn day_range_covers_every_touched_day() {
    let (from, to) = (ms("2026-11-01", 480), ms("2026-11-08", 480));
    let days = day_range(from, to, 480);
    assert_eq!(
        (days.len(), days[0].as_str(), days[6].as_str()),
        (7, "2026-11-01", "2026-11-07")
    );
    assert_eq!(day_range(from, to + 1, 480).len(), 8);
    assert_eq!(day_range(from + 3_600_000, to, 480).len(), 7);
    assert!(day_range(10, 10, 0).is_empty() && day_range(20, 10, 0).is_empty());
}

#[test]
fn day_range_keeps_the_newest_days_when_capped() {
    let capped = day_range(0, ms("2030-01-01", 0), 0);
    assert_eq!(
        (capped.len(), capped.last().unwrap().as_str()),
        (MAX_RANGE_DAYS, "2029-12-31")
    );
}

#[test]
fn local_minute_labels_the_wall_clock_minute() {
    let tz = 8 * 60;
    let two_pm = ms("2026-11-09", tz) + 14 * 3_600_000;
    assert_eq!(local_minute(two_pm + 59_999, tz), "2026-11-09T14:00");
    assert_eq!(
        local_minute(ms("2026-11-09", -300), -300),
        "2026-11-09T00:00"
    );
}

#[test]
fn buckets_are_cut_from_local_midnight() {
    let tz = 8 * 60;
    let two_pm = ms("2026-11-09", tz) + 14 * 3_600_000;
    assert_eq!(
        bucket_start_ms(two_pm + 7 * 60_000, tz, 5),
        two_pm + 5 * 60_000
    );
    let now = two_pm + 3 * 60_000;
    let hour = bucket_range(now - 3_600_000, now, tz, 5);
    assert_eq!(
        (hour.len(), hour[0].as_str(), hour[12].as_str()),
        (13, "2026-11-09T13:00", "2026-11-09T14:00")
    );
    let week = bucket_range(now - 7 * DAY_MS, now, tz, 360);
    assert_eq!((week.len(), week[0].as_str()), (29, "2026-11-02T12:00"));
    assert_eq!(bucket_range(now - 90 * DAY_MS, now, tz, 1_440).len(), 91);
}

#[test]
fn bucket_range_caps_at_max_buckets() {
    let now = ms("2026-11-09", 480) + 14 * 3_600_000 + 3 * 60_000;
    assert_eq!(bucket_range(0, now, 0, 1).len(), MAX_BUCKETS);
}

// These inputs overflowed i64 (a panic in debug builds).
#[test]
fn bucket_starts_saturate_at_the_ends_of_time() {
    let day = 1_440;
    let shift = 840 * 60_000;
    for tz in [-840, 840] {
        // The true start lies at or before `i64::MIN`, so the nearest representable one is it.
        assert_eq!(bucket_start_ms(i64::MIN, tz, day), i64::MIN, "{tz}");
        let start = bucket_start_ms(i64::MAX, tz, day);
        assert!(i64::MAX - start < DAY_MS + shift, "{tz}: {start}");
        assert_eq!(bucket_range(i64::MIN, i64::MAX, tz, 1).len(), MAX_BUCKETS);
        assert!(bucket_range(i64::MIN, i64::MIN + DAY_MS, tz, day).len() <= 2);
    }
}
