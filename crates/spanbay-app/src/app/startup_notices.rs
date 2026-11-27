//! What the post-paint start routine wants the user to know (docs/architecture.md §3).
//!
//! Single responsibility: keep the notices of the second start stage (leftover processes
//! stopped, a cleanup step that failed, a gateway that did not start) until the webview asks.
//! The routine finishes after the window is shown and the page may load before or after it, so
//! the page reads [`app_startup_report_impl`] once and also listens for the shell's event.

use std::sync::{Mutex, PoisonError};

use super::AppState;

/// Most notices kept; a longer list keeps its first entries.
pub const MAX_STARTUP_NOTICES: usize = 32;

/// The notices of this process's start routine; `None` until it finished.
#[derive(Debug, Default)]
pub struct StartupNotices(Mutex<Option<Vec<String>>>);

impl StartupNotices {
    /// Records the routine's outcome (bounded to [`MAX_STARTUP_NOTICES`]); a second call
    /// replaces the first.
    pub fn finish(&self, mut notices: Vec<String>) {
        notices.truncate(MAX_STARTUP_NOTICES);
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(notices);
    }

    /// The notices, or `None` while the routine still runs.
    #[must_use]
    pub fn get(&self) -> Option<Vec<String>> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// The start routine's notices in words (English, from the core), empty when it went
/// cleanly; `None` while it still runs.
#[must_use]
pub fn app_startup_report_impl(state: &AppState) -> Option<Vec<String>> {
    state.startup_notices.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_are_absent_until_the_routine_finishes_and_bounded_after() {
        let notices = StartupNotices::default();
        assert_eq!(notices.get(), None);
        notices.finish(
            (0..MAX_STARTUP_NOTICES + 5)
                .map(|n| n.to_string())
                .collect(),
        );
        let kept = notices.get().unwrap();
        assert_eq!(kept.len(), MAX_STARTUP_NOTICES);
        assert_eq!(kept[0], "0");
    }
}
