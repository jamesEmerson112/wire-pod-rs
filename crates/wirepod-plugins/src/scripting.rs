//! Translation of `pkg/scripting/scripting.go`.

/*

assumeBehaviorControl(priority int)
    -   10,20,30 (10 highest priority, overriding behaviors. 30 lowest)
releaseBehaviorControl()

<these require behavior control>
<goroutine determines whether the function blocks or not>
sayText(text string, goroutine bool)
playAnimation(animation string, goroutine bool)
sleep(milliseconds int)
moveLift(radpersecond int)
moveHead(radpersecond int)

// leftWheelmmps2 and rightWheelmmps2 are what you want the wheels to accelerate to. if you want
// the wheels to accelerate immediately, just set leftWheelmmps2 and rightWheelmmps2 to 0
moveWheels(leftWheelmmps, rightWheelmmps, leftWheelmmps2, rightWheelmmps2 int)

// won't block
showImage(filePath string, durationMs int)

// wrapper for http.Post and http.Get respectively
// timeout is in seconds
// set timeout to 0 for default
// each return response as strings
postHTTPRequest(url, contentType, body string, timeout int) (resp string)
getHTTPRequest(url, timeout int) (resp string)

*/

/*

<additions with no Go counterpart>
<each blocks and returns what the robot answered, so a script can branch on it>
<the answer is two words, the status and the result, as in RESPONSE_RECEIVED SUCCESS;>
<the robot always sets the status to RESPONSE_RECEIVED here, so test the second word>
<timeout is in seconds; leave it out for 30>
<both need behavior control, or they wait out the whole timeout>
<behavior control at 10 turns off his cliff reaction until release; drive him on the floor>
<behavior control at 20 keeps the cliff reaction on and still lets goToPose run>
goToPose(xMm, yMm, angleRad float, timeout float) (result string)
lookAroundInPlace(timeout float) (result string)

*/

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use http::{HeaderValue, StatusCode as HttpStatus, header};
use http_body_util::BodyExt;
use mlua::{AnyUserData, Lua, UserData, Value};
use serde::Deserialize;
use tokio::runtime::Handle;
use wirepod_core::logger::COMP_LUA;
use wirepod_core::{AppState, ConnError, Esn, RobotEntry, SdkSession, StatusCode};
use wirepod_proto::anki::vector::external_interface as pb;
use wirepod_vector::{MotionOutcome, SdkClient, logged, sdk_client, status_error};

use crate::bcontrol::set_b_control_functions;
use crate::display::convert_pixels_to_raw_bitmap;

/// Go returns a bare `error` whose text is what the HTTP body carries, so both
/// arms render transparently. The Lua arm keeps the text rather than the error,
/// because `mlua::Error` is neither `Send` nor `Sync` and the script runs on a
/// blocking thread.
#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    #[error("{0}")]
    Conn(#[from] ConnError),
    #[error("{0}")]
    Lua(String),
}

