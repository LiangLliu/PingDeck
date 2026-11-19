//! Tests of the idle limit of `SafeStream` on paused time: a plain body restarts it with every
//! chunk, an event stream only with an event the reader reports.

use std::time::{Duration, Instant};

use futures_util::stream;
use tokio::time::Instant as TokioInstant;

use super::*;

/// Yields a keep-alive comment after each delay (seconds), then stays silent forever.
fn script(steps: &[u64]) -> BoxStream<'static, Result<Bytes, SafeTransportError>> {
    let delays = steps.to_vec();
    stream::unfold(delays.into_iter(), |mut delays| async move {
        match delays.next() {
            Some(delay) => {
                tokio::time::sleep(Duration::from_secs(delay)).await;
                Some((Ok(Bytes::from_static(b": PROCESSING\n\n")), delays))
            }
            None => std::future::pending().await,
        }
    })
    .boxed()
}

fn open(delays: &[u64], idle_s: u64) -> SafeStream {
    let limits = StreamLimits {
        first_byte: Duration::from_secs(60),
        idle: Duration::from_secs(idle_s),
        total: None,
    };
    SafeStream::new(
        200,
        Vec::new(),
        None,
        script(delays),
        limits,
        Instant::now(),
        CancelToken::new(),
    )
}

#[tokio::test(start_paused = true)]
async fn a_plain_body_restarts_the_idle_limit_with_every_chunk() {
    let start = TokioInstant::now();
    let mut body = open(&[10, 10, 10, 10], 15);
    for _ in 0..4 {
        assert!(body.next_chunk().await.unwrap().is_ok());
    }
    let error = body.next_chunk().await.unwrap().unwrap_err();
    assert!(matches!(error, SafeTransportError::Timeout), "{error:?}");
    assert_eq!(start.elapsed(), Duration::from_secs(55));
}

#[tokio::test(start_paused = true)]
async fn keep_alive_chunks_of_an_event_stream_do_not_restart_the_idle_limit() {
    let start = TokioInstant::now();
    let mut events = open(&[10, 10, 10, 10], 25);
    events.idle_by_events();
    assert!(events.next_chunk().await.unwrap().is_ok());
    assert!(events.next_chunk().await.unwrap().is_ok());
    let error = events.next_chunk().await.unwrap().unwrap_err();
    assert!(matches!(error, SafeTransportError::Timeout), "{error:?}");
    assert_eq!(start.elapsed(), Duration::from_secs(25));
    assert!(events.next_chunk().await.is_none(), "the stream is over");
}

#[tokio::test(start_paused = true)]
async fn a_reported_event_restarts_the_idle_limit_of_an_event_stream() {
    let start = TokioInstant::now();
    let mut events = open(&[20, 20, 20], 25);
    events.idle_by_events();
    assert!(events.next_chunk().await.unwrap().is_ok());
    events.note_event();
    assert!(events.next_chunk().await.unwrap().is_ok());
    events.note_event();
    assert!(events.next_chunk().await.unwrap().is_ok());
    // No event was reported for the third chunk, so 25 s after the second event it ends.
    let error = events.next_chunk().await.unwrap().unwrap_err();
    assert!(matches!(error, SafeTransportError::Timeout), "{error:?}");
    assert_eq!(start.elapsed(), Duration::from_secs(65));
}
