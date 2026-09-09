//! The robot seam, the per-robot stream ownership and the camera throughput
//! meters.

pub mod conn;
pub mod events;
pub mod meter;
pub mod session;

pub use crate::robot::conn::{
    BatteryLevel, BatteryReading, CameraControl, CameraFrame, ConnError, ConnTarget, EventItem,
    EventReceiver, FrameOutcome, FrameSink, FrameStream, ProtocolResult, ProtocolVerdict,
    RobotConn, RobotConnFactory, StatusCode, StimEvent,
};
pub use crate::robot::events::{
    EVENT_CONNECTION_ID, EVENT_WHITELIST, EventLoopExit, run_event_stream,
};
pub use crate::robot::meter::{CamMeter, CamMeters};
pub use crate::robot::session::{CamOwner, EventOwner, StimSample};
