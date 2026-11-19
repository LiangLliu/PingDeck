//! The capture side of the request path: bounded copies of what crossed the wire.
//!
//! Single responsibility: the small pieces the pipeline hands to the log row's
//! [`RawExchange`](crate::llm::capture::RawExchange): the upstream request as sent
//! ([`Sent`]), a recorder for the SSE frames of one stream direction ([`FrameTap`]) and the header
//! lists. Everything here only copies bounded text (`capture::bound_body`, `FrameRecorder`);
//! redaction happens on the log-writer thread.

use std::time::Instant;

use crate::llm::capture::{BoundedBody, FrameRecorder};
use crate::llm::capture_record::{CapturedFrames, TranslationNote};
use crate::llm::router::Target;
use crate::llm::sse::SseParser;
use crate::llm::wire::{Protocol, request_headers};

/// What one attempt sent upstream, for the capture.
#[derive(Debug, Clone, Default)]
pub(super) struct Sent {
    /// The request URL (the writer masks secrets in it).
    pub(super) url: String,
    /// The request headers, credentials included (the writer redacts them).
    pub(super) headers: Vec<(String, String)>,
    /// The body when it differs from the client's except `model`; `None` for a verbatim forward.
    pub(super) body: Option<BoundedBody>,
    /// The body was re-encoded through the hub format (the row's `translated` flag).
    pub(super) reencoded: bool,
    /// What the re-encoding dropped or changed.
    pub(super) notes: Vec<TranslationNote>,
    /// The canonical hub request the client's body decoded to (small requests only).
    pub(super) hub: Option<serde_json::Value>,
}

impl Sent {
    /// The attempt of `target` over `protocol` (`url` is where it goes). `body` is the bounded copy
    /// of the request body, when it differs from the client's except `model`.
    pub(super) fn of(
        target: &Target,
        protocol: Protocol,
        url: String,
        stream: bool,
        body: Option<BoundedBody>,
    ) -> Self {
        let mut headers = vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            (
                "accept".to_owned(),
                if stream {
                    "text/event-stream"
                } else {
                    "application/json"
                }
                .to_owned(),
            ),
        ];
        headers.extend(request_headers(
            protocol,
            target.auth_for(protocol),
            &target.secret,
            &target.quirks,
        ));
        Self {
            url,
            headers,
            body,
            ..Self::default()
        }
    }
}

/// Records the SSE frames of one direction of a stream as the bytes pass.
pub(super) struct FrameTap {
    started: Instant,
    parser: SseParser,
    recorder: FrameRecorder,
}

impl FrameTap {
    /// A tap for a stream that began at `started`.
    pub(super) fn new(started: Instant) -> Self {
        Self {
            started,
            parser: SseParser::new(),
            recorder: FrameRecorder::new(),
        }
    }

    /// Takes the next network chunk.
    pub(super) fn feed(&mut self, bytes: &[u8]) {
        let at = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        for event in self.parser.feed(bytes) {
            self.recorder.push(at, event.event.as_deref(), &event.data);
        }
    }

    /// The frames seen, after the parser's last partial frame.
    pub(super) fn finish(mut self) -> CapturedFrames {
        let at = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        for event in self.parser.finish() {
            self.recorder.push(at, event.event.as_deref(), &event.data);
        }
        self.recorder.finish()
    }
}
