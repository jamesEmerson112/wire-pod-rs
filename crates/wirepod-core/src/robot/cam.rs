//! Taking the camera, giving it back, and pumping frames while it is held.
//!
//! Go splits the same work into `startCamStream` (`server.go:684-695`),
//! `finishCamStream` (`server.go:700-707`) and the frame loop inside
//! `camStreamHandler` (`server.go:752-788`). The first two hold the robot's
//! camera operation lock across both the registry update and the
//! `EnableImageStreaming` RPC, which is the whole point: without it a departing
//! handler's disable can land after a replacement has already enabled the
//! camera, and the new owner's feed dies.
//!
//! The Rust shape is a `#[must_use]` [`CamGuard`] with an explicit
//! [`CamGuard::finish`] and a `Drop` that cleans up whatever `finish` did not.
//! Go can lean on `defer finishCamStream` because a goroutine always runs to
//! completion, whereas an axum handler future is dropped outright when the
//! browser aborts the request. A guard that only logged on drop would therefore
//! leak the claim, with a cancellation token nobody holds and a robot whose
//! camera stays on, so the drop path does the work rather than reporting it.
//!
//! The frame pump is deliberately smaller than Go's loop: it takes a
//! [`FrameSink`] rather than an HTTP response, and the multipart framing and the
//! quality-50 JPEG re-encode stay with the route, which is deferred until a
//! codec is in the lock file. The sink therefore receives the bytes the robot
//! sent.

use std::fmt;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::esn::Generation;
use crate::robot::conn::{CameraControl, ConnError, FrameOutcome, FrameSink, FrameStream};
use crate::robot::meter::CamMeter;
use crate::robot::session::SdkSession;
use crate::timings::Timings;

/// Proof that this handler owns the robot's camera feed.
///
/// Held for as long as the feed is being read, and handed back with
/// [`CamGuard::finish`]. The generation inside is what makes the release safe: a
/// handler that has already been displaced finds a newer generation, releases
/// nothing and issues no disable, so it cannot turn the camera off underneath
/// the handler that replaced it.
///
/// The guard owns the session it was claimed from rather than being handed one
/// back at `finish`. Generations are numbered per session, so `Generation(1)`
/// exists on every robot, and a guard that named no session could release a
/// different robot's feed.
///
/// The switch is an `Arc<dyn CameraControl>` rather than an
/// `Arc<dyn RobotConn>`: it is all the guard needs, and a caller holding an
/// `Arc<dyn RobotConn>` reaches it with a single upcast coercion.
#[must_use = "the camera stays claimed and the robot's camera stays on until the guard is finished"]
pub struct CamGuard {
    session: Arc<SdkSession>,
    camera: Arc<dyn CameraControl>,
    timings: Timings,
    generation: Generation,
    finished: bool,
}

impl fmt::Debug for CamGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CamGuard")
            .field("esn", &self.session.esn())
            .field("generation", &self.generation)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl CamGuard {
    /// The generation this handler owns the feed with.
    pub fn generation(&self) -> Generation {
        self.generation
    }

    /// Gives the feed back, and turns the camera off if this handler still owns
    /// it.
    ///
    /// Returns whether the release succeeded, which is exactly when a disable
    /// was issued. Both steps run under the robot's camera operation lock, which
    /// is what makes the ownership check and the RPC atomic against a
    /// replacement taking over (`server.go:700-707`).
    ///
    /// The guard is only marked finished once both steps have run, so a `finish`
    /// future dropped while it was still queued on the operation lock falls back
    /// to the drop path rather than leaving the claim behind.
    pub async fn finish(mut self) -> bool {
        let released = {
            let _op = self.session.lock_cam_op().await;
            let released = self.session.cam.release(self.generation);
            if released {
                // Go's `enableImageStreaming` discards the RPC result entirely
                // (`server.go:673-678`), and there is nothing a departing
                // handler could do with it anyway.
                let _ = enable_image_streaming(self.camera.as_ref(), &self.timings, false).await;
            }
            released
        };
        self.finished = true;
        released
    }
}

impl Drop for CamGuard {
    /// The cleanup for a guard whose handler never reached [`CamGuard::finish`],
    /// which is what an aborted browser request looks like.
    ///
    /// The release is synchronous and generation-checked, so the claim and its
    /// cancellation token are gone by the time the drop returns even on a thread
    /// with no runtime. The disable cannot be, because `Drop` is not async, so it
    /// is spawned. That leaves the release outside the operation lock, which is
    /// the one property the explicit path keeps.
    ///
    /// The spawned task closes the gap that opens up instead of leaving it. It
    /// takes the operation lock and re-reads ownership before it issues
    /// anything, so a replacement that claimed while the disable was still
    /// queued keeps its camera; a replacement that claims later queues on that
    /// same lock and turns the camera back on after the disable. Either order
    /// therefore ends with the camera on for whoever owns the feed, which is the
    /// invariant Go's generation check protects when a departing handler races a
    /// replacement (`robot.go:121-131`).
    fn drop(&mut self) {
        if self.finished || !self.session.cam.release(self.generation) {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                target: "sdkapp",
                esn = %self.session.esn(),
                "camera guard dropped outside a runtime; the robot's camera may be left on"
            );
            return;
        };
        let session = Arc::clone(&self.session);
        let camera = Arc::clone(&self.camera);
        let timings = self.timings;
        handle.spawn(async move {
            let _op = session.lock_cam_op().await;
            if session.cam.current().is_some() {
                tracing::debug!(
                    target: "sdkapp",
                    esn = %session.esn(),
                    "camera guard dropped without a finish; a new owner claimed the feed, so the camera was left on"
                );
                return;
            }
            match enable_image_streaming(camera.as_ref(), &timings, false).await {
                Ok(()) => tracing::debug!(
                    target: "sdkapp",
                    esn = %session.esn(),
                    "camera guard dropped without a finish; the camera was turned off"
                ),
                Err(err) => tracing::warn!(
                    target: "sdkapp",
                    esn = %session.esn(),
                    %err,
                    "camera guard dropped without a finish; turning the camera off failed"
                ),
            }
        });
    }
}

