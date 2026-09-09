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
//! [`CamGuard::finish`], because `Drop` cannot be async and spawning from `Drop`
//! is a known hazard. `Drop` only logs a leak, so the compiler warning plus the
//! runtime log are together the closest safe analogue of Go's
//! `defer finishCamStream`.
//!
//! The frame pump is deliberately smaller than Go's loop: it takes a
//! [`FrameSink`] rather than an HTTP response, and the multipart framing and the
//! quality-50 JPEG re-encode stay with the route, which is deferred until a
//! codec is in the lock file. The sink therefore receives the bytes the robot
//! sent.

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
/// The guard names no session on purpose. It carries only the generation, so it
/// is `'static` and moves into a spawned handler task without an `Arc` clone of
/// its own; the task passes its own `&SdkSession` back in at `finish`.
#[must_use = "the camera stays claimed and the robot's camera stays on until the guard is finished"]
#[derive(Debug)]
pub struct CamGuard {
    generation: Generation,
    finished: bool,
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
    pub async fn finish(
        mut self,
        session: &SdkSession,
        camera: &dyn CameraControl,
        timings: &Timings,
    ) -> bool {
        self.finished = true;
        let _op = session.lock_cam_op().await;
        release_and_disable(session, camera, timings, self.generation).await
    }
}

impl Drop for CamGuard {
    fn drop(&mut self) {
        if !self.finished {
            tracing::error!(
                target: "sdkapp",
                "camera guard dropped without finish; the robot keeps the feed claimed and its camera on"
            );
        }
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
/// The settle is paid only when a live owner was displaced, because it exists to
/// let the robot drop the `CameraFeed` that was just cancelled and a first claim
/// on an idle robot has nothing to wait for (`server.go:688-692`).
///
/// The enable is bounded by `timings.enable`, standing in for the
/// `context.WithTimeout` Go wraps the RPC in so that a robot which has stopped
/// answering cannot park the caller in the very call meant to free it
/// (`server.go:671`). A failed enable hands the feed straight back through the
/// same generation-checked release the guard would have done, so a start that
/// returns an error leaves no owner behind; Go reaches the same place by
/// discarding the error and letting its deferred `finishCamStream` run.
pub async fn start_cam_stream(
    session: &SdkSession,
    camera: &dyn CameraControl,
    timings: &Timings,
    cancel: CancellationToken,
) -> Result<CamGuard, ConnError> {
    let _op = session.lock_cam_op().await;
    let (generation, displaced) = session.cam.claim(cancel);
    if let Some(displaced) = displaced {
        displaced.cancel();
        tokio::time::sleep(timings.settle).await;
    }
    match enable_image_streaming(camera, timings, true).await {
        Ok(()) => Ok(CamGuard {
            generation,
            finished: false,
        }),
        Err(err) => {
            release_and_disable(session, camera, timings, generation).await;
            Err(err)
        }
    }
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

/// Gives the feed back if `generation` still holds it, and disables the camera
/// when it did.
///
/// The caller must already hold the robot's camera operation lock.
async fn release_and_disable(
    session: &SdkSession,
    camera: &dyn CameraControl,
    timings: &Timings,
    generation: Generation,
) -> bool {
    if !session.cam.release(generation) {
        return false;
    }
    // Go's `enableImageStreaming` discards the RPC result entirely
    // (`server.go:673-678`), and there is nothing a departing handler could do
    // with it anyway.
    let _ = enable_image_streaming(camera, timings, false).await;
    true
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
