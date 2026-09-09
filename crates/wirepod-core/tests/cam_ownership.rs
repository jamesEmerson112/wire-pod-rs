//! Camera ownership and the handoff. Ports of Go test 2,
//! `TestReleaseCamStreamIgnoresSupersededOwner` (`sdkapp_test.go:131-164`), and
//! Go test 1, `TestCamStreamHandoffKeepsCameraOn` (`sdkapp_test.go:66-127`).
//!
//! The state machine itself is synchronous by design, so the ownership tests
//! need no runtime. The handoff tests do, and they replace Go's 60 randomised
//! iterations with the interleavings that actually exist once the camera
//! operation lock is in place: the replacement claims while the departing
//! handler is still running, the replacement is inside its enable when the
//! departing handler queues behind it, and the departing handler gets there
//! first. All three must end with the camera on, which is the invariant Go
//! states at `sdkapp_test.go:60-61`. The probabilistic form is kept behind
//! `#[ignore]` at the bottom.
//!
//! No test pauses the clock. Deadlines are real and small, and the only
//! wall-clock element is a ceiling that fires on a regression.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use wirepod_core::test_support::RecordingCamera;
use wirepod_core::{CamOwner, SdkSession, Timings, start_cam_stream};

/// Nothing here waits on anything real, so this only ever fires on a
/// regression.
const CEILING: Duration = Duration::from_secs(2);

/// Zero settle, and an enable deadline long enough that only a deliberately
/// parked camera reaches it.
fn timings() -> Timings {
    Timings {
        enable: Duration::from_secs(5),
        ..Timings::instant()
    }
}

/// The same, with a settle no test can afford to pay, so paying it is a
/// timeout rather than a slow test.
fn timings_with_an_unaffordable_settle() -> Timings {
    Timings {
        settle: Duration::from_secs(30),
        ..timings()
    }
}

async fn within<F: Future>(operation: F) -> F::Output {
    tokio::time::timeout(CEILING, operation)
        .await
        .expect("the operation did not finish inside the ceiling")
}

#[test]
fn release_ignores_a_superseded_owner() {
    let owner = CamOwner::new();

    let (first, displaced) = owner.claim(CancellationToken::new());
    assert!(
        displaced.is_none(),
        "first claim reported displacing an owner that did not exist"
    );

    let (second, displaced) = owner.claim(CancellationToken::new());
    assert!(
        displaced.is_some(),
        "second claim did not report displacing the first"
    );
    assert_ne!(second, first, "second claim reused the first generation");

    assert!(
        !owner.release(first),
        "the displaced handler was allowed to release the new owner's feed"
    );
    assert!(
        owner.is_streaming(),
        "the displaced handler cleared the streaming flag under the new owner"
    );
    assert_eq!(
        owner.current(),
        Some(second),
        "the displaced handler took the registry entry with it"
    );

    assert!(
        owner.release(second),
        "the current owner could not release its own feed"
    );
    assert!(
        !owner.is_streaming(),
        "streaming still set after the owner released"
    );
    assert_eq!(owner.current(), None, "the entry survived its own release");
}

/// The displaced owner's token comes back from the claim itself, taken in the
/// same critical section, because reading it in a second call would let two
/// claims cancel each other's replacement (`robot.go:105-116`).
#[test]
fn a_claim_hands_back_the_token_of_the_owner_it_displaced() {
    let owner = CamOwner::new();
    let first = CancellationToken::new();
    let second = CancellationToken::new();

    let (_, displaced) = owner.claim(first.clone());
    assert!(displaced.is_none());

    let (_, displaced) = owner.claim(second.clone());
    displaced
        .expect("the claim did not hand back the displaced owner's token")
        .cancel();

    assert!(
        first.is_cancelled(),
        "the displaced owner was not cancelled"
    );
    assert!(
        !second.is_cancelled(),
        "the claim cancelled the token it was just given"
    );
}

/// Go's `stopCamStream` clears the flag and cancels the owner but leaves the
/// registry entry in place, because the departing handler's own release is what
/// deletes it and issues the disable (`robot.go:136-144`).
#[test]
fn stop_cancels_the_owner_and_keeps_the_entry() {
    let owner = CamOwner::new();
    let cancel = CancellationToken::new();
    let (generation, _) = owner.claim(cancel.clone());

    owner.stop();
    assert!(!owner.is_streaming(), "stop left the streaming flag set");
    assert!(cancel.is_cancelled(), "stop did not cancel the owner");
    assert_eq!(
        owner.current(),
        Some(generation),
        "stop deleted the owner entry"
    );
    assert!(
        owner.release(generation),
        "the owner could not release after a stop"
    );
}

