//! Go's `sdkapp/bcassume.go`, with the two behaviour-control arms of
//! `server.go` that drive it.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::response::Response;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use wirepod_core::RobotEntry;
use wirepod_core::logger::COMP_SDK;
use wirepod_proto::anki::vector::external_interface as pb;
use wirepod_vector::motionlog::{control_granted, control_released};
use wirepod_vector::status_error;

use crate::sdkapp::sdk_client;
use crate::{literals, reply};

/// How often Go's outer goroutine re-reads `BcAssumption`.
const POLL: Duration = Duration::from_millis(500);

fn control_request(priority: &str) -> pb::BehaviorControlRequest {
    let priority = if priority == "high" {
        pb::control_request::Priority::OverrideBehaviors
    } else {
        pb::control_request::Priority::Default
    };
    pb::BehaviorControlRequest {
        request_type: Some(pb::behavior_control_request::RequestType::ControlRequest(
            pb::ControlRequest {
                priority: priority as i32,
            },
        )),
    }
}

fn control_release() -> pb::BehaviorControlRequest {
    pb::BehaviorControlRequest {
        request_type: Some(pb::behavior_control_request::RequestType::ControlRelease(
            pb::ControlRelease {},
        )),
    }
}

/// `server.go:325-328`: `success` goes out before the stream is opened.
///
/// Go runs two goroutines, an inner one that holds the stream and an outer one
/// that polls the flag every 500 ms and then signals it through a channel.
/// They become one task here, which removes the hot `default: continue` spin
/// Go's inner loop runs and changes nothing else.
pub fn assume(entry: &RobotEntry, priority: &str) -> Response {
    let response = reply::text(literals::SUCCESS);
    entry.session.bc_assumption.store(true, Ordering::SeqCst);
    let Some(mut client) = sdk_client(entry) else {
        return response;
    };
    let request = control_request(priority);
    let level = match &request.request_type {
        Some(pb::behavior_control_request::RequestType::ControlRequest(asked)) => asked.priority,
        _ => 0,
    };
    let session = Arc::clone(&entry.session);
    let esn = entry.esn.as_str().to_owned();
    tokio::spawn(async move {
        let (sender, receiver) = mpsc::channel(4);
        // Queued before the call rather than after it, because tonic's call
        // returns on the response headers and a gateway that waits for the
        // first request message would otherwise never send them.
        if sender.send(request).await.is_err() {
            return;
        }
        let mut stream = match client.behavior_control(ReceiverStream::new(receiver)).await {
            Ok(stream) => stream.into_inner(),
            Err(status) => {
                tracing::error!(target: COMP_SDK, "behavior control: {}", status_error(&status));
                return;
            }
        };
        loop {
            match stream.message().await {
                Ok(Some(resp)) => {
                    if matches!(
                        resp.response_type,
                        Some(
                            pb::behavior_control_response::ResponseType::ControlGrantedResponse(_)
                        )
                    ) {
                        control_granted(COMP_SDK, &esn, level);
                        break;
                    }
                }
                Ok(None) => return,
                Err(status) => {
                    tracing::error!(target: COMP_SDK, "behavior control: {}", status_error(&status));
                    return;
                }
            }
        }
        while session.bc_assumption.load(Ordering::SeqCst) {
            tokio::time::sleep(POLL).await;
        }
        control_released(COMP_SDK, &esn);
        let _ = sender.send(control_release()).await;
    });
    response
}

/// `server.go:329-332`.
pub fn release(entry: &RobotEntry) -> Response {
    entry.session.bc_assumption.store(false, Ordering::SeqCst);
    reply::text(literals::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_high_priority_overrides_behaviors() {
        let high = control_request("high");
        let plain = control_request("");
        let priority = |request: &pb::BehaviorControlRequest| match &request.request_type {
            Some(pb::behavior_control_request::RequestType::ControlRequest(control)) => {
                control.priority
            }
            _ => -1,
        };
        assert_eq!(
            priority(&high),
            pb::control_request::Priority::OverrideBehaviors as i32
        );
        assert_eq!(
            priority(&plain),
            pb::control_request::Priority::Default as i32
        );
        assert!(matches!(
            control_release().request_type,
            Some(pb::behavior_control_request::RequestType::ControlRelease(_))
        ));
    }
}
