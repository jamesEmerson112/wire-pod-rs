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

use std::future::Future;
use std::sync::Arc;
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
use wirepod_core::{AppState, ConnError, Esn, RobotEntry, StatusCode};
use wirepod_proto::anki::vector::external_interface as pb;
use wirepod_vector::{SdkClient, sdk_client, status_error};

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

/// `L.ToBool(n)`: only nil and false are false.
fn to_bool(value: &Value) -> bool {
    !matches!(value, Value::Nil | Value::Boolean(false))
}

fn say_text(lua: &Lua, (text, goroutine): (Value, Value)) -> mlua::Result<()> {
    let text_to_say = to_string(lua, text);
    let mut client = g_rf_ls(lua)?;
    execute_with_goroutine(&goroutine, false, async move {
        client
            .say_text(pb::SayTextRequest {
                text: text_to_say,
                use_vector_voice: true,
                duration_scalar: 1.0,
                pitch_scalar: 0.0,
            })
            .await
            .map(drop)
            .map_err(|status| status_error(&status))
    });
    Ok(())
}

fn play_animation(lua: &Lua, (animation, goroutine): (Value, Value)) -> mlua::Result<()> {
    let anim_to_play = to_string(lua, animation);
    let mut client = g_rf_ls(lua)?;
    execute_with_goroutine(&goroutine, false, async move {
        client
            .play_animation(pb::PlayAnimationRequest {
                animation: Some(pb::Animation { name: anim_to_play }),
                loops: 1,
                ..pb::PlayAnimationRequest::default()
            })
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
    let mut client = g_rf_ls(lua)?;
    execute_with_goroutine(&Value::Nil, true, async move {
        client
            .move_head(pb::MoveHeadRequest {
                speed_rad_per_sec: head_speed_f,
            })
            .await
            .map(drop)
            .map_err(|status| status_error(&status))
    });
    Ok(())
}

fn move_lift(lua: &Lua, speed: Value) -> mlua::Result<()> {
    let lift_speed = to_int(lua, speed);
    let lift_speed_f = lift_speed as f32 / 100.0;
    let mut client = g_rf_ls(lua)?;
    execute_with_goroutine(&Value::Nil, true, async move {
        client
            .move_lift(pb::MoveLiftRequest {
                speed_rad_per_sec: lift_speed_f,
            })
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
    let mut client = g_rf_ls(lua)?;
    execute_with_goroutine(&Value::Nil, true, async move {
        client
            .drive_wheels(pb::DriveWheelsRequest {
                left_wheel_mmps: left_wheel_speed as f32,
                right_wheel_mmps: right_wheel_speed as f32,
                left_wheel_mmps2: left_wheel_speed2 as f32,
                right_wheel_mmps2: right_wheel_speed2 as f32,
            })
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

/// get robot from LState. Go hands back the `*vector.Vector` and every caller
/// takes `.Conn`, so this hands back that client. A missing or wrong `bot`
/// global is Go's failed type assertion, which panics; here it is an error.
pub(crate) fn g_rf_ls(lua: &Lua) -> mlua::Result<SdkClient> {
    let ud: AnyUserData = lua.globals().get("bot")?;
    let bot = ud.borrow::<Bot>()?;
    sdk_client(bot.robot.conn.as_ref())
        .ok_or_else(|| mlua::Error::runtime("rpc error: code = Unavailable desc = no SDK client"))
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
    use std::time::Duration;

    use wirepod_core::{BotInfo, RobotConnFactory};
    use wirepod_vector::test_support::{FakeRobotHandle, spawn_fake_robot};
    use wirepod_vector::{TonicConnFactory, plaintext_builder};

    use super::*;

    /// This machine's robot. The GUID beside it is a placeholder.
    const SERIAL: &str = "00303f28";

    /// The real-clock ceiling every test runs under.
    const CEILING: Duration = Duration::from_secs(20);

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

    #[test]
    fn validate_rejects_a_syntax_error_and_the_lua_routes_register() {
        assert!(validate_lua_script("sayText(\"hi\", false)").is_ok());
        assert!(validate_lua_script("this is not lua ===").is_err());
        // `Router::route` panics on a pattern axum cannot parse.
        let _router = register_scripting_api();
    }
}
