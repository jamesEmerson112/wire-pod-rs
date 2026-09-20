//! Camera ownership and the handoff. Ports of Go test 2,
//! `TestReleaseCamStreamIgnoresSupersededOwner` (`sdkapp_test.go:131-164`), and
//! Go test 1, `TestCamStreamHandoffKeepsCameraOn` (`sdkapp_test.go:66-127`).
//!
//! The state machine itself is synchronous by design, so the ownership tests
//! need no runtime. The handoff tests do, and they replace Go's 60 randomised
//! iterations with the interleavings that actually exist once the camera
//! operation lock is in place: the replacement claims while the departing
//! handler is still running, the replacement is inside its enable when the
//! departing handler queues behind it, the departing handler gets there first,
//! and the replacement queues behind a departing handler that is parked inside
//! its disable. All four must end with the camera on, which is the invariant Go
//! states at `sdkapp_test.go:60-61`. The probabilistic form is kept behind
//! `#[ignore]` at the bottom.
//!
//! The drop tests are the other half. Go's handler is a goroutine that always
//! runs to completion, so its `defer finishCamStream` always runs; an axum
//! handler future is dropped where the browser aborted the request, so the guard
//! has to give the claim back from `Drop` as well as from `finish`.
//!
//! No test pauses the clock. Deadlines are real and small, and the only
//! wall-clock element is a ceiling that fires on a regression.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Wake, Waker};
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use wirepod_core::test_support::RecordingCamera;
use wirepod_core::{CamOwner, CameraControl, Esn, SdkSession, Timings, start_cam_stream};

/// Nothing here waits on anything real, so this only ever fires on a
/// regression.
const CEILING: Duration = Duration::from_secs(2);

/// How long the ownership probe waits for the lock it expects to be free.
const PROBE_WAIT: Duration = Duration::from_millis(500);

/// Long enough for a task the drop path spawned to have been polled, and far
/// enough inside the ceiling that it never decides a test.
const SPAWN_WINDOW: Duration = Duration::from_millis(50);

/// The robot this machine actually has.
fn session() -> Arc<SdkSession> {
    Arc::new(SdkSession::new(Esn::new("00303f28")))
}

/// The switch the guard holds, alongside the recorder the test reads. The fake
/// shares its state between clones, so both halves see the same call log.
fn control(camera: &RecordingCamera) -> Arc<dyn CameraControl> {
    Arc::new(camera.clone())
}

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

