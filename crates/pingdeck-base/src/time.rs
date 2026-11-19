//! The process clock and duration rounding.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Current wall clock in Unix milliseconds.
///
/// Saturates instead of panicking: a clock before 1970 or after year 292 million is not a
/// reason to lose a write.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// `duration` in whole milliseconds, rounded up (a phase that took time is never 0).
///
/// Saturates at `u64::MAX` instead of wrapping.
#[must_use]
pub fn ceil_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis())
        .unwrap_or(u64::MAX)
        .saturating_add(u64::from(
            !duration.subsec_nanos().is_multiple_of(1_000_000),
        ))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::ceil_ms;

    #[test]
    fn ceil_ms_rounds_any_fraction_of_a_millisecond_up() {
        assert_eq!(ceil_ms(Duration::ZERO), 0);
        assert_eq!(ceil_ms(Duration::from_nanos(1)), 1);
        assert_eq!(ceil_ms(Duration::from_millis(7)), 7);
        assert_eq!(ceil_ms(Duration::from_nanos(7_000_001)), 8);
        assert_eq!(ceil_ms(Duration::new(2, 999_999_999)), 3_000);
    }

    #[test]
    fn ceil_ms_matches_the_nanosecond_div_ceil_form() {
        // The executor's former private copy computed `as_nanos().div_ceil(1_000_000)`.
        for duration in [
            Duration::ZERO,
            Duration::from_nanos(999_999),
            Duration::from_nanos(1_000_000),
            Duration::from_nanos(1_000_001),
            Duration::new(12_345, 678_901_234),
        ] {
            let expected = u64::try_from(duration.as_nanos().div_ceil(1_000_000)).unwrap();
            assert_eq!(ceil_ms(duration), expected, "{duration:?}");
        }
    }

    #[test]
    fn ceil_ms_saturates_instead_of_wrapping() {
        assert_eq!(ceil_ms(Duration::MAX), u64::MAX);
        assert_eq!(ceil_ms(Duration::from_millis(u64::MAX)), u64::MAX);
    }
}
