//! Window path grants for OS drag-drop and native file pickers.
//!
//! Only native drop events and native pickers may record grants; paths are canonicalized
//! off the UI thread on the Tokio blocking pool.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use pingdeck_core::agent::attachments::PathGrants;
use tokio::runtime::Handle;

/// Time-limited (60s) allow-list of paths each window may read for attachment/file import.
#[derive(Clone)]
pub struct ImportGrants {
    inner: Arc<PathGrants>,
    tokio: Handle,
}

impl ImportGrants {
    #[must_use]
    pub fn new(tokio: Handle) -> Self {
        Self {
            inner: Arc::new(PathGrants::default()),
            tokio,
        }
    }

    /// Grants `window` read access to `paths` immediately and canonicalizes them off-thread.
    pub fn grant(&self, window: &str, paths: &[PathBuf]) {
        self.inner.grant(window, paths, Instant::now());
        let grants = Arc::clone(&self.inner);
        self.tokio.spawn_blocking(move || grants.resolve_pending());
    }

    /// Underlying `PathGrants` passed to `pingdeck_core::app::agent_attachment_stage_impl` etc.
    #[must_use]
    pub fn as_path_grants(&self) -> &PathGrants {
        &self.inner
    }
}
