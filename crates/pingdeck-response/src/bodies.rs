//! Bounded in-memory store for response bodies too large to ship inline over IPC.
//!
//! Single responsibility: keep the full bytes of big responses so a shell can fetch them by an
//! opaque handle (as raw bytes, no JSON/base64 detour) while the use-case result carries only a
//! preview. The store is an LRU bounded by total size only (no entry count), so a run of
//! moderately large bodies stays retrievable until the byte budget is spent; an evicted handle
//! yields a clear `NotFound`.

use std::{
    collections::VecDeque,
    sync::{Mutex, PoisonError},
};

use bytes::Bytes;

use pingdeck_base::error::AppError;

/// Bodies at or below this size travel inline in the use-case result.
pub const INLINE_BODY_LIMIT_BYTES: usize = 1024 * 1024;
/// Size of the inline preview that accompanies a stored body.
pub const BODY_PREVIEW_BYTES: usize = 64 * 1024;
/// Most bytes kept at once across all stored bodies.
pub const MAX_STORED_BYTES: usize = 64 * 1024 * 1024;

/// LRU store of response bodies, keyed by an opaque handle.
#[derive(Debug)]
pub struct BodyStore {
    inner: Mutex<Inner>,
    max_bytes: usize,
}

#[derive(Debug, Default)]
struct Inner {
    /// Least recently used first: handle, bytes and the response's Content-Type.
    entries: VecDeque<(String, Bytes, Option<String>)>,
    total_bytes: usize,
    next_id: u64,
}

impl Default for BodyStore {
    fn default() -> Self {
        Self::with_byte_limit(MAX_STORED_BYTES)
    }
}

impl BodyStore {
    /// A store with the standard byte budget ([`MAX_STORED_BYTES`]).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A store holding at most `max_bytes` bytes across all bodies.
    #[must_use]
    pub fn with_byte_limit(max_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            max_bytes,
        }
    }

    /// Stores `body` and returns its handle, evicting the least recently used bodies until the
    /// byte budget holds. A body larger than the whole budget is still kept alone.
    pub fn put(&self, body: Bytes) -> String {
        self.put_with_content_type(body, None)
    }

    /// [`BodyStore::put`] keeping the response's Content-Type, so a text view can decode the
    /// bytes with the charset it declares.
    pub fn put_with_content_type(&self, body: Bytes, content_type: Option<String>) -> String {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.next_id += 1;
        let handle = format!("body_{}", inner.next_id);
        inner.total_bytes += body.len();
        inner
            .entries
            .push_back((handle.clone(), body, content_type));
        while inner.entries.len() > 1 && inner.total_bytes > self.max_bytes {
            if let Some((_, evicted, _)) = inner.entries.pop_front() {
                inner.total_bytes -= evicted.len();
            }
        }
        handle
    }

    /// The bytes behind `handle`, marking it most recently used. The returned [`Bytes`] shares
    /// the stored buffer, so this does not copy.
    pub fn get(&self, handle: &str) -> Result<Bytes, AppError> {
        self.get_with_content_type(handle).map(|(body, _)| body)
    }

    /// [`BodyStore::get`] plus the Content-Type stored with the body.
    pub fn get_with_content_type(&self, handle: &str) -> Result<(Bytes, Option<String>), AppError> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let position = inner
            .entries
            .iter()
            .position(|(candidate, _, _)| candidate == handle)
            .ok_or_else(|| {
                AppError::not_found(format!(
                    "response body {handle} is no longer available (it was evicted or belongs to an earlier session); send the request again"
                ))
            })?;
        let entry = inner
            .entries
            .remove(position)
            .ok_or_else(|| AppError::internal("response body store lost an entry"))?;
        let found = (entry.1.clone(), entry.2.clone());
        inner.entries.push_back(entry);
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn many_small_bodies_stay_until_the_byte_budget_evicts_the_least_recently_used() {
        let store = BodyStore::with_byte_limit(100);
        // Twenty bodies fit the budget: no entry count evicts any of them.
        let handles: Vec<_> = (0..20)
            .map(|_| store.put(Bytes::from_static(b"12345")))
            .collect();
        assert!(handles.iter().all(|handle| store.get(handle).is_ok()));

        // Touching the first makes the second the eviction candidate once the budget overflows.
        assert!(store.get(&handles[0]).is_ok());
        let extra = store.put(Bytes::from_static(b"9"));
        assert!(
            store
                .get(&handles[1])
                .unwrap_err()
                .to_string()
                .contains("no longer available")
        );
        assert!(store.get(&handles[0]).is_ok() && store.get(&extra).is_ok());
    }

    #[test]
    fn a_single_oversized_body_is_still_kept_and_unknown_handles_are_not_found() {
        let store = BodyStore::with_byte_limit(4);
        let handle = store.put(Bytes::from(vec![1_u8; 100]));
        assert_eq!(store.get(&handle).unwrap().len(), 100);
        assert!(matches!(
            store.get("body_999"),
            Err(AppError::NotFound { .. })
        ));
    }
}
