//! Live frames of a compare run: the partial replies the table shows while rows stream.
//!
//! Single responsibility: turn the frames each row's call reads into `RowFrames` events, one
//! shared rate limit per compare. Frames of every row wait in one batcher; once per window
//! (`1/30 s`, the window of `AiRunEvent::Frames`) the batcher releases one batch per row that has
//! pending frames. The window clock is a parameter of the pure [`FrameBatcher`], so tests drive
//! it without wall time; [`LiveFrames`] supplies the real clock, a ticker for held-back tails and
//! the final flush a row runs before its `RowDone`.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::compare::AiCompareEvent;
use crate::events::{COALESCE_WINDOW, EventSink};
use crate::llm::ai_run::AiRunEvent;
use crate::llm::capture_record::CapturedFrame;

/// The frames one row gained, in order.
type Batch = (usize, Vec<CapturedFrame>);

/// Pending frames per row and the time of the last release: pure, the caller passes `now`.
#[derive(Debug)]
pub(super) struct FrameBatcher {
    window: Duration,
    last_release: Option<Instant>,
    pending: BTreeMap<usize, Vec<CapturedFrame>>,
}

impl FrameBatcher {
    pub(super) fn new(window: Duration) -> Self {
        Self {
            window,
            last_release: None,
            pending: BTreeMap::new(),
        }
    }

    /// Adds `items` to `row`; returns what is ready now. The first frames after a quiet period
    /// go out at once, later ones wait for the window to pass.
    pub(super) fn push(
        &mut self,
        row: usize,
        items: Vec<CapturedFrame>,
        now: Instant,
    ) -> Vec<Batch> {
        self.pending.entry(row).or_default().extend(items);
        self.poll(now)
    }

    /// Every row's pending frames, once the window since the last release has passed.
    pub(super) fn poll(&mut self, now: Instant) -> Vec<Batch> {
        let due = self
            .last_release
            .is_none_or(|last| now.saturating_duration_since(last) >= self.window);
        if self.pending.is_empty() || !due {
            return Vec::new();
        }
        self.last_release = Some(now);
        std::mem::take(&mut self.pending).into_iter().collect()
    }

    /// Takes `row`'s pending frames regardless of the window (the end of the row).
    pub(super) fn take_row(&mut self, row: usize) -> Option<Vec<CapturedFrame>> {
        self.pending.remove(&row)
    }
}

/// The compare run's frame relay: one batcher behind a lock, events go out under that lock so a
/// row's `RowFrames` can never overtake the events around them.
pub(super) struct LiveFrames<'a> {
    sink: &'a dyn EventSink<AiCompareEvent>,
    batcher: Mutex<FrameBatcher>,
}

impl<'a> LiveFrames<'a> {
    pub(super) fn new(sink: &'a dyn EventSink<AiCompareEvent>) -> Self {
        Self {
            sink,
            batcher: Mutex::new(FrameBatcher::new(COALESCE_WINDOW)),
        }
    }

    fn batcher(&self) -> MutexGuard<'_, FrameBatcher> {
        self.batcher.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn emit(&self, batches: Vec<Batch>) {
        for (index, items) in batches {
            self.sink.emit(AiCompareEvent::RowFrames { index, items });
        }
    }

    /// The sink the call of row `index` reports its frames to.
    pub(super) fn row_sink(&self, index: usize) -> RowSink<'_, 'a> {
        RowSink {
            frames: self,
            index,
        }
    }

    /// Sends everything `row` still holds; call it before the row's `RowDone`.
    pub(super) fn finish_row(&self, row: usize) {
        let mut batcher = self.batcher();
        if let Some(items) = batcher.take_row(row) {
            self.sink
                .emit(AiCompareEvent::RowFrames { index: row, items });
        }
    }

    /// Releases held-back frames once a window has passed, for as long as it is polled; never
    /// completes (race it against the rows).
    pub(super) async fn pump(&self) -> Infallible {
        if !self.sink.is_active() {
            std::future::pending::<()>().await;
        }
        let mut ticker = tokio::time::interval(COALESCE_WINDOW);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let mut batcher = self.batcher();
            let ready = batcher.poll(Instant::now());
            self.emit(ready);
        }
    }
}

/// One row's view of [`LiveFrames`]: it takes the `Frames` events of the row's run.
pub(super) struct RowSink<'f, 'a> {
    frames: &'f LiveFrames<'a>,
    index: usize,
}

impl EventSink<AiRunEvent> for RowSink<'_, '_> {
    fn emit(&self, event: AiRunEvent) -> bool {
        if let AiRunEvent::Frames { items } = event {
            let mut batcher = self.frames.batcher();
            let ready = batcher.push(self.index, items, Instant::now());
            self.frames.emit(ready);
        }
        true
    }

    fn is_active(&self) -> bool {
        self.frames.sink.is_active()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn frame(index: u32) -> CapturedFrame {
        CapturedFrame {
            index,
            t_ms: u64::from(index),
            event: "message".to_owned(),
            data: format!("d{index}"),
        }
    }

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn three_fast_rows_get_at_most_one_batch_per_row_and_window_and_keep_the_tail() {
        let base = Instant::now();
        let window = Duration::from_millis(33);
        let mut batcher = FrameBatcher::new(window);
        let mut seen: [Vec<Vec<CapturedFrame>>; 3] = Default::default();
        let mut total = [0_u32; 3];
        // One frame per row per millisecond for 200 ms, polled with the same synthetic clock.
        for ms in 0..200_u64 {
            for (row, count) in total.iter_mut().enumerate() {
                *count += 1;
                let ready = batcher.push(row, vec![frame(*count)], at(base, ms));
                for (index, items) in ready {
                    seen[index].push(items);
                }
            }
        }
        // 200 ms span: windows start at 0, 33, 66, 99, 132, 165, 198 at the latest.
        let windows = 200 / 33 + 1;
        for batches in &seen {
            assert!(
                batches.len() <= windows && batches.len() >= 5,
                "{} batches",
                batches.len()
            );
        }
        // The tail is still pending; the final flush of a row hands it over.
        for (row, batches) in seen.iter_mut().enumerate() {
            let tail = batcher.take_row(row).unwrap();
            assert!(!tail.is_empty());
            batches.push(tail);
            let indexes: Vec<u32> = batches.iter().flatten().map(|f| f.index).collect();
            assert_eq!(indexes, (1..=total[row]).collect::<Vec<_>>(), "row {row}");
            assert!(batcher.take_row(row).is_none());
        }
    }

    #[test]
    fn held_back_frames_are_released_by_a_poll_after_the_window() {
        let base = Instant::now();
        let mut batcher = FrameBatcher::new(Duration::from_millis(33));
        assert_eq!(batcher.push(0, vec![frame(1)], base).len(), 1);
        assert!(batcher.push(0, vec![frame(2)], at(base, 10)).is_empty());
        assert!(batcher.push(1, vec![frame(1)], at(base, 20)).is_empty());
        assert!(batcher.poll(at(base, 32)).is_empty());
        let ready = batcher.poll(at(base, 33));
        assert_eq!(
            ready
                .iter()
                .map(|(row, items)| (*row, items.len()))
                .collect::<Vec<_>>(),
            [(0, 1), (1, 1)]
        );
        assert!(batcher.poll(at(base, 90)).is_empty(), "nothing pending");
    }
}
