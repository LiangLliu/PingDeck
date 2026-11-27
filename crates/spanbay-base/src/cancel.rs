//! Cooperative cancellation: a cloneable token, a future that resolves when it
//! fires, a race helper, and the registry that maps UI-supplied ids to tokens.
//!
//! Single responsibility: let a caller abort an in-flight operation promptly.
//! The token wraps `tokio_util`'s `CancellationToken`, which needs no runtime to
//! poll; the operations it interrupts (`reqwest`) still need a Tokio reactor.

use std::{
    collections::{HashMap, hash_map::Entry},
    future::Future,
    sync::{Arc, Mutex, PoisonError},
};

use tokio_util::sync::{CancellationToken, WaitForCancellationFuture};

use crate::error::AppError;

/// Message shown when an operation was aborted by the user.
pub const CANCELLED_MESSAGE: &str = "Request cancelled";

/// Cloneable flag that wakes every waiter when [`CancelToken::cancel`] runs.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(CancellationToken);

/// Future returned by [`CancelToken::cancelled`].
pub type Cancelled<'a> = WaitForCancellationFuture<'a>;

impl CancelToken {
    /// Creates a token that has not been cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks the token cancelled and wakes every pending [`Cancelled`] future.
    pub fn cancel(&self) {
        self.0.cancel();
    }

    /// A token that cancels when `self` does, or on its own without touching `self`.
    #[must_use]
    pub fn child(&self) -> Self {
        Self(self.0.child_token())
    }

    /// Whether [`CancelToken::cancel`] has run.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }

    /// Future that resolves once the token is cancelled.
    pub fn cancelled(&self) -> Cancelled<'_> {
        self.0.cancelled()
    }
}

/// Runs `future` until it finishes or `token` fires; `None` means cancelled.
///
/// A token that is already cancelled returns `None` without polling `future`.
pub async fn until_cancelled<F: Future>(
    token: Option<&CancelToken>,
    future: F,
) -> Option<F::Output> {
    match token {
        None => Some(future.await),
        Some(token) if token.is_cancelled() => None,
        Some(token) => token.0.run_until_cancelled(future).await,
    }
}

/// Maps UI-supplied cancel ids to the tokens of the operations they started.
#[derive(Debug, Default)]
pub struct CancellationRegistry {
    tokens: Mutex<HashMap<String, CancelToken>>,
}

