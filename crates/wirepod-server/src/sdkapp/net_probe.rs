//! `/api-sdk/net_probe`: one timed round trip, reported as Go reports it.
//!
//! ```text
//! ctx, cancel := context.WithTimeout(r.Context(), npTimeout)
//! defer cancel()
//! start := time.Now()
//! _, err := robot.Conn.ProtocolVersion(ctx, &vectorpb.ProtocolVersionRequest{
//!     ClientVersion:  npClientVersion,
//!     MinHostVersion: npMinHostVersion,
//! })
//! rtt := time.Since(start)
//! ```
//!
//! `server.go:99-106`. Three things about that are contract.
//!
//! The verdict is never read. Go discards the response into `_` and its comment
//! says so twice: an `UNSUPPORTED` answer is still a completed round trip, and
//! this measures the link rather than the negotiation. So the success arm here
//! ignores the [`ProtocolVerdict`](wirepod_core::ProtocolVerdict) too, and the
//! loopback fake answers `UNSUPPORTED` by default precisely so that a port
//! which started reading it would fail.
//!
//! `ProtocolVersion` rather than `BatteryState` is the RPC being timed.
//! `BatteryState`, `VersionState` and `IsImageStreamingEnabled` all come back
//! at about 59 ms against a robot that pings in 2 ms, because the engine loop
//! services them; `ProtocolVersion` is answered at the gateway and lands at
//! about 14 ms. The comment at `server.go:76-91` is the reasoning, and the
//! constant [`literals::NET_PROBE_NAME`] is what tells the page which RPC
//! produced the number it is printing.
//!
//! The deadline is parented on the request context, so a client that goes away
//! ends the round trip. Axum drops the handler future when the connection
//! closes, and this awaits the RPC inline rather than spawning it, so the
//! dropped future drops the RPC future with it. Nothing is written on that
//! path, where Go writes a `Canceled` body to a connection nobody is reading;
//! that difference is deviation 19.

use std::time::Duration;

use axum::response::Response;
use serde::Serialize;
use serde_json::value::RawValue;
use wirepod_core::{AppState, ConnError, RobotEntry, go_json_f64_raw};

use crate::{literals, reply};

/// `npClientVersion` (`server.go:37`). Offered so the request is well formed,
/// not to negotiate anything.
const CLIENT_VERSION: i64 = 5;

/// `npMinHostVersion` (`server.go:38`). Zero so the robot has no cause to
/// reject on our account.
const MIN_HOST_VERSION: i64 = 0;

/// The body, whose six keys are in Go's declaration order (`server.go:47-54`).
///
/// No field has `omitempty`, so every key is present on every response,
/// including the zeros a robot whose camera has never been opened produces.
/// `camFrames` is read by no client code at all and is emitted anyway, because
/// the shape is the documented one.
///
/// `rtt_ms` is a [`RawValue`] rather than an `f64` because `serde_json`'s
/// number writer would render `0` as `0.0` and `1e21` as `1e21`, neither of
/// which is what `encoding/json` writes. [`go_json_f64_raw`] produces the Go
/// digits and this carries them through untouched.
#[derive(Serialize)]
struct NetProbe<'a> {
    #[serde(rename = "rttMs")]
    rtt_ms: &'a RawValue,
    probe: &'static str,
    target: &'a str,
    #[serde(rename = "camBytes")]
    cam_bytes: u64,
    #[serde(rename = "camFrames")]
    cam_frames: u64,
    #[serde(rename = "camOn")]
    cam_on: bool,
}

