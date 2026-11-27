//! Tests of `UpstreamStream` over scripted byte streams on paused time: the idle limit counts
//! SSE events (not keep-alive bytes), `prime` is bounded, and an undecodable frame is the
//! upstream's retryable fault.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use tokio::time::Instant;

use super::*;

type Raw = BoxStream<'static, Result<Bytes, reqwest::Error>>;

const COMMENT: &str = ": PROCESSING\n\n";
const EVENT: &str =
    "data: {\"id\":\"c\",\"model\":\"m\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
const USAGE_ONLY: &str =
    "data: {\"id\":\"c\",\"model\":\"m\",\"usage\":{\"prompt_tokens\":1,\"total_tokens\":1}}\n\n";

/// Yields each `(delay, text)` after its delay, then stays silent forever.
fn script(steps: Vec<(Duration, String)>) -> Raw {
    stream::unfold(steps.into_iter(), |mut steps| async move {
        match steps.next() {
            Some((delay, text)) => {
                tokio::time::sleep(delay).await;
                Some((Ok(Bytes::from(text)), steps))
            }
            None => std::future::pending().await,
        }
    })
    .boxed()
}

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

fn upstream(steps: Vec<(Duration, String)>, idle: Duration) -> UpstreamStream {
    UpstreamStream::over(script(steps), Protocol::OpenaiChat, idle, None).unwrap()
}

#[tokio::test(start_paused = true)]
async fn keep_alive_comments_do_not_restart_the_idle_limit() {
    let start = Instant::now();
    let steps = (0..10).map(|_| (secs(10), COMMENT.to_owned())).collect();
    let mut stream = upstream(steps, secs(25));
    assert!(stream.next_chunk().await.unwrap().is_ok());
    assert!(stream.next_chunk().await.unwrap().is_ok());
    let error = stream.next_chunk().await.unwrap().unwrap_err();
    assert!(matches!(error, AttemptError::Timeout), "{error:?}");
    assert_eq!(start.elapsed(), secs(25));
}

#[tokio::test(start_paused = true)]
async fn an_event_restarts_the_idle_limit() {
    let start = Instant::now();
    let steps = vec![
        (secs(20), EVENT.to_owned()),
        (secs(10), COMMENT.to_owned()),
        (secs(10), COMMENT.to_owned()),
        (secs(10), COMMENT.to_owned()),
    ];
    let mut stream = upstream(steps, secs(25));
    for _ in 0..3 {
        assert!(stream.next_chunk().await.unwrap().is_ok());
    }
    // The comments after the event at t=20 give 20 + 25 = 45 s; the third one would arrive at 50.
    let error = stream.next_chunk().await.unwrap().unwrap_err();
    assert!(matches!(error, AttemptError::Timeout), "{error:?}");
    assert_eq!(start.elapsed(), secs(45));
}

#[tokio::test(start_paused = true)]
async fn prime_gives_up_on_a_stream_of_usage_events_without_content() {
    let steps = (0..MAX_PRIME_CHUNKS + 10)
        .map(|_| (Duration::ZERO, USAGE_ONLY.to_owned()))
        .collect();
    let mut stream = upstream(steps, secs(30));
    let error = stream.prime(secs(600)).await.unwrap_err();
    assert!(matches!(error, AttemptError::BadBody(_)), "{error:?}");
    assert!(error.retryable());
    assert!(error.message().contains("without any content"), "{error:?}");
}

#[tokio::test(start_paused = true)]
async fn prime_gives_up_on_keep_alive_bytes_past_the_byte_cap() {
    let comment = format!(": {}\n\n", "x".repeat(128 * 1024));
    let steps = (0..40).map(|_| (Duration::ZERO, comment.clone())).collect();
    let mut stream = upstream(steps, secs(30));
    let error = stream.prime(secs(600)).await.unwrap_err();
    assert!(matches!(error, AttemptError::BadBody(_)), "{error:?}");
}

#[tokio::test(start_paused = true)]
async fn prime_still_commits_at_the_first_content_event() {
    let steps = vec![
        (Duration::ZERO, COMMENT.to_owned()),
        (Duration::ZERO, USAGE_ONLY.to_owned()),
        (Duration::ZERO, EVENT.to_owned()),
    ];
    let mut stream = upstream(steps, secs(30));
    let buffered = stream.prime(secs(600)).await.unwrap();
    assert_eq!(buffered.len(), 3);
}

#[tokio::test(start_paused = true)]
async fn a_frame_the_decoder_rejects_is_a_retryable_bad_body() {
    let steps = vec![(Duration::ZERO, "data: <html>oops</html>\n\n".to_owned())];
    let mut stream = upstream(steps, secs(30));
    let error = stream.prime(secs(600)).await.unwrap_err();
    assert!(matches!(error, AttemptError::BadBody(_)), "{error:?}");
    assert!(error.retryable());
    assert_eq!(error.kind(), FailureKind::ProtocolMismatch);
}

#[test]
fn bad_json_and_a_bad_body_are_the_upstreams_fault_and_retryable() {
    let error = parse_json(b"<html>captive portal</html>").unwrap_err();
    assert!(matches!(error, AttemptError::BadBody(_)), "{error:?}");
    assert!(error.retryable());
    assert!(
        !AttemptError::Invalid(AppError::validation("local")).retryable(),
        "a local fault stays final"
    );
}

fn ending(frames: &[&str]) -> UpstreamStream {
    let frames: Vec<Result<Bytes, reqwest::Error>> = frames
        .iter()
        .map(|frame| Ok(Bytes::from(frame.to_string())))
        .collect();
    UpstreamStream::over(
        stream::iter(frames).boxed(),
        Protocol::OpenaiChat,
        secs(30),
        None,
    )
    .unwrap()
}

// A stream that closes cleanly after the third frame without its end marker ends with a
// truncated error, never as a complete answer; one with its end closes normally.
#[tokio::test(start_paused = true)]
async fn a_clean_close_without_the_end_marker_is_a_truncated_answer() {
    let mut cut = ending(&[EVENT, EVENT, EVENT]);
    let mut finishes = 0;
    let error = loop {
        match cut.next_chunk().await {
            Some(Ok(chunk)) => {
                finishes += chunk
                    .events
                    .iter()
                    .filter(|event| matches!(event, HubEvent::Finish { .. }))
                    .count();
            }
            Some(Err(error)) => break error,
            None => panic!("a cut stream must not end cleanly"),
        }
    };
    assert_eq!(finishes, 0);
    assert_eq!(error.kind(), FailureKind::Truncated);
    assert!(cut.next_chunk().await.is_none());

    let done = "data: {\"id\":\"c\",\"model\":\"m\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    let mut whole = ending(&[EVENT, done]);
    while let Some(chunk) = whole.next_chunk().await {
        assert!(chunk.is_ok());
    }

    // A compatible upstream that ends with `[DONE]` and never sends a finish reason is complete.
    let mut bare = ending(&[EVENT, "data: [DONE]\n\n"]);
    while let Some(chunk) = bare.next_chunk().await {
        assert!(chunk.is_ok());
    }
}
