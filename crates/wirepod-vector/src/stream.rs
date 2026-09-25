//! Adapters from `tonic::Streaming` to the core stream traits.
//!
//! Both are thin: they wait for the next message, map the failure into a
//! [`ConnError`], and project the one or two fields the slice reads. Keeping
//! the projection here is what lets `wirepod-core` express the event loop and
//! the frame pump in domain types.

use async_trait::async_trait;
use tonic::Streaming;
use wirepod_core::{
    CameraFrame, ConnError, EventItem, EventReceiver, FrameStream, RobotStateSample, StimEvent,
};
use wirepod_proto::anki::vector::external_interface as pb;

use crate::error::status_error;

/// An [`EventReceiver`] over the robot's `EventStream`.
pub struct TonicEventReceiver {
    inner: Streaming<pb::EventResponse>,
}

impl TonicEventReceiver {
    /// Wraps an open event stream.
    pub fn new(inner: Streaming<pb::EventResponse>) -> Self {
        Self { inner }
    }
}

/// The [`EventItem`] one `EventResponse` carries.
///
/// Anything that is neither a stimulation nor a state event becomes
/// [`EventItem::Other`]. Go reaches the same place by calling
/// `resp.Event.GetStimulationInfo()`, which answers a nil pointer for every
/// other event type (`server.go:655-659`); it never asks for state at all.
fn classify(response: pb::EventResponse) -> EventItem {
    match response.event.and_then(|event| event.event_type) {
        Some(pb::event::EventType::StimulationInfo(info)) => EventItem::Stim(StimEvent {
            value: info.value,
            velocity: info.velocity,
        }),
        Some(pb::event::EventType::RobotState(state)) => EventItem::State(RobotStateSample {
            status: state.status,
            // The pose is a message, so prost makes it optional. A state event
            // without one is a robot that has no frame to report in, which
            // reads the same as the origin-zero default.
            x_mm: state.pose.as_ref().map_or(0.0, |pose| pose.x),
            y_mm: state.pose.as_ref().map_or(0.0, |pose| pose.y),
            angle_rad: state.pose_angle_rad,
            origin_id: state.pose.as_ref().map_or(0, |pose| pose.origin_id),
            localized_to_object_id: state.localized_to_object_id,
        }),
        _ => EventItem::Other,
    }
}

#[async_trait]
impl EventReceiver for TonicEventReceiver {
    async fn next(&mut self) -> Result<Option<EventItem>, ConnError> {
        match self.inner.message().await {
            Ok(Some(response)) => Ok(Some(classify(response))),
            Ok(None) => Ok(None),
            Err(status) => Err(status_error(&status)),
        }
    }
}

/// A [`FrameStream`] over the robot's `CameraFeed`.
pub struct TonicFrameStream {
    inner: Streaming<pb::CameraFeedResponse>,
}

impl TonicFrameStream {
    /// Wraps an open camera feed.
    pub fn new(inner: Streaming<pb::CameraFeedResponse>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl FrameStream for TonicFrameStream {
    async fn next(&mut self) -> Result<Option<CameraFrame>, ConnError> {
        match self.inner.message().await {
            // Only `data` is carried. Go reads the same one field with
            // `response.GetData()` and ignores the timestamp, the image id and
            // the encoding (`server.go:768`).
            Ok(Some(response)) => Ok(Some(CameraFrame {
                data: response.data,
            })),
            Ok(None) => Ok(None),
            Err(status) => Err(status_error(&status)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stimulation_event_carries_its_value_and_velocity() {
        let response = pb::EventResponse {
            status: None,
            event: Some(pb::Event {
                event_type: Some(pb::event::EventType::StimulationInfo(pb::StimulationInfo {
                    value: 0.75,
                    velocity: 1.5,
                    ..pb::StimulationInfo::default()
                })),
            }),
        };
        assert_eq!(
            classify(response),
            EventItem::Stim(StimEvent {
                value: 0.75,
                velocity: 1.5
            })
        );
    }

    #[test]
    fn any_other_event_is_other() {
        let response = pb::EventResponse {
            status: None,
            event: Some(pb::Event {
                event_type: Some(pb::event::EventType::WakeWord(pb::WakeWord::default())),
            }),
        };
        assert_eq!(classify(response), EventItem::Other);
    }

    #[test]
    fn an_empty_event_is_other() {
        assert_eq!(classify(pb::EventResponse::default()), EventItem::Other);
    }
}
