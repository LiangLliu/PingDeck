//! The streaming variant of the safe transport: the response head first, then the body chunk by
//! chunk, for long answers such as the SSE stream of a custom AI target.
//!
//! Three limits apply: the head must arrive within `first_byte`; no progress may stay away longer
//! than `idle` (any chunk is progress, unless the reader reports events with
//! [`SafeStream::note_event`], see [`SafeStream::idle_by_events`]); and a non-streaming call may
//! be capped as a whole (`total`). A cancel token ends the transfer at once. The stream never
//! follows a redirect and never caps the body: the reader decides how much it keeps.

use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;

use super::SafeTransportError;
use spanbay_base::cancel::{CancelToken, until_cancelled};

/// The time limits of one streaming call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamLimits {
    /// Longest wait for the response head (connection setup included).
    pub first_byte: Duration,
    /// Longest silence between two body chunks (and before the first one); for an event stream
    /// read with [`SafeStream::idle_by_events`], between two events.
    pub idle: Duration,
    /// Cap on the whole call, from its start to the last byte; `None` for streams.
    pub total: Option<Duration>,
}

impl StreamLimits {
    /// `timeout` as the first-byte limit and the silent gap of a stream.
    #[must_use]
    pub const fn stream(timeout: Duration) -> Self {
        Self {
            first_byte: timeout,
            idle: timeout,
            total: None,
        }
    }

    /// `timeout` as the limit of the whole call (a non-streaming request).
    #[must_use]
    pub const fn whole(timeout: Duration) -> Self {
        Self {
            first_byte: timeout,
            idle: timeout,
            total: Some(timeout),
        }
    }
}

/// An open response: status and headers are known, the body is read with
/// [`SafeStream::next_chunk`].
pub struct SafeStream {
    /// Status code (a 3xx is returned as is, never followed).
    pub status: u16,
    /// Response headers with lower-case names, in arrival order.
    pub headers: Vec<(String, String)>,
    /// Resolver time of the host name; `None` for an IP literal, for `localhost`/`*.localhost`,
    /// or when a proxy resolved it.
    pub dns: Option<Duration>,
    /// Connect time of the call's own connection, TLS handshake included (`None`: not measured,
    /// for example behind a proxy).
    pub connect: Option<Duration>,
    chunks: BoxStream<'static, Result<Bytes, SafeTransportError>>,
    limits: StreamLimits,
    /// When the call began (a std `Instant`: the caller's send time, for example `sent_at` of the
    /// AI run), so the `total` limit runs on wall time.
    started: Instant,
    /// When the last progress was seen (the stream's start, a chunk, or a noted event). A tokio
    /// `Instant`, so tests on paused time can drive the idle limit.
    progress: tokio::time::Instant,
    /// Whether only [`SafeStream::note_event`] counts as progress, not a chunk.
    event_progress: bool,
    cancel: CancelToken,
    done: bool,
}

impl std::fmt::Debug for SafeStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SafeStream")
            .field("status", &self.status)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl SafeStream {
    /// A stream over `chunks`; `started` is when the call began (the `total` limit counts from
    /// it).
    #[must_use]
    pub(crate) fn new(
        status: u16,
        headers: Vec<(String, String)>,
        dns: Option<Duration>,
        chunks: BoxStream<'static, Result<Bytes, SafeTransportError>>,
        limits: StreamLimits,
        started: Instant,
        cancel: CancelToken,
    ) -> Self {
        Self {
            status,
            headers,
            dns,
            connect: None,
            chunks,
            limits,
            started,
            progress: tokio::time::Instant::now(),
            event_progress: false,
            cancel,
            done: false,
        }
    }

    /// The same stream carrying the connect time of its connection.
    #[must_use]
    pub(crate) fn with_connect(mut self, connect: Option<Duration>) -> Self {
        self.connect = connect;
        self
    }

    /// Switches to event-stream idling: from now on a chunk is no progress, only
    /// [`SafeStream::note_event`] is, so SSE keep-alive comments (`: PROCESSING`) cannot hold a
    /// stalled generation open. Plain bodies keep the per-chunk limit.
    pub fn idle_by_events(&mut self) {
        self.event_progress = true;
    }

    /// Reports that the reader got a non-empty event out of the bytes; restarts the idle limit
    /// (see [`SafeStream::idle_by_events`]).
    pub fn note_event(&mut self) {
        self.progress = tokio::time::Instant::now();
    }

    /// The first header named `name` (case-insensitive).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        super::find_header(&self.headers, name)
    }

    /// The next body chunk; `None` once the body ended or a previous call failed.
    ///
    /// # Errors
    /// [`SafeTransportError::Timeout`] after `idle` without progress (or when `total` is spent),
    /// [`SafeTransportError::Cancelled`] when the token fired, or the transfer's own failure.
    pub async fn next_chunk(&mut self) -> Option<Result<Bytes, SafeTransportError>> {
        if self.done {
            return None;
        }
        if !self.event_progress {
            self.progress = tokio::time::Instant::now();
        }
        let mut deadline = self.progress + self.limits.idle;
        if let Some(total) = self.limits.total {
            let left = total.saturating_sub(self.started.elapsed());
            deadline = deadline.min(tokio::time::Instant::now() + left);
        }
        let waiting = tokio::time::timeout_at(deadline, self.chunks.next());
        let next = match until_cancelled(Some(&self.cancel), waiting).await {
            None => Some(Err(SafeTransportError::Cancelled)),
            Some(Err(_)) => Some(Err(SafeTransportError::Timeout)),
            Some(Ok(next)) => next,
        };
        if !matches!(next, Some(Ok(_))) {
            self.done = true;
        }
        next
    }
}

#[cfg(test)]
mod idle_tests;
#[cfg(test)]
mod tests;
