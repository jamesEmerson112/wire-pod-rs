//! Core domain state for the wire-pod SDK app: robot identity, the injectable
//! timing constants, Go-compatible number formatting, the robot seam, the
//! per-robot stream ownership state machines, the stim receive loop and the
//! never-pruned camera meters.
//!
//! The seam in [`robot::conn`] is expressed in domain types, so this crate
//! depends on neither `wirepod-proto` nor tonic. `wirepod-vector` implements it
//! over the generated gRPC client and `wirepod-server` consumes it, which keeps
//! core at the bottom of the dependency graph.
//!
//! The ownership state machines guard their state with [`std::sync::Mutex`] and
//! never hold a guard across an `.await`, which is what lets them be driven
//! from plain `#[test]` functions with no runtime. The crate-level `deny` below
//! makes that a compile error rather than a review comment.
//!
//! The rest of the crate's eventual responsibility (config, path resolution,
//! the logger ring, the jdocs, bot-info and session-cert stores, the registry
//! and the pinger) arrives in later commits.
#![deny(clippy::await_holding_lock)]

pub mod esn;
pub mod gofmt;
pub mod robot;
#[cfg(feature = "test-util")]
pub mod test_support;
pub mod timings;

pub use crate::esn::{Esn, Generation};
pub use crate::gofmt::{GoJsonError, go_format_f32, go_json_f64};
pub use crate::robot::{
    BatteryLevel, BatteryReading, CamMeter, CamMeters, CamOwner, CameraControl, CameraFrame,
    ConnError, ConnTarget, EVENT_CONNECTION_ID, EVENT_WHITELIST, EventItem, EventLoopExit,
    EventOwner, EventReceiver, FrameOutcome, FrameSink, FrameStream, ProtocolResult,
    ProtocolVerdict, RobotConn, RobotConnFactory, StatusCode, StimEvent, StimSample,
    run_event_stream,
};
pub use crate::timings::Timings;