/// The settle exists to let the robot drop a feed that was just cancelled, so a
/// first claim on an idle robot has nothing to wait for (`server.go:688-692`).
#[tokio::test]
async fn a_first_claim_does_not_pay_the_settle() {
    let session = SdkSession::new();
    let camera = RecordingCamera::new();
    let timings = timings_with_an_unaffordable_settle();

    let guard = within(start_cam_stream(
        &session,
        &camera,
        &timings,
        CancellationToken::new(),
    ))
    .await
    .expect("the first claim failed");

    assert_eq!(camera.calls(), vec![true]);
    assert!(guard.finish(&session, &camera, &timings).await);
    assert_eq!(camera.calls(), vec![true, false]);
}

#[tokio::test]
async fn displacing_a_live_owner_pays_the_settle() {
    let session = SdkSession::new();
    let camera = RecordingCamera::new();
    let timings = timings_with_an_unaffordable_settle();

    let cancel_a = CancellationToken::new();
    let guard_a = within(start_cam_stream(
        &session,
        &camera,
        &timings,
        cancel_a.clone(),
    ))
    .await
    .expect("the first claim failed");

    let replacement = tokio::time::timeout(
        Duration::from_millis(200),
        start_cam_stream(&session, &camera, &timings, CancellationToken::new()),
    )
    .await;

    assert!(
        replacement.is_err(),
        "the replacement claim did not sleep the settle"
    );
    assert!(
        cancel_a.is_cancelled(),
        "the settle ran before the displaced owner was cancelled"
    );
    assert_eq!(
        camera.calls(),
        vec![true],
        "the replacement enabled the camera before the settle"
    );

    assert!(!guard_a.finish(&session, &camera, &timings).await);
}

/// Go test 1, first interleaving: the replacement claims while the departing
/// handler is still reading frames.
#[tokio::test]
async fn a_handoff_under_a_live_owner_keeps_the_camera_on() {
    let session = SdkSession::new();
    let camera = RecordingCamera::new();
    let timings = timings();

    let cancel_a = CancellationToken::new();
    let guard_a = within(start_cam_stream(
        &session,
        &camera,
        &timings,
        cancel_a.clone(),
    ))
    .await
    .expect("the first claim failed");

    let cancel_b = CancellationToken::new();
    let guard_b = within(start_cam_stream(
        &session,
        &camera,
        &timings,
        cancel_b.clone(),
    ))
    .await
    .expect("the replacement claim failed");

    assert!(
        cancel_a.is_cancelled(),
        "the replacement did not cancel the handler it displaced"
    );
    assert!(
        !cancel_b.is_cancelled(),
        "the replacement cancelled its own feed"
    );
    assert_ne!(
        guard_b.generation(),
        guard_a.generation(),
        "the replacement reused the displaced generation"
    );
    assert_eq!(camera.calls(), vec![true, true]);

    assert!(
        !guard_a.finish(&session, &camera, &timings).await,
        "the displaced handler released the replacement's feed"
    );
    assert_eq!(
        camera.calls(),
        vec![true, true],
        "the displaced handler turned the camera off under a live owner"
    );
    assert_eq!(camera.last_call(), Some(true));
    assert_eq!(session.cam.current(), Some(guard_b.generation()));
    assert!(session.cam.is_streaming());

    assert!(
        guard_b.finish(&session, &camera, &timings).await,
        "the owner could not release its own feed"
    );
    assert_eq!(camera.calls(), vec![true, true, false]);
    assert_eq!(session.cam.current(), None);
}

