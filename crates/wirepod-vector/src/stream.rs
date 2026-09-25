//! Adapters from `tonic::Streaming` to the core stream traits.
//!
//! All three are thin: they wait for the next message, map the failure into a
//! [`ConnError`], and project the fields the slice reads. Keeping the
//! projection here is what lets `wirepod-core` express the event loop, the
//! frame pump and the map feed in domain types.

use async_trait::async_trait;
use tonic::Streaming;
use wirepod_core::robot::conn::NavMapReceiver;
use wirepod_core::robot::navmap::{NavMapFrame, NavMapInfo, NavMapQuad};
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

/// A [`NavMapReceiver`] over the robot's `NavMapFeed`.
pub struct TonicNavMapReceiver {
    inner: Streaming<pb::NavMapFeedResponse>,
}

impl TonicNavMapReceiver {
    /// Wraps an open nav map feed.
    pub fn new(inner: Streaming<pb::NavMapFeedResponse>) -> Self {
        Self { inner }
    }
}

/// The [`NavMapFrame`] one `NavMapFeedResponse` carries.
///
/// `map_info` is a message, so prost makes it optional; an absent one reads as
/// all zeros, a root with no size and no height. `root_center_z` is dropped,
/// because the robot's gateway always sends zero there.
fn nav_map_frame(response: pb::NavMapFeedResponse) -> NavMapFrame {
    let info = response.map_info.unwrap_or_default();
    NavMapFrame {
        origin_id: response.origin_id,
        info: NavMapInfo {
            root_depth: info.root_depth,
            root_size_mm: info.root_size_mm,
            root_center_x: info.root_center_x,
            root_center_y: info.root_center_y,
        },
        quads: response
            .quad_infos
            .into_iter()
            .map(|quad| NavMapQuad {
                content: quad.content,
                depth: quad.depth,
                rgba: quad.color_rgba,
            })
            .collect(),
    }
}

#[async_trait]
impl NavMapReceiver for TonicNavMapReceiver {
    async fn next(&mut self) -> Result<Option<NavMapFrame>, ConnError> {
        match self.inner.message().await {
            Ok(Some(response)) => Ok(Some(nav_map_frame(response))),
            Ok(None) => Ok(None),
            Err(status) => Err(status_error(&status)),
        }
    }
}

#[cfg(test)]
mod tests {
    use wirepod_core::robot::navmap::NavContent;

    use super::*;

    #[test]
    fn a_nav_map_response_keeps_every_quad_in_order() {
        let response = pb::NavMapFeedResponse {
            origin_id: 7,
            map_info: Some(pb::NavMapInfo {
                root_depth: 6,
                root_size_mm: 512.0,
                root_center_x: 64.0,
                root_center_y: -32.0,
                root_center_z: 0.0,
            }),
            quad_infos: vec![
                pb::NavMapQuadInfo {
                    content: pb::NavNodeContentType::NavNodeClearOfObstacle as i32,
                    depth: 5,
                    color_rgba: 0x00ff_00ff,
                },
                pb::NavMapQuadInfo {
                    content: pb::NavNodeContentType::NavNodeCliff as i32,
                    depth: 0,
                    color_rgba: 0x0000_00ff,
                },
            ],
        };
        assert_eq!(
            nav_map_frame(response),
            NavMapFrame {
                origin_id: 7,
                info: NavMapInfo {
                    root_depth: 6,
                    root_size_mm: 512.0,
                    root_center_x: 64.0,
                    root_center_y: -32.0,
                },
                quads: vec![
                    NavMapQuad {
                        content: 1,
                        depth: 5,
                        rgba: 0x00ff_00ff,
                    },
                    NavMapQuad {
                        content: 7,
                        depth: 0,
                        rgba: 0x0000_00ff,
                    },
                ],
            }
        );
    }

    /// The numbers `NavContent` carries are the generated enum's, pinned here
    /// because this is the only crate that can see both.
    #[test]
    fn every_nav_content_is_the_generated_content_type() {
        for content in NavContent::ALL {
            let generated = pb::NavNodeContentType::try_from(content.wire())
                .unwrap_or_else(|_| panic!("{content:?} is not a generated content type"));
            assert_eq!(generated as i32, content.wire());
        }
        assert!(pb::NavNodeContentType::try_from(NavContent::ALL.len() as i32).is_err());
    }

    #[test]
    fn a_nav_map_response_without_info_is_an_empty_root() {
        let frame = nav_map_frame(pb::NavMapFeedResponse::default());
        assert_eq!(frame.info.root_depth, 0);
        assert_eq!(frame.info.root_size_mm, 0.0);
        assert!(frame.quads.is_empty());
    }

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
