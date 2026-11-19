//! Reading the body of an AI answer: an event stream decoded frame by frame, or a plain body.
//!
//! Single responsibility: pull chunks from the opened response until it ends, a limit passes or
//! the run is stopped, keeping everything seen so far (a partial answer is a result). For a
//! stream it parses the SSE frames, decodes them with the protocol's stream
//! decoder, notes the first content event (the TTFT mark), and sends the frames to the sink
//! through a coalescer (at most one `Frames` event per 33 ms, so at most 30 per second).

use std::time::{Duration, Instant};

use bytes::Bytes;

use super::types::AiRunEvent;
use crate::llm::adapters::{ChatCodec, StreamDecoder};
use crate::llm::capture::FrameRecorder;
use crate::llm::capture_record::{CapturedFrame, CapturedFrames};
use crate::llm::hub::{HubEvent, HubFinish};
use crate::llm::safe_http::SafeTransportError;
use crate::llm::safe_http::stream::SafeStream;
use crate::llm::sse::{SseEvent, SseParser};
use pingdeck_model::events::{EventSink, StreamCoalescer};

/// How reading ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Ending {
    /// The body ended (or reached the response cap).
    Complete,
    /// The run was stopped.
    Stopped,
    /// A limit passed.
    TimedOut,
    /// The transfer failed after the head.
    Broke(SafeTransportError),
    /// A frame is not valid for the protocol.
    BadStream(String),
}

/// What was read.
pub(super) struct Reading {
    /// The body is an event stream.
    pub streamed: bool,
    /// Every body byte read, up to the response cap.
    pub raw: Vec<u8>,
    /// The cap cut the body.
    pub truncated: bool,
    /// Every frame of a stream.
    pub frames: Vec<CapturedFrame>,
    /// The frames as the capture keeps them (`None` unless the exchange is kept).
    pub recorded: Option<CapturedFrames>,
    /// Every decoded event of a stream.
    pub events: Vec<HubEvent>,
    /// The finish event, when one arrived (a cut stream has none).
    pub finish: Option<HubFinish>,
    /// When the first content event (text, reasoning, tool-call start) was read.
    pub first_content: Option<Instant>,
    /// When the last byte was read.
    pub last_byte: Instant,
    /// How it ended.
    pub ending: Ending,
}

impl Reading {
    fn new(streamed: bool, now: Instant) -> Self {
        Self {
            streamed,
            raw: Vec::new(),
            truncated: false,
            frames: Vec::new(),
            recorded: None,
            events: Vec::new(),
            finish: None,
            first_content: None,
            last_byte: now,
            ending: Ending::Complete,
        }
    }
}

fn ending_of(error: SafeTransportError) -> Ending {
    match error {
        SafeTransportError::Timeout => Ending::TimedOut,
        SafeTransportError::Cancelled => Ending::Stopped,
        other => Ending::Broke(other),
    }
}

/// Reads at most `cap` bytes of a plain body (an answer, or an error message).
pub(super) async fn read_body(mut stream: SafeStream, cap: usize) -> Reading {
    let mut reading = Reading::new(false, Instant::now());
    while let Some(next) = stream.next_chunk().await {
        match next {
            Ok(chunk) => {
                reading.last_byte = Instant::now();
                if !take(&mut reading.raw, &chunk, cap) {
                    reading.truncated = true;
                    break;
                }
            }
            Err(error) => {
                reading.ending = ending_of(error);
                break;
            }
        }
    }
    reading
}

/// Appends `chunk` to `raw` up to `cap` bytes; `false` when the cap cut it.
fn take(raw: &mut Vec<u8>, chunk: &[u8], cap: usize) -> bool {
    let room = cap.saturating_sub(raw.len());
    if chunk.len() > room {
        raw.extend_from_slice(&chunk[..room]);
        return false;
    }
    raw.extend_from_slice(chunk);
    true
}

type Item = Option<Result<Bytes, SafeTransportError>>;

async fn next_owned(mut stream: SafeStream) -> (SafeStream, Item) {
    let item = stream.next_chunk().await;
    (stream, item)
}

fn is_delta(event: &AiRunEvent) -> bool {
    matches!(event, AiRunEvent::Frames { .. })
}

fn merge(previous: &mut AiRunEvent, next: AiRunEvent) -> Result<(), AiRunEvent> {
    match (previous, next) {
        (AiRunEvent::Frames { items }, AiRunEvent::Frames { items: more }) => {
            items.extend(more);
            Ok(())
        }
        (_, next) => Err(next),
    }
}

/// The per-stream state: parser, decoder and what they produced.
struct Collector {
    parser: SseParser,
    decoder: Box<dyn StreamDecoder>,
    recorder: Option<FrameRecorder>,
    reading: Reading,
    /// The instant the request was sent: frame offsets (`t_ms`) count from here, so the first
    /// frame carries the time to first byte. The frame recorder rebases to its first frame.
    clock: Instant,
}