/// Go test 1, second interleaving: the departing handler is already waiting on
/// the operation lock while the replacement is inside its enable. This is the
/// window the lock exists to close, and before Go commit `255a737` it was the
/// one that left a dead stream.
#[tokio::test]
async fn a_handoff_that_queues_behind_the_replacement_keeps_the_camera_on() {
    let session = Arc::new(SdkSession::new());
    let camera = RecordingCamera::new();
    let timings = timings();

    let cancel_a = CancellationToken::new();
    let guard_a = within(start_cam_stream(
        &session,
        &camera,
        &timings,
        cancel_a.clone(),
    ))
    .await
    .expect("the first claim failed");

    let gate = camera.arm_enable_gate();
    let start_b = tokio::spawn({
        let session = Arc::clone(&session);
        let camera = camera.clone();
        async move { start_cam_stream(&session, &camera, &timings, CancellationToken::new()).await }
    });

    // The replacement now holds the operation lock and is parked inside its
    // enable, which is exactly where Go's fake burns its delay.
    within(gate.wait_entered()).await;
    assert!(
        cancel_a.is_cancelled(),
        "the replacement did not cancel the handler it displaced"
    );

    let mut finish_a = tokio::spawn({
        let session = Arc::clone(&session);
        let camera = camera.clone();
        async move { guard_a.finish(&session, &camera, &timings).await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut finish_a)
            .await
            .is_err(),
        "the departing handler ran its release while the replacement held the operation lock"
    );

    let queued = camera.stamp();
    gate.release();

    let guard_b = within(start_b)
        .await
        .expect("the replacement task panicked")
        .expect("the replacement claim failed");
    let released = within(finish_a).await.expect("the departing task panicked");

    assert!(
        !released,
        "the displaced handler released the replacement's feed"
    );
    assert_eq!(
        camera.calls(),
        vec![true, true],
        "the displaced handler turned the camera off under a live owner"
    );
    assert_eq!(camera.last_call(), Some(true));

    let log = camera.call_log();
    assert!(
        log[1].on && log[1].order > queued,
        "the replacement's enable did not land after the departing handler had queued behind it"
    );

    assert!(guard_b.finish(&session, &camera, &timings).await);
    assert_eq!(camera.calls(), vec![true, true, false]);
}

/// Go test 1, third interleaving: the departing handler wins the operation
/// lock, so its disable lands first and the replacement then finds no previous
/// owner, skips the settle and turns the camera back on.
#[tokio::test]
async fn a_handoff_after_a_completed_release_keeps_the_camera_on() {
    let session = SdkSession::new();
    let camera = RecordingCamera::new();
    let timings = timings_with_an_unaffordable_settle();

    let guard_a = within(start_cam_stream(
        &session,
        &camera,
        &timings,
        CancellationToken::new(),
    ))
    .await
    .expect("the first claim failed");
    assert!(guard_a.finish(&session, &camera, &timings).await);

    let guard_b = within(start_cam_stream(
        &session,
        &camera,
        &timings,
        CancellationToken::new(),
    ))
    .await
    .expect("the replacement claim failed");

    assert_eq!(camera.calls(), vec![true, false, true]);
    assert_eq!(camera.last_call(), Some(true));
    assert!(guard_b.finish(&session, &camera, &timings).await);
}

/// The enable is bounded, standing in for the `context.WithTimeout` Go wraps
/// the RPC in (`server.go:671`), and the expiry reads exactly like grpc-go's.
/// A start that fails hands the feed back rather than leaving it claimed with
/// no guard to release it.
#[tokio::test]
async fn an_enable_that_never_answers_maps_to_the_go_deadline_error() {
    let session = SdkSession::new();
    let camera = RecordingCamera::new();
    let timings = Timings {
        enable: Duration::from_millis(20),
        ..Timings::instant()
    };
    let _gate = camera.arm_enable_gate();

    let err = within(start_cam_stream(
        &session,
        &camera,
        &timings,
        CancellationToken::new(),
    ))
    .await
    .expect_err("a parked enable did not time out");

    assert_eq!(
        err.to_string(),
        "rpc error: code = DeadlineExceeded desc = context deadline exceeded"
    );
    assert_eq!(
        session.cam.current(),
        None,
        "a failed start left the feed claimed with nobody to release it"
    );
    assert!(!session.cam.is_streaming());
    assert_eq!(
        camera.calls(),
        vec![false],
        "the failed start did not hand the feed back the way a finish would"
    );
}

/// The probabilistic form of Go's 60 iterations. The deterministic
/// interleavings above are the gate; this exists for anyone who wants to run
/// the race itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "stress variant of the deterministic handoff tests"]
async fn concurrent_handoffs_keep_the_camera_on() {
    for iteration in 0..60 {
        let session = Arc::new(SdkSession::new());
        let camera = RecordingCamera::new().with_disable_delay(Duration::from_millis(1));
        let timings = timings();

        let guard_a = within(start_cam_stream(
            &session,
            &camera,
            &timings,
            CancellationToken::new(),
        ))
        .await
        .expect("the first claim failed");

        let departing = {
            let session = Arc::clone(&session);
            let camera = camera.clone();
            async move { guard_a.finish(&session, &camera, &timings).await }
        };
        let replacement = {
            let session = Arc::clone(&session);
            let camera = camera.clone();
            async move { start_cam_stream(&session, &camera, &timings, CancellationToken::new()).await }
        };
        // Alternating which one is spawned first is what makes both orders of
        // the operation lock actually happen; spawning the same one first every
        // time wins the lock every time and only ever runs one of them.
        let (finish_a, start_b) = if iteration % 2 == 0 {
            let finish_a = tokio::spawn(departing);
            (finish_a, tokio::spawn(replacement))
        } else {
            let start_b = tokio::spawn(replacement);
            (tokio::spawn(departing), start_b)
        };

        let released = within(finish_a).await.expect("the departing task panicked");
        let guard_b = within(start_b)
            .await
            .expect("the replacement task panicked")
            .expect("the replacement claim failed");

        assert_eq!(
            camera.last_call(),
            Some(true),
            "the camera was left off under a live owner"
        );
        let disables = camera.calls().iter().filter(|on| !**on).count();
        assert_eq!(
            disables,
            usize::from(released),
            "a disable was issued by a handler that no longer owned the feed"
        );

        assert!(guard_b.finish(&session, &camera, &timings).await);
        assert_eq!(camera.last_call(), Some(false));
    }
}
