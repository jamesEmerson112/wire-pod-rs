//! The camera routes on the `/api-sdk/` prefix.
//!
//! Only `stop_cam_stream` does anything here. `begin_cam_stream` is a no-op that
//! answers `done`, because the one statement in Go's arm is commented out
//! (`server.go:517-520`), and it stays inline in the dispatch beside the other
//! literal answers. The feed itself is claimed by `/cam-stream`, which is
//! [`CAM_STREAM_PATH`] and is not part of the slice.

use axum::response::Response;
use wirepod_core::RobotEntry;

use crate::{literals, reply};

/// The camera route Go registers outside both prefixes (`server.go:818`).
///
/// It is named here rather than left to be discovered, because the route it
/// names carries a rule that is easy to lose. `camStreamHandler` runs its own
/// connect preamble and throws the robot index away
/// (`server.go:710`: `robotObj, _, err := getRobot(...)`), so it never writes
/// `robots[robotIndex].ConnTimer = 0`. A page showing nothing but the camera is
/// therefore dropped by the idle sweep after 300 seconds while frames are still
/// flowing, and the port reproduces that by not touching from this path. The
/// asymmetry is deliberate on both sides: `RobotRegistry::touch` says the same
/// thing from the registry's end, and
/// `tests/lifecycle.rs::the_cam_stream_route_is_outside_the_prefix_and_touches_nothing`
/// pins it while the route is still absent.
///
/// The route lands in P4 proper together with the multipart framing and the
/// quality-50 JPEG re-encode. Whatever implements it must not touch the idle
/// timer.
pub const CAM_STREAM_PATH: &str = "/cam-stream";

/// `/api-sdk/stop_cam_stream`: ends the feed, and answers `done`.
///
/// ```text
/// case r.URL.Path == "/api-sdk/stop_cam_stream":
///     stopCamStream(robotObj.ESN)
///     fmt.Fprint(w, "done")
///     return
/// ```
///
/// `server.go:521-524`. The whole of the work is `CamOwner::stop`, which is
/// `stopCamStream` (`robot.go:136-144`): it clears the streaming flag and
/// cancels the owning handler's token, and it deliberately leaves the ownership
/// entry in place. That last part is what makes this route safe to call at any
/// time. Deleting the entry here would let the departing handler's own release
/// find nothing to release, so the camera would never be turned off; leaving it
/// means the handler that is being cancelled runs its release under the camera
/// operation lock and issues the disable itself.
///
/// Clearing the flag alone would not end anything, because a handler only
/// samples the flag after a receive returns and a docked robot sending no frames
/// never returns one. The cancel is what actually stops it.
///
/// The answer is unconditional. A robot whose camera was never claimed still
/// answers `done`, having sent nothing to the robot at all, because Go's
/// `stopCamStream` is a map lookup that finds nothing and its arm prints `done`
/// either way.
pub fn stop(entry: Option<&RobotEntry>) -> Response {
    let Some(entry) = entry else {
        // Unreachable: `stop_cam_stream` is not one of the two preamble-exempt
        // paths, so a connect that failed has already been answered with the
        // `error: ` body and never reaches the switch. Answering `done` rather
        // than unwrapping keeps the byte-exact body if the exemption list ever
        // grows, which is the only way this arm becomes reachable.
        return reply::text(literals::DONE);
    };
    entry.session.cam.stop();
    reply::text(literals::DONE)
}