impl Collector {
    fn frame(&mut self, sse: &SseEvent, now: Instant, out: &mut Vec<CapturedFrame>) {
        let frame = CapturedFrame {
            index: u32::try_from(self.reading.frames.len() + 1).unwrap_or(u32::MAX),
            t_ms: millis(now.saturating_duration_since(self.clock)),
            event: sse
                .event
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "message".to_owned()),
            data: sse.data.clone(),
        };
        if let Some(recorder) = &mut self.recorder {
            recorder.push(
                millis(now.saturating_duration_since(self.clock)),
                sse.event.as_deref(),
                &sse.data,
            );
        }
        out.push(frame.clone());
        self.reading.frames.push(frame);
    }

    fn decode(&mut self, events: Vec<HubEvent>, now: Instant) {
        for event in events {
            match &event {
                HubEvent::TextDelta { text } | HubEvent::ReasoningDelta { text }
                    if !text.is_empty() =>
                {
                    self.reading.first_content.get_or_insert(now);
                }
                HubEvent::ToolCallStart { .. } => {
                    self.reading.first_content.get_or_insert(now);
                }
                HubEvent::Finish { reason } => self.reading.finish = Some(reason.clone()),
                _ => {}
            }
            self.reading.events.push(event);
        }
    }

    /// Parses and decodes `sses`; `Err` when a frame is not valid for the protocol.
    fn absorb(
        &mut self,
        sses: Vec<SseEvent>,
        now: Instant,
        out: &mut Vec<CapturedFrame>,
    ) -> Result<(), String> {
        for sse in sses {
            self.frame(&sse, now, out);
            let decoded = self
                .decoder
                .push(&sse)
                .map_err(|error| error.message().to_owned())?;
            self.decode(decoded, now);
        }
        Ok(())
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Reads an event stream: frames go to `sink` coalesced, content marks the TTFT, and whatever
/// arrived before the end is kept. `clock` is the instant the request was sent (frame offsets).
pub(super) async fn read_stream(
    mut stream: SafeStream,
    codec: &dyn ChatCodec,
    cap: usize,
    keep_frames: bool,
    clock: Instant,
    sink: &dyn EventSink<AiRunEvent>,
) -> Reading {
    let mut collector = Collector {
        parser: SseParser::new(),
        decoder: codec.stream_decoder(),
        recorder: keep_frames.then(FrameRecorder::new),
        reading: Reading::new(true, Instant::now()),
        clock,
    };
    let mut coalescer = StreamCoalescer::new(is_delta, merge);
    let live = sink.is_active();
    // Keep-alive comments are bytes but no event: only events keep the idle limit alive.
    stream.idle_by_events();
    let mut next = Box::pin(next_owned(stream));
    loop {
        let due = coalescer.next_deadline();
        let step = tokio::select! {
            (stream, item) = &mut next => Some((stream, item)),
            () = tokio::time::sleep_until(
                tokio::time::Instant::from_std(due.unwrap_or_else(Instant::now))
            ), if due.is_some() => None,
        };
        let Some((mut stream, item)) = step else {
            if let Some(event) = coalescer.poll(Instant::now()) {
                sink.emit(event);
            }
            continue;
        };
        let now = Instant::now();
        match item {
            Some(Ok(chunk)) => {
                collector.reading.last_byte = now;
                let fits = take(&mut collector.reading.raw, &chunk, cap);
                let mut frames = Vec::new();
                let parsed = collector.parser.feed(&chunk);
                let progressed = !parsed.is_empty();
                if let Err(message) = collector.absorb(parsed, now, &mut frames) {
                    collector.reading.ending = Ending::BadStream(message);
                }
                if live {
                    for frame in frames {
                        for event in coalescer.push(AiRunEvent::Frames { items: vec![frame] }, now)
                        {
                            sink.emit(event);
                        }
                    }
                }
                if !fits {
                    collector.reading.truncated = true;
                    break;
                }
                if collector.reading.ending != Ending::Complete {
                    break;
                }
                if progressed {
                    stream.note_event();
                }
                next = Box::pin(next_owned(stream));
            }
            Some(Err(error)) => {
                collector.reading.ending = ending_of(error);
                break;
            }
            None => {
                let mut frames = Vec::new();
                let rest = collector.parser.finish();
                let closed = collector.absorb(rest, now, &mut frames).and_then(|()| {
                    let closing = collector
                        .decoder
                        .finish()
                        .map_err(|error| error.message().to_owned())?;
                    collector.decode(closing, now);
                    Ok(())
                });
                if let Err(message) = closed {
                    collector.reading.ending = Ending::BadStream(message);
                }
                if live {
                    for frame in frames {
                        for event in coalescer.push(AiRunEvent::Frames { items: vec![frame] }, now)
                        {
                            sink.emit(event);
                        }
                    }
                }
                break;
            }
        }
    }
    // The terminal event follows this flush at once.
    if let Some(event) = coalescer.flush() {
        sink.emit(event);
    }
    collector.reading.recorded = collector.recorder.map(FrameRecorder::finish);
    collector.reading
}