impl From<mlua::Error> for ScriptError {
    fn from(err: mlua::Error) -> Self {
        Self::Lua(err.to_string())
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ExternalLuaRequest {
    #[serde(default)]
    pub esn: String,
    #[serde(default)]
    pub script: String,
}

pub struct Bot {
    pub esn: Esn,
    pub robot: Arc<RobotEntry>,
}

impl UserData for Bot {}

/// `L.ToString(n)`: gopher-lua coerces a number and answers `""` for anything
/// it cannot coerce, where mlua's `String` conversion would raise.
pub(crate) fn to_string(lua: &Lua, value: Value) -> String {
    match lua.coerce_string(value) {
        Ok(Some(text)) => text.to_string_lossy(),
        _ => String::new(),
    }
}

/// `L.ToInt(n)`, with the same answer for anything that will not coerce.
pub(crate) fn to_int(lua: &Lua, value: Value) -> i64 {
    match lua.coerce_number(value) {
        Ok(Some(number)) => number as i64,
        _ => 0,
    }
}

/// `L.ToNumber(n)`, with 0 for anything that will not coerce.
fn to_number(lua: &Lua, value: Value) -> f64 {
    match lua.coerce_number(value) {
        Ok(Some(number)) => number,
        _ => 0.0,
    }
}

/// `L.ToBool(n)`: only nil and false are false.
fn to_bool(value: &Value) -> bool {
    !matches!(value, Value::Nil | Value::Boolean(false))
}

fn say_text(lua: &Lua, (text, goroutine): (Value, Value)) -> mlua::Result<()> {
    let text_to_say = to_string(lua, text);
    let (mut client, session) = g_rf_ls_with_session(lua)?;
    let spoken = text_to_say.clone();
    execute_with_goroutine(&goroutine, false, async move {
        logged(
            COMP_LUA,
            &session,
            "SayText",
            &format!("text={spoken:?}"),
            client.say_text(pb::SayTextRequest {
                text: text_to_say,
                use_vector_voice: true,
                duration_scalar: 1.0,
                pitch_scalar: 0.0,
            }),
        )
        .await
        .map(drop)
        .map_err(|status| status_error(&status))
    });
    Ok(())
}

fn play_animation(lua: &Lua, (animation, goroutine): (Value, Value)) -> mlua::Result<()> {
    let anim_to_play = to_string(lua, animation);
    let (mut client, session) = g_rf_ls_with_session(lua)?;
    let requested = anim_to_play.clone();
    execute_with_goroutine(&goroutine, false, async move {
        // The one motion response that says outright whether the behaviour
        // ran, and which animation a trigger resolved to.
        logged(
            COMP_LUA,
            &session,
            "PlayAnimation",
            &format!("animation={requested}"),
            client.play_animation(pb::PlayAnimationRequest {
                animation: Some(pb::Animation { name: anim_to_play }),
                loops: 1,
                ..pb::PlayAnimationRequest::default()
            }),
        )
        .await
        .map(drop)
        .map_err(|status| status_error(&status))
    });
    Ok(())
}

fn sleep(lua: &Lua, milliseconds: Value) -> mlua::Result<()> {
    let sleep_in_ms = to_int(lua, milliseconds);
    std::thread::sleep(Duration::from_millis(sleep_in_ms.max(0) as u64));
    Ok(())
}

fn move_head(lua: &Lua, speed: Value) -> mlua::Result<()> {
    let head_speed = to_int(lua, speed);
    let head_speed_f = head_speed as f32 / 100.0;
    let (mut client, session) = g_rf_ls_with_session(lua)?;
    execute_with_goroutine(&Value::Nil, true, async move {
        logged(
            COMP_LUA,
            &session,
            "MoveHead",
            &format!("rad_per_sec={head_speed_f}"),
            client.move_head(pb::MoveHeadRequest {
                speed_rad_per_sec: head_speed_f,
            }),
        )
        .await
        .map(drop)
        .map_err(|status| status_error(&status))
    });
    Ok(())
}

fn move_lift(lua: &Lua, speed: Value) -> mlua::Result<()> {
    let lift_speed = to_int(lua, speed);
    let lift_speed_f = lift_speed as f32 / 100.0;
    let (mut client, session) = g_rf_ls_with_session(lua)?;
    execute_with_goroutine(&Value::Nil, true, async move {
        logged(
            COMP_LUA,
            &session,
            "MoveLift",
            &format!("rad_per_sec={lift_speed_f}"),
            client.move_lift(pb::MoveLiftRequest {
                speed_rad_per_sec: lift_speed_f,
            }),
        )
        .await
        .map(drop)
        .map_err(|status| status_error(&status))
    });
    Ok(())
}

fn move_wheels(lua: &Lua, speeds: (Value, Value, Value, Value)) -> mlua::Result<()> {
    let left_wheel_speed = to_int(lua, speeds.0);
    let right_wheel_speed = to_int(lua, speeds.1);
    let left_wheel_speed2 = to_int(lua, speeds.2);
    let right_wheel_speed2 = to_int(lua, speeds.3);
    let (mut client, session) = g_rf_ls_with_session(lua)?;
    execute_with_goroutine(&Value::Nil, true, async move {
        logged(
            COMP_LUA,
            &session,
            "DriveWheels",
            &format!("lw={left_wheel_speed} rw={right_wheel_speed}"),
            client.drive_wheels(pb::DriveWheelsRequest {
                left_wheel_mmps: left_wheel_speed as f32,
                right_wheel_mmps: right_wheel_speed as f32,
                left_wheel_mmps2: left_wheel_speed2 as f32,
                right_wheel_mmps2: right_wheel_speed2 as f32,
            }),
        )
        .await
        .map(drop)
        .map_err(|status| status_error(&status))
    });
    Ok(())
}

// later
fn show_image_on_screen(lua: &Lua, (path, duration): (Value, Value)) -> mlua::Result<()> {
    let file_path = to_string(lua, path);
    let duration = to_int(lua, duration);
    let f = match image::ImageReader::open(&file_path) {
        Ok(reader) => reader,
        Err(err) => {
            tracing::info!(comp = "", "Lua error: unable to open image file:{err}");
            return Ok(());
        }
    };
    let img = match f
        .with_guessed_format()
        .map_err(image::ImageError::IoError)
        .and_then(image::ImageReader::decode)
    {
        Ok(img) => img,
        Err(err) => {
            tracing::info!(comp = "", "Lua error: file is not an image:{err}");
            return Ok(());
        }
    };
    let pixels = convert_pixels_to_raw_bitmap(&img, 100);
    let mut buf = Vec::with_capacity(pixels.len() * 2);
    for ui in pixels {
        // The robot's gateway reads each pixel big-endian while wire-pod writes
        // the value little-endian; `docs/robot-api.md` records the disagreement.
        buf.extend_from_slice(&ui.to_le_bytes());
    }
    let mut client = g_rf_ls(lua)?;
    execute_with_goroutine(&Value::Nil, true, async move {
        client
            .display_face_image_rgb(pb::DisplayFaceImageRgbRequest {
                face_data: buf,
                duration_ms: duration as u32,
                interrupt_running: true,
            })
            .await
            .map(drop)
            .map_err(|status| status_error(&status))
    });
    Ok(())
}

/// Go mutates `http.DefaultClient.Timeout` in place, which leaks the deadline
/// to every other user of that client; this builds one client per call.
fn http_client(timeout: i64) -> reqwest::Client {
    let mut builder = reqwest::Client::builder();
    if timeout > 0 {
        builder = builder.timeout(Duration::from_secs(timeout as u64));
    }
    builder.build().unwrap_or_default()
}

fn post_http_request(lua: &Lua, args: (Value, Value, Value, Value)) -> mlua::Result<mlua::String> {
    let url = to_string(lua, args.0);
    let content_type = to_string(lua, args.1);
    let body = to_string(lua, args.2);
    let timeout = to_int(lua, args.3);
    let cl = http_client(timeout);
    let Ok(handle) = Handle::try_current() else {
        return lua.create_string("");
    };
    let resp = match handle.block_on(
        cl.post(url)
            .header(header::CONTENT_TYPE, content_type)
            .body(body)
            .send(),
    ) {
        Ok(resp) => resp,
        Err(err) => {
            tracing::info!(comp = "", "Lua postHTTPRequest error:{err}");
            return lua.create_string(format!("http error: {err}"));
        }
    };
    // Go pushes the body, then pushes either the read error or the body again,
    // and returns only that last push.
    match handle.block_on(resp.bytes()) {
        Ok(b) => lua.create_string(b),
        Err(err) => {
            tracing::info!(comp = "", "Lua postHTTPRequest error:{err}");
            lua.create_string(format!("http error: {err}"))
        }
    }
}

fn get_http_request(lua: &Lua, (url, timeout): (Value, Value)) -> mlua::Result<mlua::String> {
    let url = to_string(lua, url);
    let timeout = to_int(lua, timeout);
    let cl = http_client(timeout);
    let Ok(handle) = Handle::try_current() else {
        return lua.create_string("");
    };
    let resp = match handle.block_on(cl.get(url).send()) {
        Ok(resp) => resp,
        Err(err) => {
            tracing::info!(comp = "", "Lua getHTTPRequest error:{err}");
            return lua.create_string(format!("http error: {err}"));
        }
    };
    match handle.block_on(resp.bytes()) {
        Ok(b) => lua.create_string(b),
        Err(err) => {
            tracing::info!(comp = "", "Lua getHTTPRequest error:{err}");
            lua.create_string(format!("http error: {err}"))
        }
    }
}

/// How long `goToPose` and `lookAroundInPlace` wait for an answer when the
/// script names no timeout.
const ACTION_TIMEOUT: Duration = Duration::from_secs(30);

/// Counts the action tags drawn so far. The gateway rejects an action whose
/// `id_tag` falls outside `FIRST_SDK_TAG..=LAST_SDK_TAG`, zero included.
static NEXT_SDK_TAG: AtomicU32 = AtomicU32::new(0);

/// The `n`th tag of the SDK window, wrapping inside it.
fn sdk_tag(n: u32) -> i32 {
    let first = pb::ActionTagConstants::FirstSdkTag as u32;
    let span = pb::ActionTagConstants::LastSdkTag as u32 - first + 1;
    (first + n % span) as i32
}

/// The bound on the best-effort cancel a timed-out action sends.
const CANCEL_TIMEOUT: Duration = Duration::from_secs(2);

fn next_sdk_tag() -> i32 {
    sdk_tag(NEXT_SDK_TAG.fetch_add(1, Ordering::Relaxed))
}

/// The script's timeout in seconds, or [`ACTION_TIMEOUT`] when it is missing,
/// zero, negative or not a number.
fn action_timeout(lua: &Lua, value: Value) -> Duration {
    match Duration::try_from_secs_f64(to_number(lua, value)) {
        Ok(timeout) if !timeout.is_zero() => timeout,
        _ => ACTION_TIMEOUT,
    }
}

/// Waits on one action call from the script's thread and hands the script the
/// decoded answer, the RPC error, or `on_timeout` once `timeout` has passed,
/// running `on_give_up` first in that last case.
fn block_on_action<F, G>(timeout: Duration, on_timeout: String, fut: F, on_give_up: G) -> String
where
    F: Future<Output = Result<String, ConnError>>,
    G: Future<Output = ()>,
{
    let Ok(handle) = Handle::try_current() else {
        return String::new();
    };
    match handle.block_on(async move { tokio::time::timeout(timeout, fut).await }) {
        Ok(Ok(answer)) => answer,
        Ok(Err(err)) => {
            tracing::info!(comp = "", "LUA: failure: {err}");
            err.to_string()
        }
        Err(_) => {
            handle.block_on(on_give_up);
            tracing::info!(comp = "", "LUA: failure: {on_timeout}");
            on_timeout
        }
    }
}

/// Drives him to `(x_mm, y_mm)` facing `angle_rad`, and returns his answer,
/// such as `RESPONSE_RECEIVED SUCCESS` or
/// `RESPONSE_RECEIVED PATH_PLANNING_FAILED`, the RPC error, or a timeout
/// message once `timeout_s` (30 by default) has passed. The robot's gateway
/// sets the status word of an action's reply to `RESPONSE_RECEIVED` whatever
/// happened, so the second word is the one to test.
///
/// - Without behaviour control he queues the action and never answers, so call
///   `assumeBehaviorControl` first. At priority 10 (`OVERRIDE_BEHAVIORS`) that
///   also turns off his cliff reaction until release: he neither stops at a
///   drop nor records it in his map, so a script that drives him should run on
///   the floor. Priority 20 (`DEFAULT`) keeps the cliff reaction on
///   (`SDKDefault.json` sets `disableCliffDetection` false) and still activates
///   the SDK behaviour an action needs.
/// - A target less than 40 mm away is planned without regard to obstacles and
///   only checked for collisions afterwards.
/// - The path is purely positional, so he turns in place at both ends.
/// - The target is read in whatever coordinate frame he is in when the request
///   arrives, and that frame is thrown away whenever he is picked up.
/// - A planning failure arrives as `PATH_PLANNING_FAILED` (the proto's
///   `PATH_PLANNING_FAILED_ABORT`) or, for an unreachable goal, most likely as
///   `FAILED_TRAVERSING_PATH`. It never arrives as `PATH_PLANNING_FAILED_RETRY`,
///   which the engine never produces.
/// - On a timeout the action is cancelled by its tag, because the robot keeps
///   it queued and would otherwise carry it out later, for instance the moment
///   the script next takes behaviour control.
fn go_to_pose(lua: &Lua, args: (Value, Value, Value, Value)) -> mlua::Result<String> {
    let x_mm = to_number(lua, args.0) as f32;
    let y_mm = to_number(lua, args.1) as f32;
    let rad = to_number(lua, args.2) as f32;
    let timeout = action_timeout(lua, args.3);
    let (mut client, session) = g_rf_ls_with_session(lua)?;
    let mut canceller = client.clone();
    let esn = session.esn().as_str().to_owned();
    let id_tag = next_sdk_tag();
    let sent = format!("x_mm={x_mm} y_mm={y_mm} rad={rad} id_tag={id_tag}");
    let timed_out = sent.clone();
    // The robot relays an action's result only while the SDK behaviour is
    // active, so a missing control lock is the usual cause, but a pick-up or a
    // cliff reaction that interrupts the behaviour ends the same way.
    let on_timeout = format!(
        "timeout after {timeout:?}: GoToPose never answered; it answers only while the script holds behavior control (assumeBehaviorControl), and a pick-up or cliff reaction can interrupt it; the move was cancelled"
    );
    Ok(block_on_action(
        timeout,
        on_timeout,
        async move {
            logged(
                COMP_LUA,
                &session,
                "GoToPose",
                &sent,
                client.go_to_pose(pb::GoToPoseRequest {
                    x_mm,
                    y_mm,
                    rad,
                    motion_prof: None,
                    id_tag,
                    num_retries: 0,
                }),
            )
            .await
            .map(|response| response.get_ref().describe())
            .map_err(|status| status_error(&status))
        },
        async move {
            // The deadline dropped the logged call before it could write its
            // line, so the timeout gets one here.
            tracing::debug!(
                target: "sdkapp",
                comp = COMP_LUA,
                bot = esn,
                "motion GoToPose({timed_out}) no answer within {timeout:?}"
            );
            // Best effort: the answer goes to the log, and the script already has
            // its timeout message.
            let cancelled = tokio::time::timeout(
                CANCEL_TIMEOUT,
                canceller.cancel_action_by_id_tag(pb::CancelActionByIdTagRequest {
                    id_tag: id_tag.unsigned_abs(),
                }),
            )
            .await;
            let outcome = match cancelled {
                Ok(Ok(_)) => "sent".to_owned(),
                Ok(Err(status)) => status_error(&status).to_string(),
                Err(_) => format!("no answer within {CANCEL_TIMEOUT:?}"),
            };
            tracing::debug!(
                target: "sdkapp",
                comp = COMP_LUA,
                bot = esn,
                "motion CancelActionByIdTag(id_tag={id_tag}) after a GoToPose timeout: {outcome}"
            );
        },
    ))
}

/// Has him look around where he stands, and returns his answer, normally
/// `RESPONSE_RECEIVED COMPLETE`, the RPC error, or a timeout message once
/// `timeout_s` (30 by default) has passed.
///
/// Like `goToPose`, it answers only while the script holds behaviour control;
/// without it the call waits out its timeout. `WONT_ACTIVATE` comes back only
/// when control is held and the look-around refuses to start. On a timeout the
/// behaviour is cancelled, because a look-around still running keeps his
/// external movement commands switched off, so the script's next move would be
/// silently ignored.
fn look_around_in_place(lua: &Lua, timeout: Value) -> mlua::Result<String> {
    let timeout = action_timeout(lua, timeout);
    let (mut client, session) = g_rf_ls_with_session(lua)?;
    let mut canceller = client.clone();
    let esn = session.esn().as_str().to_owned();
    let on_timeout = format!(
        "timeout after {timeout:?}: LookAroundInPlace never answered; it answers only while the script holds behavior control (assumeBehaviorControl); the behavior was cancelled"
    );
    Ok(block_on_action(
        timeout,
        on_timeout,
        async move {
            logged(
                COMP_LUA,
                &session,
                "LookAroundInPlace",
                "",
                client.look_around_in_place(pb::LookAroundInPlaceRequest {}),
            )
            .await
            .map(|response| response.get_ref().describe())
            .map_err(|status| status_error(&status))
        },
        async move {
            let cancelled = tokio::time::timeout(
                CANCEL_TIMEOUT,
                canceller.cancel_behavior(pb::CancelBehaviorRequest {}),
            )
            .await;
            let outcome = match cancelled {
                Ok(Ok(_)) => "sent".to_owned(),
                Ok(Err(status)) => status_error(&status).to_string(),
                Err(_) => format!("no answer within {CANCEL_TIMEOUT:?}"),
            };
            tracing::debug!(
                target: "sdkapp",
                comp = COMP_LUA,
                bot = esn,
                "motion LookAroundInPlace() no answer within {timeout:?}; CancelBehavior: {outcome}"
            );
        },
    ))
}

/// get robot from LState. Go hands back the `*vector.Vector` and every caller
/// takes `.Conn`, so this hands back that client. A missing or wrong `bot`
/// global is Go's failed type assertion, which panics; here it is an error.
pub(crate) fn g_rf_ls(lua: &Lua) -> mlua::Result<SdkClient> {
    Ok(g_rf_ls_with_session(lua)?.0)
}

/// [`g_rf_ls`] with the robot's session beside the client, for the call sites
/// that log a motion call and open its window.
pub(crate) fn g_rf_ls_with_session(lua: &Lua) -> mlua::Result<(SdkClient, Arc<SdkSession>)> {
    let ud: AnyUserData = lua.globals().get("bot")?;
    let bot = ud.borrow::<Bot>()?;
    let client = sdk_client(bot.robot.conn.as_ref()).ok_or_else(|| {
        mlua::Error::runtime("rpc error: code = Unavailable desc = no SDK client")
    })?;
    Ok((client, Arc::clone(&bot.robot.session)))
}

pub fn make_lua_state(bot: Option<Bot>) -> Result<Lua, ScriptError> {
    let lua = Lua::new();
    // Go preloads `gopher-lua-libs`, about thirty Go-implemented modules a
    // script can `require`. mlua's own standard library is what a script
    // gets here instead, so a script that requires one of those modules
    // fails where Go's would not.
    let globals = lua.globals();
    globals.set("sayText", lua.create_function(say_text)?)?;
    globals.set("playAnimation", lua.create_function(play_animation)?)?;
    globals.set("sleep", lua.create_function(sleep)?)?;
    globals.set("moveHead", lua.create_function(move_head)?)?;
    globals.set("moveLift", lua.create_function(move_lift)?)?;
    globals.set("moveWheels", lua.create_function(move_wheels)?)?;
    globals.set("showImage", lua.create_function(show_image_on_screen)?)?;
    globals.set("postHTTPRequest", lua.create_function(post_http_request)?)?;
    globals.set("getHTTPRequest", lua.create_function(get_http_request)?)?;
    // Additions with no Go counterpart.
    globals.set("goToPose", lua.create_function(go_to_pose)?)?;
    globals.set(
        "lookAroundInPlace",
        lua.create_function(look_around_in_place)?,
    )?;
    set_b_control_functions(&lua)?;
    if let Some(bot) = bot {
        let conn = Arc::clone(&bot.robot.conn);
        let handle = Handle::try_current()
            .map_err(|_| ConnError::new(StatusCode::Unavailable, "no runtime"))?;
        handle.block_on(async move {
            match tokio::time::timeout(Duration::from_secs(3), conn.battery_state()).await {
                Ok(result) => result.map(drop),
                Err(_) => Err(ConnError::deadline_exceeded()),
            }
        })?;
        globals.set("bot", lua.create_userdata(bot)?)?;
    }
    Ok(lua)
}

/// Go's goroutine path spawns onto the scheduler and its blocking path runs
/// inline; both log the failure and neither reaches the script. Off a runtime
/// there is nothing to spawn onto, so the call does nothing rather than panic.
fn execute_with_goroutine<F>(goroutine: &Value, force: bool, fut: F)
where
    F: Future<Output = Result<(), ConnError>> + Send + 'static,
{
    let goroutine = if force { true } else { to_bool(goroutine) };
    let Ok(handle) = Handle::try_current() else {
        return;
    };
    if goroutine {
        handle.spawn(async move {
            if let Err(err) = fut.await {
                tracing::info!(comp = "", "LUA: failure: {err}");
            }
        });
    } else if let Err(err) = handle.block_on(fut) {
        tracing::info!(comp = "", "LUA: failure: {err}");
    }
}

/// Go resolves the serial here through `vars.GetRobot`; the connected robot is
/// a parameter instead, so the caller owns the registry. mlua's state is not
/// `Send`, so the script runs on a blocking thread and its RPCs reach the
/// runtime through the handle.
pub async fn run_lua_script(entry: Arc<RobotEntry>, lua_script: String) -> Result<(), ScriptError> {
    tokio::task::spawn_blocking(move || {
        let lua = make_lua_state(Some(Bot {
            esn: entry.esn.clone(),
            robot: entry,
        }))?;
        lua.load(lua_script).exec()?;
        let _ = lua.load("releaseBehaviorControl()").exec();
        Ok(())
    })
    .await
    .map_err(|err| ConnError::new(StatusCode::Internal, err.to_string()))?
}

pub fn validate_lua_script(lua_script: &str) -> Result<(), ScriptError> {
    // Go discards this error and would then dereference nil; in validating mode
    // nothing here can fail but the allocation.
    let lua = make_lua_state(None)?;
    lua.load(format!("return function() {lua_script} end"))
        .exec()?;
    Ok(())
}

/// Go's `http.Error`: the message with a newline, `text/plain; charset=utf-8`
/// and `X-Content-Type-Options: nosniff`.
fn http_error(body: String) -> Response {
    let mut response = (body + "\n").into_response();
    *response.status_mut() = HttpStatus::INTERNAL_SERVER_ERROR;
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

pub async fn scripting_api(State(state): State<Arc<AppState>>, r: Request) -> Response {
    match r.uri().path() {
        "/api-lua/run_script" => {
            let f_body = match r.into_body().collect().await {
                Ok(body) => body.to_bytes(),
                Err(err) => {
                    return http_error(format!("request body couldn't be read: {err}"));
                }
            };
            let script_req: ExternalLuaRequest = match serde_json::from_slice(&f_body) {
                Ok(req) => req,
                Err(err) => {
                    return http_error(format!("request body couldn't be unmarshalled: {err}"));
                }
            };
            // `vars.GetRobot`, lifted out of `MakeLuaState`.
            let entry = match state.get_robot(&Esn::new(&script_req.esn)).await {
                Ok(entry) => entry,
                Err(err) => {
                    tracing::debug!(comp = "", "{err}");
                    return http_error(err.to_string());
                }
            };
            if let Err(err) = run_lua_script(entry, script_req.script).await {
                tracing::debug!(comp = "", "{err}");
                return http_error(err.to_string());
            }
            ().into_response()
        }
        _ => ().into_response(),
    }
}

/// The subtree prefix, registered with its trailing slash. A Go subtree pattern
/// matches its own bare prefix where axum's wildcard does not, so both forms go
/// to the same handler, as `wirepod-server`'s router does for its own prefixes.
pub const PREFIX: &str = "/api-lua/";

pub fn register_scripting_api() -> Router<Arc<AppState>> {
    Router::new()
        .route(PREFIX, any(scripting_api))
        .route("/api-lua/*rest", any(scripting_api))
}

#[cfg(test)]
mod tests {
    use std::ops::RangeInclusive;
    use std::time::Duration;

    use wirepod_core::{BotInfo, RobotConnFactory};
    use wirepod_vector::test_support::{FakeRobotHandle, spawn_fake_robot};
    use wirepod_vector::{TonicConnFactory, plaintext_builder};

    use super::*;

    /// This machine's robot. The GUID beside it is a placeholder.
    const SERIAL: &str = "00303f28";

    /// The real-clock ceiling every test runs under.
    const CEILING: Duration = Duration::from_secs(20);

    /// `FIRST_SDK_TAG..=LAST_SDK_TAG`, the only action tags the gateway takes.
    const SDK_WINDOW: RangeInclusive<i32> = 2_000_001..=3_000_000;

    async fn live_entry() -> (Arc<RobotEntry>, FakeRobotHandle) {
        let (addr, handle) = spawn_fake_robot().await;
        let bot_info: BotInfo = serde_json::from_str(&format!(
            concat!(
                r#"{{"global_guid":"global-guid-placeholder","robots":[{{"esn":"{esn}","#,
                r#""ip_address":"{ip}","guid":"robot-guid-placeholder","activated":true}}]}}"#
            ),
            esn = SERIAL,
            ip = addr
        ))
        .expect("parse the loopback fixture");
        let factory: Arc<dyn RobotConnFactory> =
            Arc::new(TonicConnFactory::with_endpoint_builder(plaintext_builder()));
        let state = AppState::builder(factory).bot_info(bot_info).build();
        let entry = state
            .get_robot(&Esn::new(SERIAL))
            .await
            .expect("dial the fake robot");
        (entry, handle)
    }

    /// [`run_lua_script`] with the script's return value handed back.
    async fn eval_script(entry: Arc<RobotEntry>, script: &'static str) -> String {
        tokio::task::spawn_blocking(move || {
            let lua = make_lua_state(Some(Bot {
                esn: entry.esn.clone(),
                robot: entry,
            }))
            .expect("the Lua state is made");
            lua.load(script)
                .eval::<String>()
                .expect("the script returns a string")
        })
        .await
        .expect("the script thread finished")
    }

    /// The RPC and arguments of the last motion call, as its log line
    /// rendered them. The fake records which RPC arrived but not its body, so
    /// this is where the values that went out are read back.
    fn last_sent(entry: &RobotEntry) -> (&'static str, String) {
        let call = entry
            .session
            .state_stream
            .motion_call()
            .expect("the call opened a motion window");
        (call.rpc, call.args)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_script_drives_the_robot_through_the_whole_surface() {
        tokio::time::timeout(CEILING, async {
            let (entry, handle) = live_entry().await;
            let script = r#"
                assumeBehaviorControl(10)
                sayText("hello", false)
                playAnimation("anim_greeting", false)
                moveHead(50)
                moveLift(50)
                moveWheels(100, 100, 0, 0)
                sleep(1)
                releaseBehaviorControl()
            "#;
            run_lua_script(Arc::clone(&entry), script.to_owned())
                .await
                .expect("the script runs");
            let methods = handle.methods();
            // The four the script waits on. `moveHead`, `moveLift` and
            // `moveWheels` are Go's forced-goroutine calls, so their arrival is
            // not ordered against the end of the script.
            for expected in [
                "BatteryState",
                "BehaviorControl",
                "say_text",
                "play_animation",
            ] {
                assert!(methods.contains(&expected), "{expected} in {methods:?}");
            }
            handle.shutdown().await;
        })
        .await
        .expect("the script finished");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn go_to_pose_returns_the_answer_and_tags_each_call_inside_the_sdk_window() {
        tokio::time::timeout(CEILING, async {
            let (entry, handle) = live_entry().await;
            let mut tags = Vec::new();
            for _ in 0..2 {
                let answer =
                    eval_script(Arc::clone(&entry), "return goToPose(200, -50, 1.5)").await;
                // The fake answers an all-default body.
                assert_eq!(answer, "UNKNOWN no-result");
                let (rpc, args) = last_sent(&entry);
                assert_eq!(rpc, "GoToPose");
                let tag: i32 = args
                    .strip_prefix("x_mm=200 y_mm=-50 rad=1.5 id_tag=")
                    .and_then(|tag| tag.parse().ok())
                    .unwrap_or_else(|| panic!("the pose and a tag in {args:?}"));
                assert!(SDK_WINDOW.contains(&tag), "{tag} inside the SDK window");
                tags.push(tag);
            }
            assert_ne!(tags[0], tags[1], "each call draws its own tag");
            let arrived = handle
                .methods()
                .into_iter()
                .filter(|method| *method == "go_to_pose")
                .count();
            assert_eq!(arrived, 2);
            handle.shutdown().await;
        })
        .await
        .expect("the script finished");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn look_around_in_place_returns_the_behavior_result() {
        tokio::time::timeout(CEILING, async {
            let (entry, handle) = live_entry().await;
            let answer = eval_script(Arc::clone(&entry), "return lookAroundInPlace()").await;
            assert_eq!(answer, "UNKNOWN INVALID_STATE");
            assert_eq!(last_sent(&entry).0, "LookAroundInPlace");
            assert!(handle.methods().contains(&"look_around_in_place"));
            handle.shutdown().await;
        })
        .await
        .expect("the script finished");
    }

    #[test]
    fn the_action_tag_wraps_inside_the_sdk_window() {
        assert_eq!(sdk_tag(0), *SDK_WINDOW.start());
        assert_eq!(sdk_tag(999_999), *SDK_WINDOW.end());
        assert_eq!(sdk_tag(1_000_000), *SDK_WINDOW.start());
        assert!(SDK_WINDOW.contains(&sdk_tag(u32::MAX)));
    }

    #[test]
    fn validate_rejects_a_syntax_error_and_the_lua_routes_register() {
        assert!(validate_lua_script("sayText(\"hi\", false)").is_ok());
        assert!(validate_lua_script("this is not lua ===").is_err());
        // `Router::route` panics on a pattern axum cannot parse.
        let _router = register_scripting_api();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_timed_out_action_cancels_before_the_script_hears_the_timeout() {
        let gave_up = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&gave_up);
        // Scripts run on a blocking thread, so the test does too.
        let answer = tokio::task::spawn_blocking(move || {
            block_on_action(
                Duration::from_millis(10),
                "timeout".to_owned(),
                std::future::pending::<Result<String, ConnError>>(),
                async move { flag.store(true, std::sync::atomic::Ordering::SeqCst) },
            )
        })
        .await
        .expect("the blocking task finished");
        assert_eq!(answer, "timeout");
        assert!(gave_up.load(std::sync::atomic::Ordering::SeqCst));
    }
}
