//! A stall timer that notices the machine sleeping (sleep and wake).
//!
//! Single responsibility: [`wake_aware_sleep`], the real clock's `stall_wait`. The monotonic clock
//! does not run while the machine sleeps, so a 90 s stall timer simply pauses across a sleep and
//! the stream that went silent meanwhile would wait 90 more awake seconds. The timer therefore
//! also reads the wall clock every tick: when the wall clock advanced by clearly more than the
//! monotonic one, the machine slept, and the deadline moves to `after_wake` from now.

use std::time::Duration;

/// How often the wall clock is compared with the monotonic one.
pub(super) const TICK: Duration = Duration::from_secs(1);
/// The wall clock must gain this much on the monotonic clock in one tick to count as a sleep.
const SLEEP_JUMP_MS: i64 = 3_000;

/// Sleeps `stall`, or `after_wake` after a sleep was seen, whichever ends first. `wall_ms` is the
/// wall clock in milliseconds.
pub(super) async fn wake_aware_sleep(
    stall: Duration,
    after_wake: Duration,
    tick: Duration,
    wall_ms: impl Fn() -> i64,
) {
    let mut last = tokio::time::Instant::now();
    let mut deadline = last + stall;
    let mut last_wall = wall_ms();
    loop {
        if last >= deadline {
            return;
        }
        tokio::time::sleep(tick.min(deadline - last)).await;
        let now = tokio::time::Instant::now();
        let wall = wall_ms();
        let monotonic_ms = i64::try_from((now - last).as_millis()).unwrap_or(i64::MAX);
        if (wall - last_wall) - monotonic_ms >= SLEEP_JUMP_MS {
            deadline = deadline.min(now + after_wake);
        }
        (last, last_wall) = (now, wall);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::*;

    const STALL: Duration = Duration::from_secs(90);
    const WAKE: Duration = Duration::from_secs(10);

    /// A wall clock that follows tokio's (paused) time plus a hand-made jump, like a machine
    /// that slept for `jump`.
    fn clock(start: tokio::time::Instant, jump: &Arc<AtomicI64>) -> impl Fn() -> i64 {
        let jump = Arc::clone(jump);
        move || {
            1_000_000
                + i64::try_from(start.elapsed().as_millis()).unwrap_or(0)
                + jump.load(Ordering::SeqCst)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_sleep_the_timer_runs_the_full_stall() {
        let start = tokio::time::Instant::now();
        let jump = Arc::new(AtomicI64::new(0));
        wake_aware_sleep(STALL, WAKE, TICK, clock(start, &jump)).await;
        assert_eq!(start.elapsed(), STALL);
    }

    #[tokio::test(start_paused = true)]
    async fn a_sleep_in_the_middle_shortens_the_wait_to_ten_seconds_after_the_wake() {
        let start = tokio::time::Instant::now();
        let jump = Arc::new(AtomicI64::new(0));
        let waker = Arc::clone(&jump);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20_500)).await;
            // Thirty minutes of sleep pass on the wall clock only.
            waker.store(30 * 60 * 1000, Ordering::SeqCst);
        });
        wake_aware_sleep(STALL, WAKE, TICK, clock(start, &jump)).await;
        let waited = start.elapsed();
        assert!(waited >= Duration::from_secs(30), "{waited:?}");
        assert!(waited <= Duration::from_secs(32), "{waited:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_sleep_near_the_end_never_extends_the_deadline() {
        let start = tokio::time::Instant::now();
        let jump = Arc::new(AtomicI64::new(0));
        let waker = Arc::clone(&jump);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(85)).await;
            waker.store(60_000, Ordering::SeqCst);
        });
        wake_aware_sleep(STALL, WAKE, TICK, clock(start, &jump)).await;
        assert!(start.elapsed() <= STALL + TICK);
    }
}
