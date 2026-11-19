//! The local UTC offset the usage rollup cuts days with.
//!
//! Single responsibility: [`TzOffset`], a cell holding the offset in minutes east of UTC. The
//! shell may set it (the webview knows the user's zone); until then it is read from the OS,
//! cached and re-read hourly so a daylight-saving change is followed.

use std::sync::atomic::{AtomicBool, AtomicI16, AtomicI64, Ordering};

use crate::llm::runtime::now_ms;
use crate::llm::usage::MAX_TZ_OFFSET_MINUTES;

/// How long an OS reading is trusted.
const REFRESH_MS: i64 = 3_600_000;

/// The offset cell; see the module docs.
#[derive(Debug, Default)]
pub struct TzOffset {
    minutes: AtomicI16,
    overridden: AtomicBool,
    /// Time of the next OS reading, Unix ms; 0 = never read.
    next_read_ms: AtomicI64,
}

impl TzOffset {
    /// A cell that has not read the OS yet and has no override.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Minutes east of UTC. Refreshes from the OS clock when the cached reading is older than an
    /// hour.
    #[must_use]
    pub fn get(&self) -> i16 {
        if !self.overridden.load(Ordering::Relaxed) {
            let now = now_ms();
            if now >= self.next_read_ms.load(Ordering::Relaxed) {
                self.next_read_ms.store(now + REFRESH_MS, Ordering::Relaxed);
                self.minutes.store(os_offset_minutes(), Ordering::Relaxed);
            }
        }
        self.minutes.load(Ordering::Relaxed)
    }

    /// Pins the offset (clamped to +-14 h) and stops reading the OS.
    pub fn set(&self, minutes: i16) {
        self.minutes.store(
            minutes.clamp(-MAX_TZ_OFFSET_MINUTES, MAX_TZ_OFFSET_MINUTES),
            Ordering::Relaxed,
        );
        self.overridden.store(true, Ordering::Relaxed);
    }
}

/// The local UTC offset in minutes east of UTC from the system clock.
fn os_offset_minutes() -> i16 {
    (chrono::Local::now().offset().local_minus_utc() / 60)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pinned_offset_wins_over_the_os_and_is_clamped() {
        let cell = TzOffset::new();
        cell.set(480);
        assert_eq!(cell.get(), 480);
        cell.set(i16::MAX);
        assert_eq!(cell.get(), MAX_TZ_OFFSET_MINUTES);
    }
}
