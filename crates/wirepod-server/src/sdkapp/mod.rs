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

pub mod batterywatchdog;
pub mod bcassume;
pub mod cam;
pub mod cam_stream;
pub mod disconnect;
pub mod faces;
pub mod motion;
pub mod net_probe;
pub mod photos;
pub mod sdk_info;
pub mod settings;
pub mod speech;
pub mod stim;

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::Response;
use http::header;
use http_body_util::BodyExt;
use wirepod_core::{AppState, Esn, RobotEntry};

use crate::form::{self, Form};
use crate::{literals, reply};

/// What a route needing the generated SDK client answers when the connection
/// is not a tonic one, which only a test fake is.
pub(crate) const NO_SDK_CLIENT: &str = "rpc error: code = Unavailable desc = no SDK client";

/// The generated SDK client behind a connected robot, which is what Go reaches
/// as `robot.Conn`.
pub(crate) fn sdk_client(entry: &RobotEntry) -> Option<wirepod_vector::SdkClient> {
    wirepod_vector::sdk_client(entry.conn.as_ref())
}

/// The subtree prefix, registered with its trailing slash (`server.go:806`).
pub const PREFIX: &str = "/api-sdk/";

/// The ten routes the early slice covers, named in the plan.
///
/// Nine of them are `case` arms in Go's switch; `debug` is not, and reaches the
/// 404 default there as it does here. It is in the list because it is one of
/// the two paths the preamble exempts, which is what makes it the test that
/// pins the ordering between the preamble and the fallback.
///
/// The list is kept now that the other arms are translated too, because
/// `tests/lifecycle.rs` walks it to prove the idle timer is reset.
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
///
/// `play_sound` is the one route whose body is a multipart upload rather than
/// a form, and Go's `FormValue` merge does not read one, so the bytes are
/// taken here and the request is rebuilt before the merge runs.
pub async fn handle(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let mut req = req;
    let mut sound = None;
    if req.uri().path() == speech::PLAY_SOUND_PATH {
        let (parts, body) = req.into_parts();
        let bytes = match body.collect().await {
            Ok(collected) => collected.to_bytes().to_vec(),
            Err(_) => Vec::new(),
        };
        let content_type = parts
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        sound = speech::form_file(content_type.as_deref(), &bytes, "sound");
        req = Request::from_parts(parts, Body::from(bytes));
    }
    let (parts, form) = form::read(req).await;
    dispatch(&state, parts.uri.path(), &form, sound).await
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
async fn dispatch(
    state: &Arc<AppState>,
    path: &str,
    form: &Form,
    sound: Option<Vec<u8>>,
) -> Response {
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

        // Every remaining arm reads the connected robot. None of them is
        // preamble-exempt, so the `Err` arm is unreachable: an unresolvable
        // serial or a failed dial has already been written and returned above.
        // It answers the dispatch default rather than panicking, because an
        // unreachable arm that cannot be reached by a request is not worth a
        // way to take the process down.
        _ => match robot.as_deref() {
            Ok(entry) => connected_route(state, route, entry, form, sound).await,
            Err(_) => reply::not_found(),
        },
    }
}

/// The routes behind a connected robot, in Go's `switch` order.
///
/// Split out so that [`dispatch`]'s `match` stays one flat list of route names
/// rather than nesting the `Ok`/`Err` on every one of them. Go reads
/// `robotObj` in each arm directly, because its preamble left a zero value
/// there rather than an error.
async fn connected_route(
    state: &AppState,
    route: &str,
    entry: &RobotEntry,
    form: &Form,
    sound: Option<Vec<u8>>,
) -> Response {
    match route {
        "net_probe" => net_probe::handle(state, entry).await,
        "alexa_sign_in" => speech::alexa_opt_in(entry, true).await,
        "alexa_sign_out" => speech::alexa_opt_in(entry, false).await,
        "cloud_intent" => speech::cloud_intent(entry, form.get("intent")).await,
        "eye_color" => settings::eye_color(entry, form.get("color")).await,
        "custom_eye_color" => {
            settings::custom_eye_color(entry, form.get("hue"), form.get("sat")).await
        }
        "volume" => settings::set_intbool(entry, "master_volume", form.get("volume")).await,
        "locale" => settings::set_string(entry, "locale", form.get("locale")).await,
        "location" => settings::set_string(entry, "default_location", form.get("location")).await,
        "timezone" => settings::set_string(entry, "time_zone", form.get("timezone")).await,
        "get_sdk_settings" => settings::get_sdk_settings(state, entry).await,
        "play_sound" => speech::play_sound(entry, sound).await,
        "get_battery" => speech::get_battery(entry).await,
        "time_format_12" => settings::set_intbool(entry, "clock_24_hour", "false").await,
        "time_format_24" => settings::set_intbool(entry, "clock_24_hour", "true").await,
        "temp_c" => settings::set_intbool(entry, "temp_is_fahrenheit", "false").await,
        "temp_f" => settings::set_intbool(entry, "temp_is_fahrenheit", "true").await,
        "button_hey_vector" => settings::set_intbool(entry, "button_wakeword", "0").await,
        "button_alexa" => settings::set_intbool(entry, "button_wakeword", "1").await,
        "assume_behavior_control" => bcassume::assume(entry, form.get("priority")),
        "release_behavior_control" => bcassume::release(entry),
        "say_text" => speech::say_text(entry, form.get("text")).await,
        "move_wheels" => motion::move_wheels(entry, form.get("lw"), form.get("rw")).await,
        "move_lift" => motion::move_lift(entry, form.get("speed")).await,
        "move_head" => motion::move_head(entry, form.get("speed")).await,
        "get_faces" => faces::get_faces(entry).await,
        "rename_face" => {
            faces::rename_face(
                entry,
                form.get("id"),
                form.get("oldname"),
                form.get("newname"),
            )
            .await
        }
        "delete_face" => faces::delete_face(entry, form.get("id")).await,
        "add_face" => faces::add_face(entry, form.get("name")).await,
        "mirror_mode" => motion::mirror_mode(entry, form.get("enable")).await,
        "begin_event_stream" => stim::begin(entry),
        "stop_event_stream" => stim::stop(entry),
        "get_stim_status" => stim::status(entry),
        "stop_cam_stream" => cam::stop(Some(entry)),
        "get_image_ids" => photos::get_image_ids(entry).await,
        "get_image" => photos::get_image(entry, form.get("id")).await,
        "get_image_thumb" => photos::get_image_thumb(entry, form.get("id")).await,
        "delete_image" => photos::delete_image(entry, form.get("id")).await,
        "get_robot_stats" => speech::get_robot_stats(entry).await,
        "print_robot_info" => speech::print_robot_info(entry),
        "disconnect" => disconnect::handle(state, Some(entry)).await,
        "trigger_wake_word" => speech::trigger_wake_word(entry).await,
        // Go's `default`.
        _ => reply::not_found(),
    }
}
