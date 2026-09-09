//! The camera frame pump.
//!
//! Go has no test for its frame loop, so none of this is a port. What it pins is
//! the accounting and the exit rules the loop at `server.go:752-788` relies on:
//! a frame is counted before anything looks at it, an undecodable frame is
//! counted and skipped rather than dropped or fatal, and a receive error,
//! a clean end, a cancellation and a client that has gone away each end the pump
//! with their own reason.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use wirepod_core::test_support::{FakeFrameStream, RecordingSink};
use wirepod_core::{CamMeter, ConnError, FrameOutcome, PumpExit, StatusCode, cam_stream_pump};

/// Real time, so it only ever fires on a regression.
const CEILING: Duration = Duration::from_secs(2);

async fn within<F: Future>(operation: F) -> F::Output {
    tokio::time::timeout(CEILING, operation)
        .await
        .expect("the pump did not finish inside the ceiling")
}

/// Go counts the frame before it decodes it, because a frame that fails to
/// decode still crossed the wire and the measurement is of the link rather than
/// of the picture (`server.go:770-776`). The sink reads the meter as it is
/// handed each frame, so a count that moved afterwards shows up as a zero.
#[tokio::test]
async fn a_frame_is_counted_before_the_sink_sees_it() {
    let meter = Arc::new(CamMeter::new());
    let (sink, log) = RecordingSink::new();
    let mut sink = sink.watching(Arc::clone(&meter));
    let (mut frames, handle) = FakeFrameStream::new();

    handle.send_frame(vec![1, 2, 3]);
    handle.send_frame(vec![4, 5]);
    handle.end();

    let exit = within(cam_stream_pump(
        &mut frames,
        &meter,
        &mut sink,
        CancellationToken::new(),
    ))
    .await;

    assert_eq!(exit, PumpExit::StreamEnded);
    assert_eq!(
        log.readings(),
        vec![(3, 1), (5, 2)],
        "the meter had not counted the frame the sink was being handed"
    );
    assert_eq!(meter.read(), (5, 2));
    assert_eq!(log.frames(), vec![vec![1, 2, 3], vec![4, 5]]);
}

/// An undecodable frame is counted and produces nothing on the wire, and the
/// pump carries on, which is Go's `continue` at `server.go:777-782`.
#[tokio::test]
async fn a_skipped_frame_is_counted_and_the_pump_carries_on() {
    let meter = Arc::new(CamMeter::new());
    let (sink, log) = RecordingSink::new();
    let mut sink = sink.watching(Arc::clone(&meter));
    let (mut frames, handle) = FakeFrameStream::new();

    log.script(FrameOutcome::Sent);
    log.script(FrameOutcome::Skipped);
    handle.send_frame(vec![1, 2, 3]);
    handle.send_frame(vec![4, 5, 6, 7]);
    handle.send_frame(vec![8]);
    handle.end();

    let exit = within(cam_stream_pump(
        &mut frames,
        &meter,
        &mut sink,
        CancellationToken::new(),
    ))
    .await;

    assert_eq!(
        exit,
        PumpExit::StreamEnded,
        "a skipped frame ended the pump instead of being skipped"
    );
    assert_eq!(
        log.outcomes(),
        vec![
            FrameOutcome::Sent,
            FrameOutcome::Skipped,
            FrameOutcome::Sent
        ]
    );
    assert_eq!(
        meter.read(),
        (8, 3),
        "the skipped frame was not counted, or a later frame was not reached"
    );
}

#[tokio::test]
async fn a_robot_that_closes_the_feed_ends_the_pump() {
    let meter = CamMeter::new();
    let (mut sink, log) = RecordingSink::new();
    let (mut frames, handle) = FakeFrameStream::new();

    handle.end();

    let exit = within(cam_stream_pump(
        &mut frames,
        &meter,
        &mut sink,
        CancellationToken::new(),
    ))
    .await;

    assert_eq!(exit, PumpExit::StreamEnded);
    assert_eq!(log.frame_count(), 0);
    assert_eq!(meter.read(), (0, 0));
}

/// A receive error ends the pump rather than re-entering the loop, which is
/// what stops a persistent error spinning it hot (`server.go:762-768`).
#[tokio::test]
async fn a_receive_error_ends_the_pump() {
    let meter = CamMeter::new();
    let (mut sink, log) = RecordingSink::new();
    let (mut frames, handle) = FakeFrameStream::new();

    let failure = ConnError::new(StatusCode::Unavailable, "the robot dropped the stream");
    handle.send_frame(vec![9, 9]);
    handle.fail(failure.clone());

    let exit = within(cam_stream_pump(
        &mut frames,
        &meter,
        &mut sink,
        CancellationToken::new(),
    ))
    .await;

    assert_eq!(exit, PumpExit::StreamError(failure));
    assert_eq!(log.frame_count(), 1);
    assert_eq!(meter.read(), (2, 1));
}

/// A robot that is docked or asleep sends no frames at all, so the only thing
/// that can end the pump is the token. This is the case Go's handler needs its
/// cancellable child context for (`server.go:716-722`).
#[tokio::test]
async fn a_cancelled_token_ends_a_pump_that_is_waiting_for_a_frame() {
    let meter = Arc::new(CamMeter::new());
    let cancel = CancellationToken::new();
    let (mut sink, log) = RecordingSink::new();
    let (mut frames, _handle) = FakeFrameStream::new();

    let pump = tokio::spawn({
        let meter = Arc::clone(&meter);
        let cancel = cancel.clone();
        async move { cam_stream_pump(&mut frames, &meter, &mut sink, cancel).await }
    });

    cancel.cancel();

    assert_eq!(
        within(pump).await.expect("the pump task panicked"),
        PumpExit::Cancelled
    );
    assert_eq!(log.frame_count(), 0);
}

/// The browser going away is its own exit, and the frame that discovered it is
/// still counted, because it crossed the wire before anyone tried to write it.
#[tokio::test]
async fn a_closed_sink_ends_the_pump() {
    let meter = CamMeter::new();
    let (mut sink, log) = RecordingSink::new();
    let (mut frames, handle) = FakeFrameStream::new();

    log.close();
    handle.send_frame(vec![1, 2, 3]);
    handle.send_frame(vec![4, 5, 6]);

    let exit = within(cam_stream_pump(
        &mut frames,
        &meter,
        &mut sink,
        CancellationToken::new(),
    ))
    .await;

    assert_eq!(exit, PumpExit::SinkClosed);
    assert_eq!(log.frame_count(), 0);
    assert_eq!(meter.read(), (3, 1));
}
