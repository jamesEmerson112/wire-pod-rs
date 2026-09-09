//! `/api-sdk/*`: the connect preamble and the dispatch behind it.
//!
//! Go registers this prefix with a trailing slash (`server.go:806`), which
//! makes it a subtree pattern, and then dispatches inside one handler by exact
//! string equality on the whole path in a tagless `switch` whose `default`
//! answers 404 (`server.go:67-70`). The Rust router reproduces that shape
//! deliberately rather than registering each route: the preamble runs for every
//! path under the prefix, including unknown ones, so an unknown path with an
//! unknown serial answers the doubled connect error at HTTP 200 and never
//! reaches the 404. Registering the routes individually would 404 first and
//! lose that ordering.
//!
//! The path this handler dispatches on is Go's `r.URL.Path`, which is decoded:
//! [`crate::router`]'s middleware has already unescaped each segment, so
//! `GET /api-sdk/deb%75g` reaches the `debug` arm rather than the catch-all.

pub mod cam;
pub mod disconnect;
pub mod net_probe;
pub mod sdk_info;
pub mod stim;

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::response::Response;
use wirepod_core::{AppState, Esn, RobotEntry};

use crate::form::{self, Form};
use crate::{literals, reply};

/// The subtree prefix, registered with its trailing slash (`server.go:806`).
pub const PREFIX: &str = "/api-sdk/";

/// The ten routes the early slice covers, named in the plan.
///
/// Nine of them are `case` arms in Go's switch; `debug` is not, and reaches the
/// 404 default there as it does here. It is in the list because it is one of
/// the two paths the preamble exempts, which is what makes it the test that
/// pins the ordering between the preamble and the fallback.
///
/// The other 36 `/api-sdk/*` arms Go has are deferred to P4 and answer the same
/// 404 as a stub. That stub is not a contract: `deviations.md` lists them by
/// name and no test asserts their status.
pub const SLICE_ROUTES: [&str; 10] = [
    "conn_test",
    "net_probe",
    "begin_event_stream",
    "stop_event_stream",
    "get_stim_status",
    "begin_cam_stream",
    "stop_cam_stream",
    "disconnect",
    "get_sdk_info",
    "debug",
];

/// Whether `name` is one of [`SLICE_ROUTES`].
pub fn is_slice_route(name: &str) -> bool {
    SLICE_ROUTES.contains(&name)
}

/// The two paths the preamble exempts (`server.go:60`).
///
/// The exemption covers the error write and the idle-timer reset, and nothing
/// else. The connect itself is still attempted, which is why a `get_sdk_info`
/// against a dead robot still pays for a dial.
pub fn is_preamble_exempt(path: &str) -> bool {
    path == "/api-sdk/get_sdk_info" || path == "/api-sdk/debug"
}

/// The one handler behind `/api-sdk/` and everything under it.
pub async fn handle(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let (parts, form) = form::read(req).await;
    dispatch(&state, parts.uri.path(), &form).await
}

/// The preamble, then the switch.
///
/// ```text
/// robotObj, robotIndex, err := getRobot(r.FormValue("serial"))
/// if r.URL.Path != "/api-sdk/get_sdk_info" && r.URL.Path != "/api-sdk/debug" {
///     if err != nil {
///         fmt.Fprint(w, "error: "+err.Error())
///         return
///     }
///     robots[robotIndex].ConnTimer = 0
/// }
/// ```
///
/// `server.go:56-66`. `getRobot` runs for every path with no exception, the
/// two named paths are exempt only from the error write and from the timer
/// reset, and that timer reset is the only thing anywhere that keeps a robot
/// out of the 300 second idle sweep.
async fn dispatch(state: &Arc<AppState>, path: &str, form: &Form) -> Response {
    let serial = Esn::new(form.get("serial"));
    let exempt = is_preamble_exempt(path);

    let robot = state.get_robot(&serial).await;
    if !exempt {
        match &robot {
            Ok(entry) => entry.touch(state.clock().now()),
            Err(err) => return reply::text(format!("{}{err}", literals::ERROR_PREFIX)),
        }
    }

    let route = path.strip_prefix(PREFIX).unwrap_or_default();
    match route {
        // Implemented in this slice.
        "conn_test" => reply::text(literals::SUCCESS),
        "get_sdk_info" => sdk_info::handle(state),
        "begin_cam_stream" => {
            // Go's only statement in this arm is commented out
            // (`server.go:517-520`): the camera is claimed by `/cam-stream`
            // itself, not here, so the route is a no-op that answers `done`.
            reply::text(literals::DONE)
        }
        "debug" => {
            // Go has no `case` arm for `debug` either, so it falls to the
            // default 404 (`server.go:68-70`). Because it is preamble-exempt,
            // a bad serial does not change that. Written out rather than left
            // to the catch-all so the exemption and the 404 stay visible
            // together.
            reply::not_found()
        }

        // The four routes that read the connected robot. None of them is
        // preamble-exempt, so the `Err` arm is unreachable: an unresolvable
        // serial or a failed dial has already been written and returned above.
        // It answers the dispatch default rather than panicking, because an
        // unreachable arm that cannot be reached by a request is not worth a
        // way to take the process down.
        "net_probe" | "begin_event_stream" | "stop_event_stream" | "get_stim_status" => {
            match &robot {
                Ok(entry) => connected_route(state, route, entry).await,
                Err(_) => reply::not_found(),
            }
        }

        // `as_deref().ok()` is `Ok` for both of these: neither path is
        // preamble-exempt, so a failed connect was answered above and never
        // reaches the switch. Each handler documents the unreachable `None`.
        "stop_cam_stream" => cam::stop(robot.as_deref().ok()),
        "disconnect" => disconnect::handle(state, robot.as_deref().ok()).await,

        // Go's `default`, which is also where its other 36 arms land while they
        // are deferred.
        _ => reply::not_found(),
    }
}

/// The four routes behind a connected robot.
///
/// Split out so that [`dispatch`]'s `match` stays one flat list of route names
/// rather than nesting the `Ok`/`Err` on every one of them. Go reads
/// `robotObj` in each arm directly, because its preamble left a zero value
/// there rather than an error.
async fn connected_route(state: &AppState, route: &str, entry: &RobotEntry) -> Response {
    match route {
        "net_probe" => net_probe::handle(state, entry).await,
        "begin_event_stream" => stim::begin(entry),
        "stop_event_stream" => stim::stop(entry),
        "get_stim_status" => stim::status(entry),
        // Unreachable: the caller matched this same list before it called.
        _ => reply::not_found(),
    }
}
