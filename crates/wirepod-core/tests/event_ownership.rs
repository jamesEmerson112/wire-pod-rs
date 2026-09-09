//! Stim stream ownership and the receive loop. Ports of Go tests 3, 4, 5 and 6:
//! `TestEventStreamNeverHasTwoOwners` (`sdkapp_test.go:253-302`),
//! `TestStopEventStreamEndsReceiverPromptly` (`sdkapp_test.go:306-334`),
//! `TestClaimEventStreamRefusesWhileOwned` (`sdkapp_test.go:338-360`) and
//! `TestSupersededReceiverCannotWriteStimState` (`sdkapp_test.go:364-390`).
//!
//! The two ownership tests need no runtime: the state machine is synchronous by
//! design. The loop tests use a real clock with a two second ceiling that fires
//! only on a regression, and no paused time anywhere. Every await in this file
//! is under that ceiling.
//!
//! The two teardown tests wait for the receiver's own readiness signal before
//! stopping it, so the stop reaches a receiver that is genuinely parked in its
//! receive. Readiness is a oneshot fired by the receiver, never a polling loop.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use wirepod_core::test_support::{FakeReceiver, LiveCounter};
use wirepod_core::{
    ConnError, EVENT_CONNECTION_ID, EVENT_WHITELIST, EventLoopExit, EventOwner, StatusCode,
    StimSample, run_event_stream,
};

/// Wall-clock ceiling for every loop teardown here. It is a regression alarm,
/// not a timing assertion: the fix makes teardown immediate.
const CEILING: Duration = Duration::from_secs(2);

/// The double click: re-selecting Stim must reuse the running stream rather
/// than start a second one, and a begin arriving straight after a stop must not
/// be turned away.
#[test]
fn claim_refuses_while_owned_and_is_admitted_after_a_stop() {
    let owner = EventOwner::new();

    let cancel = CancellationToken::new();
    assert!(owner.claim(cancel.clone()).is_some(), "first claim refused");
    assert!(owner.is_streaming(), "the claim did not mark the stream");

    assert!(
        owner.claim(CancellationToken::new()).is_none(),
        "second claim was admitted while the stream was already owned"
    );

    let stopped = owner
        .stop()
        .expect("stop did not hand back the owner's token");
    assert!(!owner.is_streaming(), "stop left the stream marked");
    assert!(
        !cancel.is_cancelled(),
        "stop cancelled under its own lock instead of handing the token back"
    );
    stopped.cancel();
    assert!(
        cancel.is_cancelled(),
        "the token stop handed back was not the owner's"
    );

    assert!(
        owner.claim(CancellationToken::new()).is_some(),
        "claim refused after a stop; a begin right after a stop must not be turned away"
    );
    assert!(
        owner.stop().is_some(),
        "stop did not report the owner it just took away"
    );
    assert!(
        owner.stop().is_none(),
        "stop reported an owner for a stream nobody holds"
    );
}

#[test]
fn a_superseded_receiver_cannot_write_stim() {
    let owner = EventOwner::new();

    let old = owner
        .claim(CancellationToken::new())
        .expect("first claim refused");
    assert!(
        owner.write_stim(old, StimSample::new(0.25, 1.0)),
        "the owner's write was refused"
    );
    assert_eq!(owner.stim().value, 0.25, "owner write did not land");

    owner
        .stop()
        .expect("stop did not hand back the owner's token")
        .cancel();
    // Go zeroes the value in the same critical section as the release
    // (`robot.go:258`). The Go test never observes it, so this port does.
    assert_eq!(
        owner.stim(),
        StimSample::ZERO,
        "stop left a stale reading behind"
    );

    let new = owner
        .claim(CancellationToken::new())
        .expect("could not claim after stop");
    assert!(
        owner.write_stim(new, StimSample::new(0.75, 1.0)),
        "the new owner's write was refused"
    );

    // The superseded receiver tries to publish one last reading.
    assert!(
        !owner.write_stim(old, StimSample::new(0.1, 1.0)),
        "a superseded receiver's write was accepted"
    );
    assert_eq!(
        owner.stim().value,
        0.75,
        "a superseded receiver overwrote the current owner's value"
    );
}

