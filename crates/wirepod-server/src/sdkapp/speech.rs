//! `say_text`, `play_sound`, `cloud_intent`, the Alexa routes,
//! `trigger_wake_word`, `get_battery`, `get_robot_stats` and
//! `print_robot_info` from Go's `sdkapp/server.go`.

use std::time::Duration;

use axum::response::Response;
use http::{HeaderValue, StatusCode, header};
use serde::Serialize;
use serde_json::value::RawValue;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use wirepod_core::logger::COMP_SDK;
use wirepod_core::{ConnError, JdocKind, RobotEntry, go_json_f32_raw};
use wirepod_proto::anki::vector::external_interface as pb;
use wirepod_vector::status_error;

use crate::sdkapp::{NO_SDK_CLIENT, sdk_client};
use crate::{literals, reply};

/// The path whose body is a multipart upload rather than a form.
pub const PLAY_SOUND_PATH: &str = "/api-sdk/play_sound";

/// Go's `len([]rune(text)) >= 600`.
const SAY_TEXT_LIMIT: usize = 600;

/// `server.go:335`.
const TEXT_TOO_LONG: &str = "error: text is too long";

/// The deadline `get_battery` puts on its own context.
const BATTERY_TIMEOUT: Duration = Duration::from_secs(15);

/// The audio stream's sample rate and volume (`server.go:256-259`).
const AUDIO_FRAME_RATE: u32 = 8000;
const AUDIO_VOLUME: u32 = 100;

/// The chunk size Go slices the upload into, and the gap between chunks.
const AUDIO_CHUNK: usize = 1024;
const AUDIO_GAP: Duration = Duration::from_millis(60);

/// The robot's console-variable port and the wake-word variable.
const CONSOLE_PORT: u16 = 8889;
const CONSOLE_QUERY: &str = "consolevarset?key=FakeButtonPressType&value=singlePressDetected";
const CONSOLE_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn say_text(entry: &RobotEntry, text: &str) -> Response {
    // Go writes the refusal and then says the text anyway, because the arm has
    // no `return`, so an over-long request answers both bodies concatenated.
    let mut body = String::new();
    if text.chars().count() >= SAY_TEXT_LIMIT {
        body.push_str(TEXT_TOO_LONG);
    }
    body.push_str(literals::SUCCESS);
    let Some(mut client) = sdk_client(entry) else {
        return reply::text(body);
    };
    let _ = client
        .say_text(pb::SayTextRequest {
            text: text.to_owned(),
            use_vector_voice: true,
            duration_scalar: 1.0,
            pitch_scalar: 0.0,
        })
        .await;
    reply::text(body)
}

pub async fn cloud_intent(entry: &RobotEntry, intent: &str) -> Response {
    let Some(mut client) = sdk_client(entry) else {
        return reply::text(literals::DONE);
    };
    let _ = client
        .app_intent(pb::AppIntentRequest {
            intent: intent.to_owned(),
            param: String::new(),
        })
        .await;
    reply::text(literals::DONE)
}

pub async fn alexa_opt_in(entry: &RobotEntry, opt_in: bool) -> Response {
    let Some(mut client) = sdk_client(entry) else {
        return reply::text(literals::SUCCESS);
    };
    let _ = client.alexa_opt_in(pb::AlexaOptInRequest { opt_in }).await;
    reply::text(literals::SUCCESS)
}

/// `server.go:231-281`: stream the upload to the robot in 1024-byte chunks.
///
/// Nothing is written to the response on any path, so the reply is zero bytes
/// with no content type.
pub async fn play_sound(entry: &RobotEntry, sound: Option<Vec<u8>>) -> Response {
    let Some(pcm) = sound else {
        tracing::error!(target: COMP_SDK, "Error retrieving the file: no sound part");
        return reply::empty();
    };
    let Some(mut client) = sdk_client(entry) else {
        return reply::empty();
    };
    let (sender, receiver) = mpsc::channel(4);
    // Go discards this error and then sends on a nil client.
    let opened = client
        .external_audio_stream_playback(ReceiverStream::new(receiver))
        .await;
    if let Err(status) = opened {
        tracing::error!(target: COMP_SDK, "play sound: {}", status_error(&status));
        return reply::empty();
    }

    let prepare = pb::external_audio_stream_request::AudioRequestType::AudioStreamPrepare(
        pb::ExternalAudioStreamPrepare {
            audio_frame_rate: AUDIO_FRAME_RATE,
            audio_volume: AUDIO_VOLUME,
        },
    );
    if sender.send(audio_request(prepare)).await.is_err() {
        return reply::empty();
    }
    // Go's loop keeps only whole chunks and drops the remainder.
    for chunk in pcm.chunks_exact(AUDIO_CHUNK) {
        let message = pb::external_audio_stream_request::AudioRequestType::AudioStreamChunk(
            pb::ExternalAudioStreamChunk {
                audio_chunk_size_bytes: chunk.len() as u32,
                audio_chunk_samples: chunk.to_vec(),
            },
        );
        if sender.send(audio_request(message)).await.is_err() {
            return reply::empty();
        }
        tokio::time::sleep(AUDIO_GAP).await;
    }
    let complete = pb::external_audio_stream_request::AudioRequestType::AudioStreamComplete(
        pb::ExternalAudioStreamComplete {},
    );
    let _ = sender.send(audio_request(complete)).await;
    reply::empty()
}

