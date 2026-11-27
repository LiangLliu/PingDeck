//! In-memory hold for deleted things that an Undo toast can still bring back.
//!
//! Single responsibility: keep a removed value (an environment, a request subtree), secret
//! values included, until its hard limit or one second after the toast is dismissed, and drop it
//! on time so secrets never outlive the hold. Shared by `environment_service` and
//! `tree_service`.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::workspace::WorkspaceId;

use super::factory_service::new_entity_id;

/// The longest a deleted value stays restorable, whatever the toast does.
const UNDO_LIMIT: Duration = Duration::from_secs(60);

/// How long a deleted value stays restorable after its toast is dismissed.
const UNDO_GRACE: Duration = Duration::from_secs(1);

/// Opaque handle of a deleted value that a restore use-case can bring back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UndoToken(pub String);

/// A value an [`UndoStore`] can hold: it knows the workspace that may restore it.
pub(crate) trait UndoHeld: Clone {
    /// The workspace whose id a restore must present.
    fn workspace_id(&self) -> &WorkspaceId;
}

struct UndoEntry<T> {
    token: String,
    workspace_id: WorkspaceId,
    value: T,
    created_at: Instant,
    expires_at: Instant,
}

struct UndoState<T> {
    entries: Vec<UndoEntry<T>>,
    timer_running: bool,
    closed: bool,
}

struct UndoShared<T> {
    state: Mutex<UndoState<T>>,
    wake: Condvar,
}

impl<T> UndoShared<T> {
    fn lock(&self) -> MutexGuard<'_, UndoState<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Deleted values held in memory for the Undo toast.
///
/// An entry lives until its hard limit, `UNDO_LIMIT` (60 s) after the delete; dismissing the
/// toast ([`Self::commit_at`]) shortens that to `UNDO_GRACE` (1 s) after the dismissal. Every
/// method takes the current instant, so tests drive expiry with synthetic instants. Expired
/// entries are dropped on every access, and [`Self::arm_timer`] keeps one timer thread that drops
/// them on time, so secret values leave memory even when nothing touches the store again.
pub(crate) struct UndoStore<T: Send + 'static> {
    shared: Arc<UndoShared<T>>,
}

impl<T: Send + 'static> Default for UndoStore<T> {
    fn default() -> Self {
        Self {
            shared: Arc::new(UndoShared {
                state: Mutex::new(UndoState {
                    entries: Vec::new(),
                    timer_running: false,
                    closed: false,
                }),
                wake: Condvar::new(),
            }),
        }
    }
}

