//! The caller-supplied wall clock and its local-time stamps for backup names.

/// `strftime` format of a backup stamp: local `YYYYMMDD-HHMMSS`.
const STAMP_FORMAT: &str = "%Y%m%d-%H%M%S";
/// Length of a stamp and the index of its dash, both fixed by [`STAMP_FORMAT`].
pub(super) const STAMP_LEN: usize = 15;
/// See [`STAMP_LEN`].
pub(super) const STAMP_DASH_AT: usize = 8;

/// Wall-clock time and local UTC offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    /// Unix time in milliseconds.
    pub now_ms: i64,
    /// Local offset from UTC in minutes (UTC+8 is `480`).
    pub tz_offset_minutes: i32,
}

impl Clock {
    /// The system clock with the machine's current UTC offset.
    #[must_use]
    pub fn system() -> Self {
        Self {
            now_ms: pingdeck_base::time::now_ms(),
            tz_offset_minutes: local_offset_minutes(),
        }
    }

    /// `YYYYMMDD-HHMMSS` in local time, `bump_secs` seconds after now (1970 when out of range).
    pub(super) fn stamp(self, bump_secs: i64) -> String {
        let local =
            self.now_ms.div_euclid(1000) + i64::from(self.tz_offset_minutes) * 60 + bump_secs;
        chrono::DateTime::from_timestamp(local, 0)
            .unwrap_or_default()
            .format(STAMP_FORMAT)
            .to_string()
    }

    /// Unix ms of a stamp made by [`Clock::stamp`] in this clock's zone; `None` for no stamp.
    pub(super) fn stamp_ms(self, stamp: &str) -> Option<i64> {
        let local = chrono::NaiveDateTime::parse_from_str(stamp, STAMP_FORMAT).ok()?;
        let secs = local.and_utc().timestamp() - i64::from(self.tz_offset_minutes) * 60;
        Some(secs * 1000)
    }
}

/// The machine's UTC offset in minutes.
fn local_offset_minutes() -> i32 {
    chrono::Local::now().offset().local_minus_utc() / 60
}

#[cfg(test)]
mod tests {
    use super::super::test_util::CLOCK as TEST_CLOCK;
    use super::*;

    #[test]
    fn stamps_are_local_time() {
        assert_eq!(TEST_CLOCK.stamp(0), "20261107-100000");
        let tokyo = Clock {
            tz_offset_minutes: 540,
            ..TEST_CLOCK
        };
        assert_eq!(tokyo.stamp(0), "20261107-190000");
        let la = Clock {
            tz_offset_minutes: -480,
            ..TEST_CLOCK
        };
        assert_eq!(la.stamp(0), "20261107-020000");
        let before_midnight = Clock {
            tz_offset_minutes: -600,
            now_ms: 5 * 3_600_000,
        };
        assert_eq!(before_midnight.stamp(0), "19691231-190000");
        assert_eq!(TEST_CLOCK.stamp(3600 * 14), "20261108-000000");
    }
}
