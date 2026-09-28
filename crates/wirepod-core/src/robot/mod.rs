//! The robot seam, the per-robot stream ownership, the camera throughput
//! meters, the connection registry and the battery energy estimate.

pub mod cam;
pub mod conn;
pub mod energy;
pub mod events;
pub mod meter;
pub mod navmap;
pub mod navmap_feed;
pub mod observe;
pub mod registry;
pub mod robotstate;
pub mod session;
pub mod state_stream;

pub use crate::robot::cam::{CamGuard, PumpExit, cam_stream_pump, start_cam_stream};
pub use crate::robot::conn::{
    BatteryLevel, BatteryReading, CameraControl, CameraFrame, ConnError, ConnTarget, EventItem,
    EventReceiver, FrameOutcome, FrameSink, FrameStream, JdocKind, NamedJdoc, ProtocolResult,
    ProtocolVerdict, RobotConn, RobotConnFactory, StatusCode, StimEvent,
};
pub use crate::robot::energy::{
    BatteryObservation, DEFAULT_CHARGE_SECS, DEFAULT_RUNTIME_SECS, EnergyEvent, EnergySnapshot,
    EnergyStore, LOW_LINE_VOLTS, battery_percent, energy_from_volts,
};
pub use crate::robot::events::{
    EVENT_CONNECTION_ID, EVENT_WHITELIST, EventLoopExit, run_event_stream,
};
pub use crate::robot::meter::{CamMeter, CamMeters};
pub use crate::robot::registry::{GetRobotError, RobotEntry, RobotRegistry};
pub use crate::robot::robotstate::{RobotStateSample, StateChange, StateTracker};
pub use crate::robot::session::{CamOwner, EventOwner, SdkSession, StimSample};