fn audio_request(
    kind: pb::external_audio_stream_request::AudioRequestType,
) -> pb::ExternalAudioStreamRequest {
    pb::ExternalAudioStreamRequest {
        audio_request_type: Some(kind),
    }
}

/// Go's `r.FormFile(name)`: the bytes of one part of a multipart body.
pub fn form_file(content_type: Option<&str>, body: &[u8], name: &str) -> Option<Vec<u8>> {
    let boundary = content_type?
        .split(';')
        .find_map(|part| part.trim().strip_prefix("boundary="))?
        .trim_matches('"');
    let separator = format!("\r\n--{boundary}");
    let marker = format!("name=\"{name}\"");
    let mut at = find(body, format!("--{boundary}").as_bytes(), 0)?;
    loop {
        let head = find(body, b"\r\n\r\n", at)?;
        let start = head + 4;
        let end = find(body, separator.as_bytes(), start)?;
        if String::from_utf8_lossy(&body[at..head]).contains(&marker) {
            return Some(body[start..end].to_vec());
        }
        at = end + separator.len();
    }
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|index| index + from)
}

/// `ResponseStatus` as `json.Marshal` renders the generated Go struct.
#[derive(Serialize)]
struct Status {
    #[serde(skip_serializing_if = "zero_i32")]
    code: i32,
}

#[derive(Serialize)]
struct CubeBattery<'a> {
    #[serde(skip_serializing_if = "zero_i32")]
    level: i32,
    #[serde(skip_serializing_if = "empty")]
    factory_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    battery_volts: Option<Box<RawValue>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_since_last_reading_sec: Option<Box<RawValue>>,
}

#[derive(Serialize)]
struct BatteryState<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<Status>,
    #[serde(skip_serializing_if = "zero_i32")]
    battery_level: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    battery_volts: Option<Box<RawValue>>,
    #[serde(skip_serializing_if = "not_set")]
    is_charging: bool,
    #[serde(skip_serializing_if = "not_set")]
    is_on_charger_platform: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    suggested_charger_sec: Option<Box<RawValue>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cube_battery: Option<CubeBattery<'a>>,
}

fn zero_i32(value: &i32) -> bool {
    *value == 0
}

fn empty(value: &&str) -> bool {
    value.is_empty()
}

fn not_set(value: &bool) -> bool {
    !*value
}

/// A `float32` field under `omitempty`: absent at zero, and otherwise
/// formatted the way `encoding/json` formats a 32-bit float.
fn go_float(value: f32) -> Option<Box<RawValue>> {
    if value == 0.0 {
        return None;
    }
    go_json_f32_raw(value).ok()
}

fn battery_json(resp: &pb::BatteryStateResponse) -> Option<String> {
    let wire = BatteryState {
        status: resp
            .status
            .as_ref()
            .map(|status| Status { code: status.code }),
        battery_level: resp.battery_level,
        battery_volts: go_float(resp.battery_volts),
        is_charging: resp.is_charging,
        is_on_charger_platform: resp.is_on_charger_platform,
        suggested_charger_sec: go_float(resp.suggested_charger_sec),
        cube_battery: resp.cube_battery.as_ref().map(|cube| CubeBattery {
            level: cube.level,
            factory_id: &cube.factory_id,
            battery_volts: go_float(cube.battery_volts),
            time_since_last_reading_sec: go_float(cube.time_since_last_reading_sec),
        }),
    };
    serde_json::to_string(&wire).ok()
}

