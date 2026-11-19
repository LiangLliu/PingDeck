//! Config sync runtime held by `AppState`: the last export outcome and the debounced background
//! exporter. Config writes in `pingdeck-app`, `pingdeck-app-llm` and `pingdeck-app-agent` call
//! [`SyncRuntime::notify_changed`]; `pingdeck-app-sync`, above all three, installs the export job
//! as a closure through [`SyncRuntime::install_auto_exporter`] and records manual exports.

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::Duration;

use crate::app::current_time_ms;
use crate::error::AppError;

/// Default quiet window before an automatic config export fires.
pub const AUTO_EXPORT_DEBOUNCE: Duration = Duration::from_secs(2);

/// Outcome of the latest export, shared between the app and the auto-export thread.
#[derive(Debug, Default)]
pub struct ExportLog {
    /// Unix milliseconds when the last export finished successfully.
    pub last_export_at_ms: Option<i64>,
    /// Error message of the last failed export, if any.
    pub last_error: Option<String>,
}

/// What the export thread is told.
enum Message {
    /// A config entity changed.
    Changed,
    /// Run the pending export now (if any), then signal the sender (tests).
    Flush(Sender<()>),
}

/// Owns the background thread that runs the export job. Dropping it ends the thread.
pub struct AutoExporter {
    wake: Sender<Message>,
}

impl AutoExporter {
    /// Starts the thread. `job` returns whether it exported (`false`: auto-export is off).
    pub fn new(
        debounce: Duration,
        log: Arc<Mutex<ExportLog>>,
        job: impl Fn() -> Result<bool, AppError> + Send + 'static,
    ) -> Self {
        let (wake, notices) = mpsc::channel::<Message>();
        // A failed spawn leaves the sender with no receiver: notifications are then dropped and
        // manual export still works, which is better than failing the whole app start.
        let _ = thread::Builder::new()
            .name("pingdeck-sync-export".to_owned())
            .spawn(move || {
                while let Ok(first) = notices.recv() {
                    // Without a pending change a flush has nothing to run.
                    let mut flushed = match first {
                        Message::Changed => None,
                        Message::Flush(done) => {
                            let _ = done.send(());
                            continue;
                        }
                    };
                    // Quiet period: every notice inside the window restarts it.
                    loop {
                        match notices.recv_timeout(debounce) {
                            Ok(Message::Changed) => {}
                            Ok(Message::Flush(done)) => {
                                flushed = Some(done);
                                break;
                            }
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    let outcome = job();
                    {
                        let mut log = log.lock().unwrap_or_else(PoisonError::into_inner);
                        match outcome {
                            Ok(true) => {
                                log.last_export_at_ms = Some(current_time_ms());
                                log.last_error = None;
                            }
                            Ok(false) => {}
                            Err(error) => log.last_error = Some(error.message().to_owned()),
                        }
                    }
                    if let Some(done) = flushed {
                        let _ = done.send(());
                    }
                }
            });
        Self { wake }
    }

    /// Wakes the thread; a no-op when it is gone.
    pub fn notify(&self) {
        let _ = self.wake.send(Message::Changed);
    }

    /// Runs the pending export immediately and returns once it and its log update are done, so
    /// tests never wait on the debounce timer.
    pub fn flush(&self) {
        let (done, finished) = mpsc::channel();
        if self.wake.send(Message::Flush(done)).is_ok() {
            let _ = finished.recv();
        }
    }
}

/// Config sync state of an app: the last export outcome and the debounced auto-exporter.
#[derive(Default)]
pub struct SyncRuntime {
    log: Arc<Mutex<ExportLog>>,
    auto: OnceLock<AutoExporter>,
}

impl SyncRuntime {
    /// Creates an unattached runtime; [`SyncRuntime::install_auto_exporter`] wires background
    /// exports for disk-backed databases.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches the background auto-export job once. Subsequent calls are no-ops.
    pub fn install_auto_exporter(
        &self,
        debounce: Duration,
        job: impl Fn() -> Result<bool, AppError> + Send + 'static,
    ) {
        let _ = self
            .auto
            .get_or_init(|| AutoExporter::new(debounce, Arc::clone(&self.log), job));
    }

    /// Tells the auto-exporter a config entity changed. Cheap and never blocks: a burst of calls
    /// becomes one export once the changes stop for the debounce period.
    pub fn notify_changed(&self) {
        if let Some(auto) = self.auto.get() {
            auto.notify();
        }
    }

    /// Records the outcome of an export.
    pub fn record(&self, outcome: &Result<(), String>, at_ms: i64) {
        let mut log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
        match outcome {
            Ok(()) => {
                log.last_export_at_ms = Some(at_ms);
                log.last_error = None;
            }
            Err(message) => log.last_error = Some(message.clone()),
        }
    }

    /// `(last export time, last error)`.
    #[must_use]
    pub fn last(&self) -> (Option<i64>, Option<String>) {
        let log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
        (log.last_export_at_ms, log.last_error.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::{AutoExporter, ExportLog};

    const NEVER: Duration = Duration::from_secs(3600);

    #[test]
    fn auto_exporter_coalesces_notices_and_flushes_on_demand() {
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&runs);
        let log = Arc::new(std::sync::Mutex::new(ExportLog::default()));
        let exporter = AutoExporter::new(NEVER, Arc::clone(&log), move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        });
        exporter.flush();
        assert_eq!(runs.load(Ordering::SeqCst), 0);
        for _ in 0..20 {
            exporter.notify();
        }
        exporter.flush();
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert!(log.lock().unwrap().last_export_at_ms.is_some());
        exporter.notify();
        exporter.flush();
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }
}
