//! Go's `servers/chipper/connectioncheck.go`: the robot's gRPC reachability probe.

use std::time::{Duration, Instant};

use tonic::{Request, Response, Status, Streaming};
use wirepod_proto::chippergrpc2 as pb;

use crate::chipper::server::Server;
use crate::vtt::{ResponseStream, response_channel};

const CONNECTION_CHECK_TIMEOUT: Duration = Duration::from_secs(15);

/// StreamingConnectionCheck is used by the end device to make sure it can successfully communicate
pub async fn streaming_connection_check(
    _server: &Server,
    request: Request<Streaming<pb::StreamingConnectionCheckRequest>>,
) -> Result<Response<ResponseStream<pb::ConnectionCheckResponse>>, Status> {
    let mut stream = request.into_inner();
    // Go logs the device id before it tests the error, which dereferences nil
    // when the first receive fails; the log moves below the test.
    let req = match stream.message().await {
        Ok(Some(req)) => req,
        Ok(None) => {
            tracing::error!(target: "conn", "conn check error: stream closed before the first request");
            return Err(Status::invalid_argument("no connection check request"));
        }
        Err(status) => {
            tracing::error!(target: "conn", "conn check error: {status}");
            return Err(status);
        }
    };
    tracing::debug!(target: "conn", bot = %req.device_id, "incoming connection check");
    let device_id = req.device_id.clone();

    let deadline = Instant::now() + CONNECTION_CHECK_TIMEOUT;

    // Go divides without a guard, which panics when the robot sends zero.
    if req.audio_per_request == 0 {
        tracing::error!(target: "conn", bot = %device_id, "conn check error: audio_per_request is zero");
        return Err(Status::invalid_argument("audio_per_request is zero"));
    }
    let frames_per_request = req.total_audio_ms / req.audio_per_request;

    let mut to_send = pb::ConnectionCheckResponse::default();

    // count frames, we already pulled the first one
    let mut frames: u32 = 1;
    to_send.frames_received = frames;
    let mut err = None;
    loop {
        // Go's select carries a `default` arm, so the deadline is only read
        // between receives and never interrupts one.
        if Instant::now() >= deadline {
            tracing::debug!(target: "conn", bot = %device_id, "expired, frames received {frames}");
            to_send.status = "Timeout".to_owned();
            break;
        }
        match stream.message().await {
            Ok(Some(_)) => {
                frames += 1;
                to_send.frames_received = frames;
                if frames >= frames_per_request {
                    tracing::debug!(target: "conn", bot = %device_id, "success");
                    to_send.status = "Success".to_owned();
                    break;
                }
            }
            // Go reads the nil error's message here, which panics.
            Ok(None) => {
                tracing::error!(target: "conn", bot = %device_id, "conn check error: stream closed");
                to_send.status = "Error".to_owned();
                break;
            }
            Err(status) => {
                tracing::error!(target: "conn", bot = %device_id, "conn check error: {status}");
                err = Some(status);
                to_send.status = "Error".to_owned();
                break;
            }
        }
    }

    // Go sends the response and then returns the receive error, which ends the
    // RPC with a non-OK status; the two go down the one response stream here.
    let (send, responses) = response_channel();
    if send.try_send(Ok(to_send)).is_err() {
        tracing::error!(target: "conn", bot = %device_id, "failed to send response");
    }
    if let Some(err) = err {
        let _ = send.try_send(Err(err));
    }
    Ok(Response::new(responses))
}