/// Takes the robot's camera feed and turns its camera on.
///
/// The claim is preemptive: whichever handler held the feed is cancelled rather
/// than left hanging, because a second tab, a reload or a retry must replace the
/// previous stream rather than stack another on top. The displaced token is
/// taken under the ownership lock and cancelled once that lock is gone, as Go
/// cancels `prev` after its unlock (`robot.go:105-116`).
///
/// The guard is built in the same breath as the claim, before anything is
/// awaited. Everything after the claim can be dropped underneath this future,
/// and only a guard that already exists can give the claim back when it is.
///
/// The settle is paid only when a live owner was displaced, because it exists to
/// let the robot drop the `CameraFeed` that was just cancelled and a first claim
/// on an idle robot has nothing to wait for (`server.go:688-692`).
///
/// The enable is bounded by `timings.enable`, standing in for the
/// `context.WithTimeout` Go wraps the RPC in so that a robot which has stopped
/// answering cannot park the caller in the very call meant to free it
/// (`server.go:671`). A failed enable hands the feed straight back through the
/// guard's own [`CamGuard::finish`], so a start that returns an error leaves no
/// owner behind; Go reaches the same place by discarding the error and letting
/// its deferred `finishCamStream` run.
pub async fn start_cam_stream(
    session: Arc<SdkSession>,
    camera: Arc<dyn CameraControl>,
    timings: &Timings,
    cancel: CancellationToken,
) -> Result<CamGuard, ConnError> {
    let op = session.lock_cam_op().await;
    let (generation, displaced) = session.cam.claim(cancel);
    let guard = CamGuard {
        session: Arc::clone(&session),
        camera: Arc::clone(&camera),
        timings: *timings,
        generation,
        finished: false,
    };
    if let Some(displaced) = displaced {
        displaced.cancel();
        tokio::time::sleep(timings.settle).await;
    }
    // Go discards what this call answers, a timeout included, and opens the
    // feed regardless. The real robot does let it time out.
    if let Err(err) = enable_image_streaming(camera.as_ref(), timings, true).await {
        tracing::debug!(target: "sdkapp", "enable image streaming: {err}");
    }
    drop(op);
    Ok(guard)
}

/// Why [`cam_stream_pump`] returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PumpExit {
    /// The token was cancelled, which is what a stop or a replacement claim
    /// does.
    Cancelled,
    /// The robot closed the feed cleanly.
    StreamEnded,
    /// The receive failed, which is also how a cancelled gRPC stream ends.
    StreamError(ConnError),
    /// The client is gone.
    SinkClosed,
}

/// Reads frames until the feed ends, counting every one of them.
///
/// The meter moves before the sink is handed the frame, because a frame that
/// fails to decode still crossed the wire and the measurement is of the link
/// rather than of the picture (`server.go:770-776`). A frame the sink reports as
/// [`FrameOutcome::Skipped`] therefore stays counted and produces nothing on the
/// wire, which is Go's `continue` on an undecodable image
/// (`server.go:777-782`); that skip is a crash fix, since a truncated frame used
/// to reach the encoder as a nil image and panic the process.
///
/// A receive error ends the pump rather than looping, which is what stops a
/// persistent error from spinning the loop hot (`server.go:762-768`). Go's extra
/// `isCamStreaming` sample at the top of each iteration is not reproduced: it is
/// not what stops a superseded handler, because a newer handler sets the flag
/// back to true, and everything that clears it also cancels the token this
/// selects on.
pub async fn cam_stream_pump(
    frames: &mut dyn FrameStream,
    meter: &CamMeter,
    sink: &mut dyn FrameSink,
    cancel: CancellationToken,
) -> PumpExit {
    loop {
        let received = tokio::select! {
            // A pending cancellation wins over a pending frame, so a stop is
            // never delayed by a robot that is still sending.
            biased;
            () = cancel.cancelled() => return PumpExit::Cancelled,
            received = frames.next() => received,
        };

        match received {
            Ok(Some(frame)) => {
                meter.record(frame.data.len() as u64);
                match sink.send(&frame.data).await {
                    FrameOutcome::Sent | FrameOutcome::Skipped => {}
                    FrameOutcome::Closed => return PumpExit::SinkClosed,
                }
            }
            Ok(None) => return PumpExit::StreamEnded,
            Err(err) => return PumpExit::StreamError(err),
        }
    }
}

/// The camera switch on a deadline, with an expiry that reads exactly like the
/// one grpc-go produces.
async fn enable_image_streaming(
    camera: &dyn CameraControl,
    timings: &Timings,
    on: bool,
) -> Result<(), ConnError> {
    match tokio::time::timeout(timings.enable, camera.enable_image_streaming(on)).await {
        Ok(result) => result,
        Err(_) => Err(ConnError::deadline_exceeded()),
    }
}