/// Go test 3. Repeated stop-and-begin cycles must never leave two live stim
/// receivers.
///
/// The receiver ignores cancellation entirely, so the loop's own `select!` is
/// the only thing that can end it, which is a stronger property than the Go
/// test proves against a context-aware fake.
///
/// Each cycle waits for the receiver's readiness signal before stopping it, so
/// the stop arrives while the loop is genuinely parked in its receive. Without
/// that wait the stop lands before the loop has ever been polled, and a loop
/// that sampled cancellation only at the top of its iteration would pass.
#[tokio::test]
async fn begin_and_stop_cycles_never_stack_receivers() {
    let owner = Arc::new(EventOwner::new());
    let counter = Arc::new(LiveCounter::new());

    for cycle in 0..5 {
        let cancel = CancellationToken::new();
        let generation = owner.claim(cancel.clone()).unwrap_or_else(|| {
            panic!("cycle {cycle}: begin was refused, so the previous stop did not free the stream")
        });
        let (receiver, mut handle) = FakeReceiver::counted(Arc::clone(&counter));
        let task = tokio::spawn(run_event_stream(
            Box::new(receiver),
            Arc::clone(&owner),
            generation,
            cancel.clone(),
        ));

        // Go's waitFor at `sdkapp_test.go:278` is a no-op, because the claim
        // marks the stream before the goroutine is spawned. This waits for
        // something the Go test never had: the receiver's own signal that it has
        // reached its first receive.
        timeout(CEILING, handle.ready())
            .await
            .unwrap_or_else(|_| panic!("cycle {cycle}: the receiver never reached its receive"));
        assert!(
            owner.is_streaming(),
            "cycle {cycle}: the claim did not mark the stream"
        );

        // What stop_event_stream does, in order: take the token and release
        // ownership under the lock, then cancel after the unlock
        // (`robot.go:253-263`).
        owner
            .stop()
            .unwrap_or_else(|| panic!("cycle {cycle}: stop found no owner to hand back"))
            .cancel();
        assert!(
            cancel.is_cancelled(),
            "cycle {cycle}: stop handed back a token other than the owner's"
        );

        let exit = timeout(CEILING, task)
            .await
            .unwrap_or_else(|_| panic!("cycle {cycle}: receiver still alive 2s after its stop"))
            .expect("the stim loop panicked");
        assert_eq!(
            exit,
            EventLoopExit::Cancelled,
            "cycle {cycle}: wrong exit reason"
        );
        assert_eq!(
            counter.live(),
            0,
            "cycle {cycle}: a receiver outlived the loop that owned it"
        );
    }

    assert_eq!(
        counter.peak(),
        1,
        "peak concurrent receivers was {}, want 1",
        counter.peak()
    );
    assert!(
        !owner.is_streaming(),
        "the stream is still marked after the final stop"
    );
}

/// Go test 4, the part the old code could not do at all: end a receiver parked
/// in its receive on a robot that is sending nothing.
///
/// The stop is issued only after the receiver has signalled that it reached its
/// receive, so "parked" is a fact rather than an assumption about scheduling.
#[tokio::test]
async fn stop_ends_a_parked_receiver_promptly() {
    let owner = Arc::new(EventOwner::new());
    let cancel = CancellationToken::new();
    let generation = owner
        .claim(cancel.clone())
        .expect("could not claim an unowned stream");

    // Never fed and deaf to cancellation, so it is parked exactly where the old
    // Go code could not get it out of.
    let (receiver, mut handle) = FakeReceiver::new();
    let task = tokio::spawn(run_event_stream(
        Box::new(receiver),
        Arc::clone(&owner),
        generation,
        cancel.clone(),
    ));

    timeout(CEILING, handle.ready())
        .await
        .expect("the receiver never reached its receive");

    owner
        .stop()
        .expect("stop found no owner to hand back")
        .cancel();

    let exit = timeout(CEILING, task)
        .await
        .expect("the receiver did not exit after the stop; it is still parked in its receive")
        .expect("the stim loop panicked");
    assert_eq!(exit, EventLoopExit::Cancelled);
    timeout(CEILING, handle.wait_dropped())
        .await
        .expect("the loop returned without dropping its receiver");

    assert!(
        !owner.is_streaming(),
        "the stream is still marked after the stop"
    );
    assert_eq!(
        owner.stim(),
        StimSample::ZERO,
        "the stop left a stale reading behind"
    );
}