impl CancellationRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an operation. Without an id the token is private to the
    /// caller and can never be cancelled by id; an id that is already
    /// registered is rejected so one operation cannot hijack another's token.
    pub fn register(&self, id: Option<&str>) -> Result<CancelGuard<'_>, AppError> {
        let (id, token) = self.insert(id)?;
        Ok(CancelGuard {
            registry: self,
            id,
            token,
        })
    }

    /// Like [`CancellationRegistry::register`], but the guard owns a handle to the registry, so
    /// it can move into a spawned task that outlives the caller's borrow.
    pub fn register_owned(
        self: &Arc<Self>,
        id: Option<&str>,
    ) -> Result<OwnedCancelGuard, AppError> {
        let (id, token) = self.insert(id)?;
        Ok(OwnedCancelGuard {
            registry: Arc::clone(self),
            id,
            token,
        })
    }

    fn insert(&self, id: Option<&str>) -> Result<(Option<String>, CancelToken), AppError> {
        let token = CancelToken::new();
        let id = id
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        if let Some(id) = id.as_ref() {
            match self.lock().entry(id.clone()) {
                Entry::Occupied(_) => {
                    return Err(AppError::conflict(
                        "an operation with this cancel token is already running",
                    ));
                }
                Entry::Vacant(slot) => {
                    slot.insert(token.clone());
                }
            }
        }
        Ok((id, token))
    }

    /// Cancels the operation registered under `id`, returning whether one was
    /// still running.
    pub fn cancel(&self, id: &str) -> bool {
        let token = self.lock().get(id.trim()).cloned();
        token.is_some_and(|token| {
            token.cancel();
            true
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, CancelToken>> {
        self.tokens.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Keeps an operation registered; removes the entry when dropped.
#[must_use = "dropping the guard immediately unregisters the operation"]
pub struct CancelGuard<'a> {
    registry: &'a CancellationRegistry,
    id: Option<String>,
    token: CancelToken,
}

impl CancelGuard<'_> {
    /// Token the operation must observe.
    #[must_use]
    pub fn token(&self) -> &CancelToken {
        &self.token
    }
}

impl Drop for CancelGuard<'_> {
    fn drop(&mut self) {
        if let Some(id) = self.id.as_ref() {
            // `register` rejects duplicate ids, so the entry under `id` is always ours.
            self.registry.lock().remove(id);
        }
    }
}

/// A [`CancelGuard`] that owns its registry handle (`'static`), for background tasks.
#[must_use = "dropping the guard immediately unregisters the operation"]
pub struct OwnedCancelGuard {
    registry: Arc<CancellationRegistry>,
    id: Option<String>,
    token: CancelToken,
}

impl OwnedCancelGuard {
    /// Token the operation must observe.
    #[must_use]
    pub fn token(&self) -> &CancelToken {
        &self.token
    }
}

impl Drop for OwnedCancelGuard {
    fn drop(&mut self) {
        if let Some(id) = self.id.as_ref() {
            self.registry.lock().remove(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;
    use tokio::time::sleep;

    #[tokio::test]
    async fn cancelling_wakes_a_pending_future_promptly() {
        let token = CancelToken::new();
        let remote = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            remote.cancel();
        });

        let outcome = until_cancelled(Some(&token), sleep(Duration::from_secs(30))).await;

        assert!(outcome.is_none());
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn a_finished_future_wins_and_an_absent_token_never_cancels() {
        let token = CancelToken::new();
        assert_eq!(until_cancelled(Some(&token), async { 7 }).await, Some(7));
        assert_eq!(until_cancelled(None, async { 9 }).await, Some(9));
    }

    #[tokio::test]
    async fn an_already_cancelled_token_skips_the_future() {
        let token = CancelToken::new();
        token.cancel();
        let ran = AtomicBool::new(false);
        let outcome = until_cancelled(Some(&token), async {
            ran.store(true, Ordering::SeqCst);
        })
        .await;
        assert!(outcome.is_none());
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[test]
    fn registry_cancels_by_id_and_forgets_finished_operations() {
        let registry = CancellationRegistry::new();
        let guard = registry.register(Some("send-1")).unwrap();
        let token = guard.token().clone();

        assert!(registry.cancel("send-1"));
        assert!(token.is_cancelled());

        drop(guard);
        assert!(!registry.cancel("send-1"));
        assert!(!registry.cancel("never-registered"));
    }

    #[test]
    fn a_duplicate_id_is_rejected_until_the_first_guard_drops() {
        let registry = CancellationRegistry::new();
        let first = registry.register(Some("dup")).unwrap();

        let error = registry.register(Some(" dup ")).err();
        assert_eq!(
            error,
            Some(AppError::conflict(
                "an operation with this cancel token is already running"
            ))
        );
        assert!(!first.token().is_cancelled());

        drop(first);
        assert!(registry.register(Some("dup")).is_ok());
    }

    #[test]
    fn operations_without_an_id_are_private() {
        let registry = CancellationRegistry::new();
        let guard = registry.register(None).unwrap();
        assert!(!registry.cancel(""));
        assert!(!guard.token().is_cancelled());
    }

    #[test]
    fn a_child_token_follows_its_parent_but_not_the_other_way() {
        let parent = CancelToken::new();
        let first = parent.child();
        let second = parent.child();
        first.cancel();
        assert!(first.is_cancelled() && !parent.is_cancelled() && !second.is_cancelled());
        parent.cancel();
        assert!(second.is_cancelled());
        // A child of a token that is already cancelled starts cancelled.
        assert!(parent.child().is_cancelled());
    }

    #[test]
    fn a_grandchild_is_cancelled_through_a_dropped_intermediate_handle() {
        let root = CancelToken::new();
        let grandchild = root.child().child();
        root.cancel();
        assert!(grandchild.is_cancelled());
    }

    #[tokio::test]
    async fn a_child_wakes_a_pending_future_when_the_parent_cancels() {
        let parent = CancelToken::new();
        let child = parent.child();
        let remote = parent.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(20)).await;
            remote.cancel();
        });
        assert!(
            until_cancelled(Some(&child), sleep(Duration::from_secs(30)))
                .await
                .is_none()
        );
    }

    #[test]
    fn an_owned_guard_moves_across_threads_and_unregisters_on_drop() {
        let registry = Arc::new(CancellationRegistry::new());
        let guard = registry.register_owned(Some("run-1")).unwrap();
        assert!(registry.register_owned(Some("run-1")).is_err());
        let token = guard.token().clone();
        std::thread::spawn(move || drop(guard)).join().unwrap();
        assert!(!registry.cancel("run-1"));
        assert!(!token.is_cancelled());
        assert!(registry.register_owned(Some("run-1")).is_ok());
    }
}