/// Times one `ProtocolVersion` round trip and answers the probe body.
pub async fn handle(state: &AppState, entry: &RobotEntry) -> Response {
    let started = state.clock().now();
    let outcome = tokio::time::timeout(
        state.timings().probe,
        entry
            .conn
            .protocol_version(CLIENT_VERSION, MIN_HOST_VERSION),
    )
    .await;
    let elapsed = state.clock().now().saturating_sub(started);

    match outcome {
        // Go assigns the response to `_`. So does this.
        Ok(Ok(_verdict)) => {}
        Ok(Err(err)) => return probe_error(&err),
        // grpc-go turns the expired context into this exact status, and the
        // dashboard prints whatever follows `error: ` verbatim. A failed RPC
        // took however long the deadline was, which says nothing about the
        // link, so the page counts it as a lost probe rather than averaging
        // the deadline into the latency figure (`server.go:107-112`).
        Err(_elapsed) => return probe_error(&ConnError::deadline_exceeded()),
    }

    // Read after the round trip, as Go reads it (`server.go:115`), and from the
    // meters that outlive the robot entry, so an idle eviction and a reconnect
    // do not look like a server restart to the page's differencing.
    let (cam_bytes, cam_frames) = state.registry().read_meter(&entry.esn);
    let rtt_ms = match go_json_f64_raw(rtt_millis(elapsed)) {
        Ok(rtt_ms) => rtt_ms,
        // Unreachable: an elapsed duration is finite and non-negative, so the
        // only three values `encoding/json` refuses cannot arise. Go's own
        // marshal failure writes `error: ` and the message, and `GoJsonError`
        // renders the same text, so the arm is quoted rather than invented.
        Err(err) => return reply::text(format!("{}{err}", literals::ERROR_PREFIX)),
    };
    let target = entry.target.grpc_target();
    let body = NetProbe {
        rtt_ms: &rtt_ms,
        probe: literals::NET_PROBE_NAME,
        target: &target,
        cam_bytes,
        cam_frames,
        cam_on: entry.session.cam.is_streaming(),
    };
    match serde_json::to_string(&body) {
        Ok(body) => reply::text(body),
        // Unreachable for this shape, as it is in Go (`server.go:123-126`).
        Err(err) => reply::text(format!("{}{err}", literals::ERROR_PREFIX)),
    }
}

/// A lost probe: `error: ` and the gRPC status text, at HTTP 200.
///
/// `fetchProbe` strips the prefix and shows the remainder in the connectivity
/// panel, whitespace-collapsed and truncated at 40 characters
/// (`webroot/sdkapp/js/vectorbrain.js:868-887`, `:942-949`), so this is the one
/// place a server error string reaches the user verbatim.
fn probe_error(err: &ConnError) -> Response {
    reply::text(format!("{}{err}", literals::ERROR_PREFIX))
}

/// Go's `float64(rtt.Microseconds()) / 1000` (`server.go:117`).
///
/// The units are what matter. `Duration.Microseconds` is integer division of
/// the nanosecond count, so the round trip is truncated to whole microseconds
/// *before* it is divided, which is why the number on the wire never has more
/// than three decimal places and why a sub-microsecond round trip reports a
/// flat `0` rather than a long fraction.
///
/// A Go `time.Duration` is an `int64` of nanoseconds and cannot hold more than
/// about 292 years, so its microsecond count always fits an `i64`. A Rust
/// [`Duration`] has a much wider range, and the saturation stands in for that
/// limit rather than for a case any clock can reach.
fn rtt_millis(elapsed: Duration) -> f64 {
    let micros = i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX);
    micros as f64 / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_round_trip_is_truncated_to_whole_microseconds_before_the_divide() {
        // Go's own example figure, and the shape the page prints.
        assert_eq!(rtt_millis(Duration::from_micros(13_482)), 13.482);
        // Truncation, not rounding: 999 nanoseconds is zero microseconds.
        assert_eq!(rtt_millis(Duration::from_nanos(999)), 0.0);
        assert_eq!(rtt_millis(Duration::from_nanos(1_999)), 0.001);
        assert_eq!(rtt_millis(Duration::ZERO), 0.0);
        assert_eq!(rtt_millis(Duration::from_millis(1)), 1.0);
        assert_eq!(rtt_millis(Duration::from_secs(5)), 5000.0);
    }

    #[test]
    fn the_versions_offered_are_gos_constants() {
        assert_eq!(CLIENT_VERSION, 5);
        assert_eq!(MIN_HOST_VERSION, 0);
    }
}
