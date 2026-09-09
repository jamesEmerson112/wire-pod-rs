//! `/api-sdk/disconnect`: dropping one robot's cached connection.

use axum::response::Response;
use wirepod_core::{AppState, RobotEntry};

use crate::{literals, reply};

/// Drops the robot, then answers `done`.
///
/// ```text
/// case r.URL.Path == "/api-sdk/disconnect":
///     removeRobot(robotObj.ESN, "server")
///     fmt.Fprint(w, "done")
///     return
/// ```
///
/// `server.go:605-608`. Two things about that shape are contract. The body is
/// written after `removeRobot` returns, and `removeRobot` sleeps three seconds
/// for every matched robot (`robot.go:471`), so the request blocks for the
/// settle before anything reaches the client; the live Go server answers this
/// route in 3.00 seconds. And the body is `done` whatever happened, because
/// `removeRobot` returns nothing at all: a robot that was already gone, or that
/// a concurrent disconnect removed first, is not distinguishable from here.
/// [`RobotRegistry::disconnect`](wirepod_core::RobotRegistry::disconnect) does
/// return whether it removed anything, and this discards it for that reason.
///
/// The serial disconnected is the entry's own, which is Go's `robotObj.ESN`
/// rather than the raw form value. The two agree up to case and surrounding
/// whitespace, since `removeRobot` matches with `strings.EqualFold`
/// (`robot.go:459`) and [`Esn`](wirepod_core::Esn) normalises both ends of the
/// same comparison.
///
/// Everything else lives in the registry, because the ordering inside is not a
/// handler's business: it stops the camera and the stim stream, pays the
/// settle, disables a camera the disconnect itself stopped, and drops the
/// entry, all under the per-serial connect lock that stands in for Go's
/// `inhibitCreation` flag. A request for the same serial arriving during the
/// settle therefore waits and then dials afresh, which is deviations 8 and 16.
pub async fn handle(state: &AppState, entry: Option<&RobotEntry>) -> Response {
    if let Some(entry) = entry {
        state.registry().disconnect(&entry.esn).await;
    }
    // Unreachable with `None`, for the reason given on
    // [`crate::sdkapp::cam::stop`]: `disconnect` is not preamble-exempt. Go's
    // arm writes `done` unconditionally once it is reached, so this does too.
    reply::text(literals::DONE)
}
