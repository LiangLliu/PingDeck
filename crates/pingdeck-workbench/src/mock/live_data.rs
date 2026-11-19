//! The live data of a running mock server: the in-memory buckets CRUD routes change.
//!
//! Single responsibility: read the buckets next to the data the user saved (the "initial data"
//! of the running configuration) and put one or all of them back. Reads copy at most
//! [`MAX_BUCKET_BYTES`] of JSON per bucket and [`MAX_TOTAL_BYTES`] in all, so a large bucket
//! cannot flood the IPC channel; the bucket lock is the one every CRUD request takes, so it is
//! held only while one bucket is compared and serialised.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::server::{MockServerManager, lock};
use crate::workspace::items::{MockDataBucket, MockServer};

/// Most JSON text sent for one bucket.
pub const MAX_BUCKET_BYTES: usize = 1024 * 1024;

/// Most JSON text sent for all buckets of one server.
pub const MAX_TOTAL_BYTES: usize = 4 * 1024 * 1024;

/// One bucket as it is in memory now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MockLiveBucket {
    /// The bucket's id (`MockDataBucket::id`).
    pub id: String,
    /// The bucket's name.
    pub name: String,
    /// The live value as compact JSON; cut at the cap when `truncated`, empty once the total
    /// budget is spent.
    pub value_json: String,
    /// Array length; `None` for an object.
    pub items: Option<u32>,
    /// The live value differs from the initial data of the running configuration.
    pub changed: bool,
    /// `value_json` is cut (so it is no longer valid JSON) or left out.
    pub truncated: bool,
}

/// Why live data could not be reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MockDataResetError {
    /// No server with this id is running.
    #[error("mock server is not running")]
    NotRunning,
    /// The running configuration has no bucket with this id.
    #[error("unknown data bucket")]
    UnknownBucket,
}

/// The bucket JSON a start (or reset) seeds: the saved text, or `[]` when it does not parse so a
/// half-written bucket cannot stop a server.
pub(super) fn initial_value(bucket: &MockDataBucket) -> Value {
    serde_json::from_str(&bucket.value_json).unwrap_or_else(|_| Value::Array(Vec::new()))
}

/// `text` cut to at most `max` bytes on a character boundary.
fn cut(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

impl MockServerManager {
    /// The running config of `id` and a handle to its state.
    fn live_state(&self, id: &str) -> Option<(Arc<MockServer>, Arc<super::server::ServerState>)> {
        let running = lock(&self.running);
        let state = Arc::clone(&running.get(id)?.state);
        let config = Arc::clone(
            &state
                .config
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        Some((config, state))
    }

    /// The live buckets of a running server in the order of its configuration; `None` when it is
    /// not running.
    #[must_use]
    pub fn live_data(&self, id: &str) -> Option<Vec<MockLiveBucket>> {
        let (config, state) = self.live_state(id)?;
        let mut budget = MAX_TOTAL_BYTES;
        let mut buckets = Vec::with_capacity(config.data_buckets.len());
        for bucket in &config.data_buckets {
            let initial = initial_value(bucket);
            let (items, changed, text) = {
                let live = lock(&state.buckets);
                let Some(value) = live.get(&bucket.id) else {
                    continue;
                };
                let items = match value {
                    Value::Array(items) => Some(u32::try_from(items.len()).unwrap_or(u32::MAX)),
                    _ => None,
                };
                let text = (budget > 0).then(|| serde_json::to_string(value).unwrap_or_default());
                (items, value != &initial, text)
            };
            let skipped = text.is_none();
            let full_len = text.as_ref().map_or(0, String::len);
            let value_json = cut(text.unwrap_or_default(), MAX_BUCKET_BYTES.min(budget));
            budget -= value_json.len();
            buckets.push(MockLiveBucket {
                id: bucket.id.clone(),
                name: bucket.name.clone(),
                truncated: skipped || value_json.len() < full_len,
                value_json,
                items,
                changed,
            });
        }
        Some(buckets)
    }

    /// Puts `bucket_id` (every bucket when `None`) back to the initial data of the running
    /// configuration, which is what the user saved.
    ///
    /// # Errors
    /// [`MockDataResetError::NotRunning`] when the server is stopped,
    /// [`MockDataResetError::UnknownBucket`] for an id the configuration does not have.
    pub fn reset_live_data(
        &self,
        id: &str,
        bucket_id: Option<&str>,
    ) -> Result<(), MockDataResetError> {
        let (config, state) = self.live_state(id).ok_or(MockDataResetError::NotRunning)?;
        if bucket_id.is_some_and(|wanted| config.data_buckets.iter().all(|b| b.id != wanted)) {
            return Err(MockDataResetError::UnknownBucket);
        }
        let mut live = lock(&state.buckets);
        for bucket in &config.data_buckets {
            if bucket_id.is_none_or(|wanted| wanted == bucket.id) {
                live.insert(bucket.id.clone(), initial_value(bucket));
            }
        }
        Ok(())
    }
}
