//! Live counts of long-running operations: flow runs, collection runs and test-bench runs.
//!
//! Single responsibility: answer "how much work is running?" for the quit/close confirm.
//! A run holds a [`RunGuard`] for its whole duration; the guard decrements the count on
//! drop, whether the run finishes, fails, is cancelled or its future is dropped. A single request
//! execution is not a run.

use std::sync::atomic::{AtomicU32, Ordering};

use super::{AppState, AppStateInner};

/// The kinds of run the quit confirm counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    /// A flow run (`flow_run`).
    Flow,
    /// A collection run (`run_collection`).
    Collection,
    /// A test-bench run (`llm_probe_run` and the context probe).
    Bench,
}

/// In-flight run counters, one per [`RunKind`].
#[derive(Debug, Default)]
pub struct RunActivity {
    flows: AtomicU32,
    collections: AtomicU32,
    bench: AtomicU32,
}

impl RunActivity {
    fn counter(&self, kind: RunKind) -> &AtomicU32 {
        match kind {
            RunKind::Flow => &self.flows,
            RunKind::Collection => &self.collections,
            RunKind::Bench => &self.bench,
        }
    }
}

/// Keeps one run counted while it is alive.
#[must_use = "the run is only counted while the guard is held"]
pub struct RunGuard {
    state: AppState,
    kind: RunKind,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.state
            .run_activity
            .counter(self.kind)
            .fetch_sub(1, Ordering::AcqRel);
    }
}

impl AppState {
    /// Counts a run of `kind` until the returned guard is dropped.
    pub fn track_run(&self, kind: RunKind) -> RunGuard {
        self.run_activity
            .counter(kind)
            .fetch_add(1, Ordering::AcqRel);
        RunGuard {
            state: self.clone(),
            kind,
        }
    }
}

impl AppStateInner {
    /// Flow runs in flight.
    #[must_use]
    pub fn active_flow_runs(&self) -> u32 {
        self.run_activity.flows.load(Ordering::Acquire)
    }

    /// Collection runs in flight.
    #[must_use]
    pub fn active_collection_runs(&self) -> u32 {
        self.run_activity.collections.load(Ordering::Acquire)
    }

    /// Test-bench runs in flight.
    #[must_use]
    pub fn active_bench_runs(&self) -> u32 {
        self.run_activity.bench.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guard_counts_its_run_until_it_is_dropped() {
        let state = AppState::open_in_memory().unwrap();
        assert_eq!(
            (
                state.active_flow_runs(),
                state.active_collection_runs(),
                state.active_bench_runs()
            ),
            (0, 0, 0)
        );
        let flow = state.track_run(RunKind::Flow);
        let second_flow = state.track_run(RunKind::Flow);
        let bench = state.track_run(RunKind::Bench);
        assert_eq!(
            (
                state.active_flow_runs(),
                state.active_collection_runs(),
                state.active_bench_runs()
            ),
            (2, 0, 1)
        );
        drop(flow);
        drop(bench);
        assert_eq!(
            (state.active_flow_runs(), state.active_bench_runs()),
            (1, 0)
        );
        drop(second_flow);
        assert_eq!(state.active_flow_runs(), 0);
    }
}
