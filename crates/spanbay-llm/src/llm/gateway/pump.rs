//! Streaming a model answer to a gateway client.
//!
//! Single responsibility: pull chunks from the upstream, forward them (verbatim for a
//! same-protocol upstream, re-encoded for a different one), and watch the events for the figures
//! the request log needs: time to first token, token usage, in-stream errors, the text generated.
//! It records the attempt's health and ends the call's [`Journal`] exactly once: when the stream
//! ends, fails, or the client goes away (the body is dropped, which also closes the upstream
//! connection). With a capture on it records the frames of both directions.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::Instant;

use bytes::Bytes;
use futures_util::stream;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Frame;

use crate::llm::adapters::StreamEncoder;
use crate::llm::client::HealthGuard;
use crate::llm::failure::{FailureKind, classify};
use crate::llm::health::Outcome;
use crate::llm::hub::HubEvent;
use crate::llm::upstream::{AttemptError, Chunk, UpstreamStream};

use super::exchange::FrameTap;
use super::journal::{Ended, Journal};
use super::server::GatewayBody;

/// The frame recorders of a captured stream.
struct Taps {
    client: FrameTap,
    /// Only for a translated stream; a verbatim one has the same frames on both sides.
    upstream: Option<FrameTap>,
}

/// Forwards one upstream stream to one client.
pub(super) struct Pump {
    upstream: UpstreamStream,
    buffered: VecDeque<Chunk>,
    /// `Some` when the client speaks a different protocol than the upstream.
    encoder: Option<Box<dyn StreamEncoder>>,
    /// Renders an in-stream error for a verbatim stream.
    error_encoder: Box<dyn StreamEncoder>,
    guard: HealthGuard,
    journal: Journal,
    taps: Option<Taps>,
    stream_error: Option<String>,
    finished: bool,
    /// The health outcome and the log entry have been recorded.
    logged: bool,
}

impl Pump {
    /// A pump over `upstream` (whose already-read `buffered` chunks come first). `encoder` re-encodes
    /// for a client of another protocol; `error_encoder` renders an in-stream error otherwise.
    pub(super) fn new(
        upstream: UpstreamStream,
        buffered: Vec<Chunk>,
        encoder: Option<Box<dyn StreamEncoder>>,
        error_encoder: Box<dyn StreamEncoder>,
        guard: HealthGuard,
        journal: Journal,
    ) -> Self {
        let started = Instant::now();
        let taps = journal.capturing().then(|| Taps {
            client: FrameTap::new(started),
            upstream: encoder.is_some().then(|| FrameTap::new(started)),
        });
        Self {
            upstream,
            buffered: buffered.into(),
            encoder,
            error_encoder,
            guard,
            journal,
            taps,
            stream_error: None,
            finished: false,
            logged: false,
        }
    }

    /// The response body that drives this pump as hyper polls it.
    pub(super) fn into_body(self) -> GatewayBody {
        let frames = stream::unfold(self, |mut pump| async move {
            let bytes = pump.next_bytes().await?;
            Some((Ok::<_, Infallible>(Frame::data(bytes)), pump))
        });
        StreamBody::new(frames).boxed_unsync()
    }

    fn observe(&mut self, event: &HubEvent) {
        match event {
            HubEvent::TextDelta { text } | HubEvent::ReasoningDelta { text } => {
                self.journal.first_content();
                self.journal.generated(text);
            }
            HubEvent::ToolCallStart { .. } => self.journal.first_content(),
            HubEvent::ToolCallDelta { arguments, .. } => self.journal.generated(arguments),
            HubEvent::Usage { usage } => self.journal.add_usage(usage),
            HubEvent::Error { message, .. } => self.stream_error = Some(message.clone()),
            _ => {}
        }
    }

    /// Bytes for the client, and what the capture sees of them.
    fn deliver(&mut self, out: Vec<u8>) -> Option<Bytes> {
        if out.is_empty() {
            return None;
        }
        if let Some(taps) = &mut self.taps {
            taps.client.feed(&out);
        }
        self.journal.sent_to_client(out.len());
        Some(Bytes::from(out))
    }

    async fn next_bytes(&mut self) -> Option<Bytes> {
        while !self.finished {
            let chunk = match self.buffered.pop_front() {
                Some(chunk) => chunk,
                None => match self.upstream.next_chunk().await {
                    Some(Ok(chunk)) => chunk,
                    Some(Err(error)) => return self.fail(&error),
                    None => return self.complete(),
                },
            };
            let mut out: Vec<u8> = Vec::new();
            for event in &chunk.events {
                self.observe(event);
                if let Some(encoder) = &mut self.encoder {
                    out.extend(encoder.push(event));
                }
            }
            if let Some(Taps {
                upstream: Some(tap),
                ..
            }) = &mut self.taps
            {
                tap.feed(&chunk.raw);
            }
            if self.encoder.is_none() {
                out.extend_from_slice(&chunk.raw);
            }
            if let Some(bytes) = self.deliver(out) {
                return Some(bytes);
            }
        }
        None
    }

    /// The upstream ended normally: close the client stream and log.
    fn complete(&mut self) -> Option<Bytes> {
        self.finished = true;
        let tail = self
            .encoder
            .as_mut()
            .map(|encoder| encoder.finish())
            .unwrap_or_default();
        let bytes = self.deliver(tail);
        match self.stream_error.clone() {
            Some(message) => self.finalize(
                Outcome::Failure,
                Ended::Failed {
                    kind: classify(None, &message),
                    error: message,
                    status: 200,
                },
            ),
            None => self.finalize(Outcome::Success, Ended::Success),
        }
        bytes
    }

    /// The upstream broke mid-stream: tell the client in its own protocol and log.
    fn fail(&mut self, error: &AttemptError) -> Option<Bytes> {
        self.finished = true;
        let message = error.message();
        let event = HubEvent::Error {
            message: message.clone(),
            kind: Some("upstream_error".to_owned()),
        };
        // Every break, a clean close without the end marker included, ends with an error frame in
        // the client's protocol after the bytes already forwarded: a client must never take half
        // an answer for a whole one (docs/architecture.md §8).
        let encoder = match &mut self.encoder {
            Some(encoder) => encoder,
            None => &mut self.error_encoder,
        };
        let mut out = encoder.push(&event);
        out.extend(encoder.finish());
        let bytes = self.deliver(out);
        if matches!(error, AttemptError::Cancelled) {
            self.finalize(Outcome::Canceled, Ended::Canceled);
        } else {
            self.finalize(
                Outcome::Failure,
                Ended::Failed {
                    status: 200,
                    error: message,
                    kind: match error.kind() {
                        FailureKind::Unknown => FailureKind::ProviderError,
                        kind => kind,
                    },
                },
            );
        }
        bytes
    }

    fn finalize(&mut self, outcome: Outcome, ended: Ended) {
        if self.logged {
            return;
        }
        self.logged = true;
        self.finished = true;
        self.guard.finish(outcome);
        if let Some(taps) = self.taps.take() {
            self.journal
                .frames(taps.client.finish(), taps.upstream.map(FrameTap::finish));
        }
        self.journal.end(&ended);
    }
}

impl Drop for Pump {
    /// The client went away before the stream ended.
    fn drop(&mut self) {
        self.finalize(Outcome::Canceled, Ended::Canceled);
    }
}