pub async fn get_battery(entry: &RobotEntry) -> Response {
    let Some(mut client) = sdk_client(entry) else {
        return reply::text(format!("{}{NO_SDK_CLIENT}", literals::ERROR_PREFIX));
    };
    let outcome = tokio::time::timeout(
        BATTERY_TIMEOUT,
        client.battery_state(pb::BatteryStateRequest {}),
    )
    .await;
    let resp = match outcome {
        Ok(Ok(resp)) => resp.into_inner(),
        Ok(Err(status)) => {
            return reply::text(format!(
                "{}{}",
                literals::ERROR_PREFIX,
                status_error(&status)
            ));
        }
        Err(_) => {
            return reply::text(format!(
                "{}{}",
                literals::ERROR_PREFIX,
                ConnError::deadline_exceeded()
            ));
        }
    };
    match battery_json(&resp) {
        Some(body) => reply::text(body),
        None => reply::text(format!("{}json: unsupported value", literals::ERROR_PREFIX)),
    }
}

/// `server.go:591-601`: the lifetime-stats jdoc, straight out to the wire with
/// no caching and no disk write.
pub async fn get_robot_stats(entry: &RobotEntry) -> Response {
    match entry.conn.pull_jdocs(&[JdocKind::RobotLifetimeStats]).await {
        // Go indexes `NamedJdocs[0]`; the seam refuses an empty answer instead.
        Ok(named) => match named.into_iter().next() {
            Some(first) => reply::text(first.doc.json_doc),
            None => reply::text(format!("{}no documents", literals::ERROR_PREFIX)),
        },
        Err(err) => reply::text(format!("{}{err}", literals::ERROR_PREFIX)),
    }
}

/// Go prints the SDK's `*vector.Vector`, which is its struct default
/// formatting. The port prints the two fields that identify the robot and
/// leaves out the GUID the SDK value also carries.
pub fn print_robot_info(entry: &RobotEntry) -> Response {
    reply::text(format!("&{{{} {}}}", entry.esn, entry.target.grpc_target()))
}

/// `server.go:609-630`: one plain HTTP GET to the robot's console-variable
/// port. Both failures are `http.Error`, which appends a newline and sets
/// nosniff.
pub async fn trigger_wake_word(entry: &RobotEntry) -> Response {
    // Go splits its own `ip:443` target rather than reading the address.
    let target = entry.target.grpc_target();
    let robot_ip = target.split(':').next().unwrap_or_default();
    let url = format!("http://{robot_ip}:{CONSOLE_PORT}/{CONSOLE_QUERY}");
    let client = match reqwest::Client::builder().timeout(CONSOLE_TIMEOUT).build() {
        Ok(client) => client,
        Err(err) => {
            return http_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to trigger wake word: {err}\n"),
            );
        }
    };
    let resp = match client.get(url).send().await {
        Ok(resp) => resp,
        Err(err) => {
            return http_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to trigger wake word: {err}\n"),
            );
        }
    };
    if resp.status() != reqwest::StatusCode::OK {
        let status = StatusCode::from_u16(resp.status().as_u16())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        return http_error(status, "Consolevars returned error\n".to_owned());
    }
    reply::text(literals::SUCCESS)
}

/// Go's `http.Error` with a body built at runtime, which
/// [`reply::go_error`](crate::reply::go_error) cannot take.
fn http_error(status: StatusCode, body: String) -> Response {
    let mut response = reply::text(body);
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static(literals::NOSNIFF),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_battery_body_drops_every_zero_field() {
        let body = battery_json(&pb::BatteryStateResponse {
            status: Some(pb::ResponseStatus { code: 1 }),
            battery_level: 3,
            battery_volts: 3.9210937,
            is_on_charger_platform: true,
            ..pb::BatteryStateResponse::default()
        })
        .expect("a finite battery reading marshals");
        // The shape the dashboard documents in `sdkapp/js/common.js:36`.
        assert_eq!(
            body,
            r#"{"status":{"code":1},"battery_level":3,"battery_volts":3.9210937,"is_on_charger_platform":true}"#
        );
        assert_eq!(
            battery_json(&pb::BatteryStateResponse::default()).expect("the zero reading marshals"),
            "{}"
        );
    }

    #[test]
    fn the_sound_part_is_the_bytes_between_its_headers_and_the_next_boundary() {
        let body = concat!(
            "--X\r\nContent-Disposition: form-data; name=\"other\"\r\n\r\nno\r\n",
            "--X\r\nContent-Disposition: form-data; name=\"sound\"; filename=\"a.pcm\"\r\n\r\n",
            "PCM\r\n--X--\r\n"
        );
        assert_eq!(
            form_file(
                Some("multipart/form-data; boundary=X"),
                body.as_bytes(),
                "sound"
            ),
            Some(b"PCM".to_vec())
        );
        assert_eq!(
            form_file(
                Some("multipart/form-data; boundary=X"),
                body.as_bytes(),
                "absent"
            ),
            None
        );
        assert_eq!(
            form_file(Some("application/json"), body.as_bytes(), "sound"),
            None
        );
    }
}