impl<T: UndoHeld + Send + 'static> UndoStore<T> {
    /// Holds `value` until `now + 60 s` and returns its token.
    pub(crate) fn insert_at(&self, now: Instant, value: T) -> UndoToken {
        let token = new_entity_id("undo");
        let mut state = self.shared.lock();
        state.entries.retain(|entry| entry.expires_at > now);
        state.entries.push(UndoEntry {
            token: token.clone(),
            workspace_id: value.workspace_id().clone(),
            value,
            created_at: now,
            expires_at: now + UNDO_LIMIT,
        });
        UndoToken(token)
    }

    /// The toast of `token` was dismissed at `now`: it expires `UNDO_GRACE` later, and never
    /// after its 60 s limit. A later dismissal never extends it. Returns whether a live entry
    /// of that token existed.
    pub(crate) fn commit_at(&self, now: Instant, token: &str) -> bool {
        let mut state = self.shared.lock();
        state.entries.retain(|entry| entry.expires_at > now);
        let Some(entry) = state.entries.iter_mut().find(|entry| entry.token == token) else {
            return false;
        };
        entry.expires_at = entry
            .expires_at
            .min(now + UNDO_GRACE)
            .min(entry.created_at + UNDO_LIMIT);
        true
    }

    /// The held value of `token` in `workspace_id`, if it has not expired. The entry stays until
    /// [`Self::discard`], so a restore that fails can be retried.
    pub(crate) fn peek_at(
        &self,
        now: Instant,
        workspace_id: &WorkspaceId,
        token: &str,
    ) -> Option<T> {
        let mut state = self.shared.lock();
        state.entries.retain(|entry| entry.expires_at > now);
        state
            .entries
            .iter()
            .find(|entry| entry.token == token && &entry.workspace_id == workspace_id)
            .map(|entry| entry.value.clone())
    }

    /// Drops the entry of `token`.
    pub(crate) fn discard(&self, token: &str) {
        self.shared
            .lock()
            .entries
            .retain(|entry| entry.token != token);
    }

    /// Drops every entry that expired at `now`.
    pub(crate) fn purge_at(&self, now: Instant) {
        self.shared
            .lock()
            .entries
            .retain(|entry| entry.expires_at > now);
    }

    /// How many entries are held (tests: proves the timer or a purge dropped them).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.shared.lock().entries.len()
    }

    /// Makes sure a timer thread is waiting for the earliest expiry, and wakes it so it re-reads
    /// the deadlines after an insert or a dismissal. The thread ends when no entry is left or the
    /// store is dropped. A thread that cannot be spawned is harmless: every access purges too.
    pub(crate) fn arm_timer(&self) {
        let mut state = self.shared.lock();
        if state.closed || state.entries.is_empty() {
            return;
        }
        if state.timer_running {
            self.shared.wake.notify_all();
            return;
        }
        state.timer_running = true;
        let shared = Arc::clone(&self.shared);
        if std::thread::Builder::new()
            .name("spanbay-undo".into())
            .spawn(move || Self::run_timer(&shared))
            .is_err()
        {
            state.timer_running = false;
        }
    }

    fn run_timer(shared: &UndoShared<T>) {
        let mut state = shared.lock();
        loop {
            let now = Instant::now();
            state.entries.retain(|entry| entry.expires_at > now);
            let next = state.entries.iter().map(|entry| entry.expires_at).min();
            let Some(next) = next.filter(|_| !state.closed) else {
                state.timer_running = false;
                return;
            };
            state = shared
                .wake
                .wait_timeout(state, next.saturating_duration_since(now))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

impl<T: Send + 'static> Drop for UndoStore<T> {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.entries.clear();
        state.closed = true;
        self.shared.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct Held(WorkspaceId);

    impl UndoHeld for Held {
        fn workspace_id(&self) -> &WorkspaceId {
            &self.0
        }
    }

    fn ws(id: &str) -> WorkspaceId {
        WorkspaceId::new(id).unwrap()
    }

    #[test]
    fn a_later_dismissal_never_extends_and_peek_keeps_the_entry_until_discard() {
        let store = UndoStore::default();
        let t0 = Instant::now();
        let secs = Duration::from_secs;
        let token = store.insert_at(t0, Held(ws("ws1")));

        assert!(store.commit_at(t0 + secs(10), &token.0));
        assert!(store.commit_at(t0 + Duration::from_millis(10_500), &token.0));
        assert!(
            store
                .peek_at(t0 + Duration::from_millis(10_999), &ws("ws1"), &token.0)
                .is_some()
        );
        assert_eq!(store.peek_at(t0 + secs(11), &ws("ws1"), &token.0), None);

        // A dismissal near the hard limit cannot push the entry past it.
        let late = store.insert_at(t0, Held(ws("ws1")));
        assert!(store.commit_at(t0 + Duration::from_millis(59_800), &late.0));
        assert_eq!(store.peek_at(t0 + secs(60), &ws("ws1"), &late.0), None);

        let kept = store.insert_at(t0, Held(ws("ws1")));
        assert_eq!(store.peek_at(t0, &ws("ws2"), &kept.0), None);
        for _ in 0..2 {
            assert_eq!(
                store.peek_at(t0, &ws("ws1"), &kept.0),
                Some(Held(ws("ws1")))
            );
        }
        store.discard(&kept.0);
        assert_eq!(store.peek_at(t0, &ws("ws1"), &kept.0), None);
    }

    #[test]
    fn the_timer_drops_a_dismissed_entry_without_another_access() {
        let store = UndoStore::default();
        let token = store.insert_at(Instant::now(), Held(ws("ws1")));
        assert!(store.commit_at(Instant::now(), &token.0));
        store.arm_timer();

        let deadline = Instant::now() + Duration::from_secs(10);
        while store.len() > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(store.len(), 0, "the held value outlived its grace second");
    }
}