/// The `biased;` in the loop's `select!`. A cancellation that is already
/// pending must win over an event that is already queued, so a stop is never
/// delayed by a robot that is still talking, and the event the loop declined to
/// read is never published.
///
/// Deterministic under the fix: `biased;` polls the cancellation arm first, and
/// an already-cancelled token resolves on that first poll, so this test passes
/// every run. Deleting the `biased;` randomises the arm order per poll and the
/// test then fails most runs but not all, which is the shape of the regression
/// it guards rather than a proof against a single run.
///
/// The token is cancelled directly rather than through a stop, so `generation`
/// still owns the stream. A loop that read the queued event would therefore
/// publish it, and the reading is what makes the miss visible.
#[tokio::test]
async fn a_pending_cancellation_wins_over_a_queued_event() {
    let owner = Arc::new(EventOwner::new());
    let cancel = CancellationToken::new();
    let generation = owner
        .claim(cancel.clone())
        .expect("could not claim an unowned stream");

    let (receiver, handle) = FakeReceiver::new();
    handle.send_stim(0.5, 1.0);
    cancel.cancel();

    let exit = timeout(
        CEILING,
        run_event_stream(
            Box::new(receiver),
            Arc::clone(&owner),
            generation,
            cancel.clone(),
        ),
    )
    .await
    .expect("the loop did not exit on an already-cancelled token");

    assert_eq!(exit, EventLoopExit::Cancelled);
    assert_eq!(
        owner.stim(),
        StimSample::ZERO,
        "the loop read the queued event instead of honouring the cancellation"
    );
}