/// Waits for work the guard's drop spawned, under the same ceiling.
async fn until(mut ready: impl FnMut() -> bool) {
    within(async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
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

/// A waker that asks, at the moment the displaced owner is cancelled, whether
/// the ownership lock is free.
///
/// `CancellationToken::cancel` wakes its waiters inline, so this runs on the
/// thread that called it, and the question is asked from a second thread so that
/// a held lock is a bounded wait rather than a hung test. In the correct order
/// the lock is free and the answer is immediate, because the cancel happens
/// after `claim` has returned; a cancel moved back inside the critical section
/// makes the answer time out.
struct OwnershipProbe {
    session: Arc<SdkSession>,
    reached: AtomicBool,
    lock_was_free: AtomicBool,
}

impl Wake for OwnershipProbe {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let (answered, answer) = std::sync::mpsc::channel();
        let session = Arc::clone(&self.session);
        std::thread::spawn(move || {
            let _ = session.cam.current();
            let _ = answered.send(());
        });
        let free = answer.recv_timeout(PROBE_WAIT).is_ok();
        self.lock_was_free.store(free, Ordering::SeqCst);
        self.reached.store(true, Ordering::SeqCst);
    }
}

/// The cancel of the owner a claim displaced happens outside the ownership
/// critical section, as Go cancels `prev` after its unlock
/// (`robot.go:105-116`).
///
/// The probe is woken from inside `start_cam_stream`'s `displaced.cancel()` and
/// finds the ownership lock free. A cancel moved inside the lock leaves it held,
/// and the probe reports that rather than deadlocking on it.
#[tokio::test]
async fn a_displaced_owner_is_cancelled_outside_the_ownership_lock() {
    let session = session();
    let camera = RecordingCamera::new();
    let control = control(&camera);
    let timings = timings();

    let cancel_a = CancellationToken::new();
    let guard_a = within(start_cam_stream(
        Arc::clone(&session),
        Arc::clone(&control),
        &timings,
        cancel_a.clone(),
    ))
    .await
    .expect("the first claim failed");

    let probe = Arc::new(OwnershipProbe {
        session: Arc::clone(&session),
        reached: AtomicBool::new(false),
        lock_was_free: AtomicBool::new(false),
    });
    let waker = Waker::from(Arc::clone(&probe));
    let mut cancelled = std::pin::pin!(cancel_a.cancelled());
    assert!(
        cancelled
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending(),
        "the first owner was already cancelled"
    );

    let guard_b = within(start_cam_stream(
        Arc::clone(&session),
        Arc::clone(&control),
        &timings,
        CancellationToken::new(),
    ))
    .await
    .expect("the replacement claim failed");

    assert!(
        probe.reached.load(Ordering::SeqCst),
        "the displaced owner was never cancelled"
    );
    assert!(
        probe.lock_was_free.load(Ordering::SeqCst),
        "the displaced owner was cancelled with the ownership lock still held"
    );
    assert!(cancel_a.is_cancelled());

    assert!(!within(guard_a.finish()).await);
    assert!(within(guard_b.finish()).await);
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
    let session = session();
    let camera = RecordingCamera::new();
    let control = control(&camera);
    let timings = timings_with_an_unaffordable_settle();

    let guard = within(start_cam_stream(
        Arc::clone(&session),
        control,
        &timings,
        CancellationToken::new(),
    ))
    .await
    .expect("the first claim failed");

    assert_eq!(camera.calls(), vec![true]);
    assert!(within(guard.finish()).await);
    assert_eq!(camera.calls(), vec![true, false]);
}

#[tokio::test]
async fn displacing_a_live_owner_pays_the_settle() {
    let session = session();
    let camera = RecordingCamera::new();
    let control = control(&camera);
    let timings = timings_with_an_unaffordable_settle();

    let cancel_a = CancellationToken::new();
    let guard_a = within(start_cam_stream(
        Arc::clone(&session),
        Arc::clone(&control),
        &timings,
        cancel_a.clone(),
    ))
    .await
    .expect("the first claim failed");

    let replacement = tokio::time::timeout(
        Duration::from_millis(200),
        start_cam_stream(
            Arc::clone(&session),
            Arc::clone(&control),
            &timings,
            CancellationToken::new(),
        ),
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
    assert_eq!(
        session.cam.current(),
        None,
        "the replacement's future was dropped in the settle and kept the claim"
    );

    assert!(!within(guard_a.finish()).await);
}

/// Go test 1, first interleaving: the replacement claims while the departing
/// handler is still reading frames.
#[tokio::test]
async fn a_handoff_under_a_live_owner_keeps_the_camera_on() {
    let session = session();
    let camera = RecordingCamera::new();
    let control = control(&camera);
    let timings = timings();

    let cancel_a = CancellationToken::new();
    let guard_a = within(start_cam_stream(
        Arc::clone(&session),
        Arc::clone(&control),
        &timings,
        cancel_a.clone(),
    ))
    .await
    .expect("the first claim failed");

    let cancel_b = CancellationToken::new();
    let guard_b = within(start_cam_stream(
        Arc::clone(&session),
        Arc::clone(&control),
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
        !within(guard_a.finish()).await,
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
        within(guard_b.finish()).await,
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
    let session = session();
    let camera = RecordingCamera::new();
    let control = control(&camera);
    let timings = timings();

    let cancel_a = CancellationToken::new();
    let guard_a = within(start_cam_stream(
        Arc::clone(&session),
        Arc::clone(&control),
        &timings,
        cancel_a.clone(),
    ))
    .await
    .expect("the first claim failed");

    let gate = camera.arm_enable_gate();
    let start_b = tokio::spawn({
        let session = Arc::clone(&session);
        let control = Arc::clone(&control);
        async move { start_cam_stream(session, control, &timings, CancellationToken::new()).await }
    });

    // The replacement now holds the operation lock and is parked inside its
    // enable, which is exactly where Go's fake burns its delay.
    within(gate.wait_entered()).await;
    assert!(
        cancel_a.is_cancelled(),
        "the replacement did not cancel the handler it displaced"
    );

    let mut finish_a = tokio::spawn(async move { guard_a.finish().await });
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

    assert!(within(guard_b.finish()).await);
    assert_eq!(camera.calls(), vec![true, true, false]);
}

/// Go test 1, third interleaving: the departing handler wins the operation
/// lock, so its disable lands first and the replacement then finds no previous
/// owner, skips the settle and turns the camera back on.
#[tokio::test]
async fn a_handoff_after_a_completed_release_keeps_the_camera_on() {
    let session = session();
    let camera = RecordingCamera::new();
    let control = control(&camera);
    let timings = timings_with_an_unaffordable_settle();

    let guard_a = within(start_cam_stream(
        Arc::clone(&session),
        Arc::clone(&control),
        &timings,
        CancellationToken::new(),
    ))
    .await
    .expect("the first claim failed");
    assert!(within(guard_a.finish()).await);

    let guard_b = within(start_cam_stream(
        Arc::clone(&session),
        Arc::clone(&control),
        &timings,
        CancellationToken::new(),
    ))
    .await
    .expect("the replacement claim failed");

    assert_eq!(camera.calls(), vec![true, false, true]);
    assert_eq!(camera.last_call(), Some(true));
    assert!(within(guard_b.finish()).await);
}

/// The fourth interleaving: the departing handler is parked inside its disable
/// when the replacement arrives, so the replacement queues on the operation lock
/// behind the RPC rather than behind the ownership update. The camera still ends
/// up on, which is what the lock is for (`server.go:700-707`).
#[tokio::test]
async fn a_replacement_that_queues_behind_a_disable_keeps_the_camera_on() {
    let session = session();
    let camera = RecordingCamera::new();
    let control = control(&camera);
    let timings = timings();

    let guard_a = within(start_cam_stream(
        Arc::clone(&session),
        Arc::clone(&control),
        &timings,
        CancellationToken::new(),
    ))
    .await
    .expect("the first claim failed");

    let gate = camera.arm_disable_gate();
    let finish_a = tokio::spawn(async move { guard_a.finish().await });
    within(gate.wait_entered()).await;

    let mut start_b = tokio::spawn({
        let session = Arc::clone(&session);
        let control = Arc::clone(&control);
        async move { start_cam_stream(session, control, &timings, CancellationToken::new()).await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut start_b)
            .await
            .is_err(),
        "the replacement claimed while the departing handler held the operation lock"
    );

    let queued = camera.stamp();
    gate.release();

    assert!(
        within(finish_a).await.expect("the departing task panicked"),
        "the owner could not release its own feed"
    );
    let guard_b = within(start_b)
        .await
        .expect("the replacement task panicked")
        .expect("the replacement claim failed");

    assert_eq!(camera.calls(), vec![true, false, true]);
    assert_eq!(
        camera.last_call(),
        Some(true),
        "the departing handler's disable landed after the replacement's enable"
    );

    let log = camera.call_log();
    assert!(
        !log[1].on && log[1].order > queued,
        "the departing handler's disable landed before the replacement queued behind it"
    );

    assert!(within(guard_b.finish()).await);
}

/// The enable is bounded, standing in for the `context.WithTimeout` Go wraps
/// the RPC in. Go discards what the call answers and opens the feed anyway, and
/// the real robot does let it time out, so an expiry must not stop the start.
#[tokio::test]
async fn an_enable_that_never_answers_still_starts_the_stream_as_go_does() {
    let session = session();
    let camera = RecordingCamera::new();
    let control = control(&camera);
    let timings = Timings {
        enable: Duration::from_millis(20),
        ..Timings::instant()
    };
    let _gate = camera.arm_enable_gate();

    let guard = within(start_cam_stream(
        Arc::clone(&session),
        control,
        &timings,
        CancellationToken::new(),
    ))
    .await
    .expect("Go opens the feed whatever the enable answered");

    assert!(session.cam.is_streaming());
    drop(guard);
}

/// The browser aborting during the settle drops the whole handler future, and
/// with it the start that is sleeping inside it. The claim has already been
/// taken by then, so the guard has to exist before the sleep for the drop to
/// have anything to give back.
#[tokio::test]
async fn a_start_dropped_in_the_settle_gives_the_claim_back() {
    let session = session();
    let camera = RecordingCamera::new();
    let control = control(&camera);
    let timings = timings_with_an_unaffordable_settle();

    let cancel_a = CancellationToken::new();
    let guard_a = within(start_cam_stream(
        Arc::clone(&session),
        Arc::clone(&control),
        &timings,
        cancel_a.clone(),
    ))
    .await
    .expect("the first claim failed");

    let dropped = tokio::time::timeout(
        Duration::from_millis(50),
        start_cam_stream(
            Arc::clone(&session),
            Arc::clone(&control),
            &timings,
            CancellationToken::new(),
        ),
    )
    .await;
    assert!(
        dropped.is_err(),
        "the replacement did not park in the settle"
    );

    assert_eq!(
        session.cam.current(),
        None,
        "the dropped start left the feed claimed with nobody to release it"
    );
    assert!(!session.cam.is_streaming());

    until(|| camera.last_call() == Some(false)).await;
    assert_eq!(
        camera.calls(),
        vec![true, false],
        "the dropped start left the camera on that the first handler had turned on"
    );

    assert!(!within(guard_a.finish()).await);
}

/// The same abort one step later: the enable has been issued and the robot may
/// well have acted on it, but the answer never arrives because the future is
/// gone. The claim still has to come back, and the camera still has to be turned
/// off.
#[tokio::test]
async fn a_start_dropped_inside_the_enable_gives_the_claim_back() {
    let session = session();
    let camera = RecordingCamera::new();
    let control = control(&camera);
    let timings = timings();
    let gate = camera.arm_enable_gate();

    within(async {
        let mut start = std::pin::pin!(start_cam_stream(
            Arc::clone(&session),
            Arc::clone(&control),
            &timings,
            CancellationToken::new(),
        ));
        tokio::select! {
            _ = &mut start => panic!("the parked enable answered"),
            () = gate.wait_entered() => {}
        }
    })
    .await;

    assert_eq!(
        session.cam.current(),
        None,
        "the dropped start left the feed claimed with nobody to release it"
    );
    assert!(!session.cam.is_streaming());

    until(|| camera.last_call() == Some(false)).await;
    assert_eq!(
        camera.calls(),
        vec![false],
        "the dropped start did not turn off a camera the robot may have turned on"
    );
}

/// A guard that reaches its drop without a finish is the abort case once the
/// stream is running. The release is synchronous, so the claim is gone before
/// the drop returns; the disable cannot be, so it is spawned.
#[tokio::test]
async fn a_guard_dropped_without_finish_releases_and_disables() {
    let session = session();
    let camera = RecordingCamera::new();
    let control = control(&camera);
    let timings = timings();

    let guard = within(start_cam_stream(
        Arc::clone(&session),
        control,
        &timings,
        CancellationToken::new(),
    ))
    .await
    .expect("the claim failed");
    assert_eq!(session.cam.current(), Some(guard.generation()));
    assert_eq!(camera.calls(), vec![true]);

    drop(guard);

    assert_eq!(
        session.cam.current(),
        None,
        "the drop did not release the claim synchronously"
    );
    assert!(!session.cam.is_streaming());

    until(|| camera.last_call() == Some(false)).await;
    assert_eq!(camera.calls(), vec![true, false]);
}

/// The drop path releases the claim synchronously but can only spawn the
/// disable, so a replacement can claim in between. The spawned task re-reads
/// ownership under the operation lock and leaves the camera alone when it finds
/// a new owner, which is the same protection Go's generation check gives a
/// departing handler that raced a replacement (`robot.go:121-131`).
#[tokio::test]
async fn a_dropped_guard_leaves_a_new_owners_camera_alone() {
    let session = session();
    let camera = RecordingCamera::new();
    let control = control(&camera);
    let timings = timings();

    let guard_a = within(start_cam_stream(
        Arc::clone(&session),
        Arc::clone(&control),
        &timings,
        CancellationToken::new(),
    ))
    .await
    .expect("the claim failed");
    let generation_a = guard_a.generation();
    drop(guard_a);

    // Nothing between the drop and this claim yields, so the spawned disable has
    // not been polled yet and the replacement lands in exactly the window the
    // drop path opens.
    let guard_b = within(start_cam_stream(
        Arc::clone(&session),
        control,
        &timings,
        CancellationToken::new(),
    ))
    .await
    .expect("the replacement claim failed");
    assert_ne!(
        guard_b.generation(),
        generation_a,
        "the replacement reused the dropped generation"
    );
    assert_eq!(
        camera.calls(),
        vec![true, true],
        "the spawned disable ran before the replacement claimed"
    );

    within(tokio::time::sleep(SPAWN_WINDOW)).await;
    assert_eq!(
        camera.calls(),
        vec![true, true],
        "the dropped guard turned the camera off under the new owner"
    );
    assert_eq!(
        session.cam.current(),
        Some(guard_b.generation()),
        "the spawned disable took the new owner's claim with it"
    );

    assert!(within(guard_b.finish()).await);
    assert_eq!(camera.calls(), vec![true, true, false]);
}

/// The probabilistic form of Go's 60 iterations. The deterministic
/// interleavings above are the gate; this exists for anyone who wants to run
/// the race itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "stress variant of the deterministic handoff tests"]
async fn concurrent_handoffs_keep_the_camera_on() {
    for iteration in 0..60 {
        let session = session();
        let camera = RecordingCamera::new().with_disable_delay(Duration::from_millis(1));
        let control = control(&camera);
        let timings = timings();

        let guard_a = within(start_cam_stream(
            Arc::clone(&session),
            Arc::clone(&control),
            &timings,
            CancellationToken::new(),
        ))
        .await
        .expect("the first claim failed");

        let departing = async move { guard_a.finish().await };
        let replacement = {
            let session = Arc::clone(&session);
            let control = Arc::clone(&control);
            async move { start_cam_stream(session, control, &timings, CancellationToken::new()).await }
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

        assert!(within(guard_b.finish()).await);
        assert_eq!(camera.last_call(), Some(false));
    }
}
