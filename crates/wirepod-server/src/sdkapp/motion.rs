//! The motion arms of Go's `sdkapp/server.go`: `move_wheels`, `move_lift`,
//! `move_head` and `mirror_mode`.

use axum::response::Response;
use wirepod_core::RobotEntry;
use wirepod_core::logger::COMP_SDK;
use wirepod_proto::anki::vector::external_interface as pb;
use wirepod_vector::{logged, status_error};

use crate::sdkapp::{NO_SDK_CLIENT, sdk_client};
use crate::{literals, reply};

/// Go's `strconv.Atoi` with the error discarded, so a bad value is a zero.
fn speed(raw: &str) -> f32 {
    raw.parse::<i64>().unwrap_or(0) as f32
}

/// The three motion routes all answer `fmt.Fprintf(w, "")`, which writes no
/// bytes and therefore carries no content type. The RPC error is discarded.
pub async fn move_wheels(entry: &RobotEntry, lw: &str, rw: &str) -> Response {
    let Some(mut client) = sdk_client(entry) else {
        return reply::empty();
    };
    let (lw, rw) = (speed(lw), speed(rw));
    // Go discards this, and so does the caller; the wrapper only reads the
    // answer on the way past. Nothing here takes behaviour control, so the
    // robot is free to ignore the call, and the logged body is what says so.
    let _ = logged(
        COMP_SDK,
        &entry.session,
        "DriveWheels",
        &format!("lw={lw} rw={rw}"),
        client.drive_wheels(pb::DriveWheelsRequest {
            left_wheel_mmps: lw,
            right_wheel_mmps: rw,
            left_wheel_mmps2: lw,
            right_wheel_mmps2: rw,
        }),
    )
    .await;
    reply::empty()
}

pub async fn move_lift(entry: &RobotEntry, raw: &str) -> Response {
    let Some(mut client) = sdk_client(entry) else {
        return reply::empty();
    };
    let speed = speed(raw);
    let _ = logged(
        COMP_SDK,
        &entry.session,
        "MoveLift",
        &format!("speed={speed}"),
        client.move_lift(pb::MoveLiftRequest {
            speed_rad_per_sec: speed,
        }),
    )
    .await;
    reply::empty()
}

pub async fn move_head(entry: &RobotEntry, raw: &str) -> Response {
    let Some(mut client) = sdk_client(entry) else {
        return reply::empty();
    };
    let speed = speed(raw);
    let _ = logged(
        COMP_SDK,
        &entry.session,
        "MoveHead",
        &format!("speed={speed}"),
        client.move_head(pb::MoveHeadRequest {
            speed_rad_per_sec: speed,
        }),
    )
    .await;
    reply::empty()
}

pub async fn mirror_mode(entry: &RobotEntry, enable: &str) -> Response {
    let Some(mut client) = sdk_client(entry) else {
        return reply::text(NO_SDK_CLIENT);
    };
    let enable = enable == "true";
    // One of the seven requests the robot refuses outright without behaviour
    // control, so its body status is worth reading even though Go ignores it.
    match logged(
        COMP_SDK,
        &entry.session,
        "EnableMirrorMode",
        &format!("enable={enable}"),
        client.enable_mirror_mode(pb::EnableMirrorModeRequest { enable }),
    )
    .await
    {
        Ok(_) => reply::text(literals::SUCCESS),
        // Go prints the error value rather than its message, which renders the
        // same text, and with no `error: ` prefix.
        Err(status) => reply::text(status_error(&status).to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bad_speed_is_zero_and_a_good_one_is_the_integer() {
        assert_eq!(speed("100"), 100.0);
        assert_eq!(speed("-50"), -50.0);
        assert_eq!(speed(""), 0.0);
        assert_eq!(speed("1.5"), 0.0);
    }
}