/// The value-present rule. Go decides a stim value is present with
/// `strings.Contains(fmt.Sprint(stimInfo), "velocity")` (`server.go:656-659`),
/// and proto3 omits zero-valued scalars from the text form, so a zero velocity
/// is skipped. Nothing on the Go side ever executes this arm.
#[tokio::test]
async fn a_zero_velocity_event_is_not_published() {
    let owner = Arc::new(EventOwner::new());
    let generation = owner
        .claim(CancellationToken::new())
        .expect("could not claim an unowned stream");

    let (receiver, handle) = FakeReceiver::new();
    handle.send_other();
    handle.send_stim(0.5, 0.0);
    handle.end();

    let exit = timeout(
        CEILING,
        run_event_stream(
            Box::new(receiver),
            Arc::clone(&owner),
            generation,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the loop did not end when its stream ended");

    assert_eq!(exit, EventLoopExit::StreamEnded);
    assert_eq!(
        owner.stim(),
        StimSample::ZERO,
        "a zero-velocity event was published"
    );
}

#[tokio::test]
async fn a_stim_event_is_published_to_the_owner() {
    let owner = Arc::new(EventOwner::new());
    let generation = owner
        .claim(CancellationToken::new())
        .expect("could not claim an unowned stream");

    let (receiver, handle) = FakeReceiver::new();
    handle.send_stim(0.5, 1.0);
    handle.end();

    let exit = timeout(
        CEILING,
        run_event_stream(
            Box::new(receiver),
            Arc::clone(&owner),
            generation,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the loop did not end when its stream ended");

    assert_eq!(exit, EventLoopExit::StreamEnded);
    assert!(
        !owner.is_streaming(),
        "the loop did not release ownership on the way out"
    );
    // Go's releaseEventStream drops the entry and clears the flag but leaves
    // the reading alone (`robot.go:236-246`); only stopEventStream zeroes it
    // (`robot.go:258`). The value is unobservable while the flag is clear, and
    // this pins the difference at zero.
    assert_eq!(owner.stim(), StimSample::new(0.5, 1.0));
}

#[tokio::test]
async fn a_receive_failure_ends_the_loop_and_releases() {
    let owner = Arc::new(EventOwner::new());
    let generation = owner
        .claim(CancellationToken::new())
        .expect("could not claim an unowned stream");

    let (receiver, handle) = FakeReceiver::new();
    let err = ConnError::new(StatusCode::Unavailable, "transport is closing");
    handle.fail(err.clone());

    let exit = timeout(
        CEILING,
        run_event_stream(
            Box::new(receiver),
            Arc::clone(&owner),
            generation,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the loop did not end when its receive failed");

    assert_eq!(exit, EventLoopExit::Failed(err));
    assert!(
        !owner.is_streaming(),
        "a failed receive did not release ownership"
    );
    assert!(
        owner.claim(CancellationToken::new()).is_some(),
        "a begin after a failure was refused"
    );
}

/// The generation fence on the way out. A loop whose stream ends after it has
/// been superseded must not release, unmark or blank the stream that replaced
/// it (`robot.go:236-246`).
#[tokio::test]
async fn a_superseded_loop_does_not_clear_its_successor() {
    let owner = Arc::new(EventOwner::new());
    let old = owner
        .claim(CancellationToken::new())
        .expect("could not claim an unowned stream");

    let (receiver, handle) = FakeReceiver::new();
    // The loop's token is deliberately not the one the claim stored, so the stop
    // below cannot reach it and loop A stays parked while B takes over.
    let task = tokio::spawn(run_event_stream(
        Box::new(receiver),
        Arc::clone(&owner),
        old,
        CancellationToken::new(),
    ));

    owner
        .stop()
        .expect("stop found no owner to hand back")
        .cancel();
    let new = owner
        .claim(CancellationToken::new())
        .expect("claim refused right after a stop");
    assert!(
        owner.write_stim(new, StimSample::new(0.75, 1.0)),
        "the new owner's write was refused"
    );

    // Only now does the superseded loop's stream end under it.
    handle.end();
    let exit = timeout(CEILING, task)
        .await
        .expect("the superseded loop did not exit when its stream ended")
        .expect("the stim loop panicked");
    assert_eq!(exit, EventLoopExit::StreamEnded);

    assert!(
        owner.is_streaming(),
        "a superseded loop cleared its successor's streaming flag"
    );
    assert_eq!(
        owner.stim().value,
        0.75,
        "a superseded loop cleared its successor's reading"
    );
    assert!(
        owner.claim(CancellationToken::new()).is_none(),
        "a superseded loop released its successor's ownership"
    );
}

/// The two literals that reach the robot on every `begin_event_stream`
/// (`server.go:483-491`).
#[test]
fn the_stim_stream_literals_match_go() {
    assert_eq!(EVENT_WHITELIST, ["stimulation_info"].as_slice());
    assert_eq!(EVENT_CONNECTION_ID, "wirepod");
}

/// The `ConnError` text contract. Go writes `"error: " + err.Error()` into the
/// response body, and for a gRPC failure `err.Error()` is grpc-go's
/// `rpc error: code = <Code> desc = <message>`.
#[test]
fn conn_error_renders_grpc_gos_text() {
    assert_eq!(
        ConnError::new(StatusCode::Unavailable, "connection refused").to_string(),
        "rpc error: code = Unavailable desc = connection refused"
    );
    assert_eq!(
        ConnError::new(StatusCode::Unauthenticated, "invalid token").to_string(),
        "rpc error: code = Unauthenticated desc = invalid token"
    );
    assert_eq!(
        ConnError::deadline_exceeded().to_string(),
        "rpc error: code = DeadlineExceeded desc = context deadline exceeded"
    );
    assert_eq!(
        format!("error: {}", ConnError::deadline_exceeded()),
        "error: rpc error: code = DeadlineExceeded desc = context deadline exceeded"
    );
}
