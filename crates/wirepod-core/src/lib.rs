//! Core domain state for the wire-pod SDK app: robot identity, the injectable
//! timing constants, Go-compatible number formatting, the robot seam, the
//! per-robot stream ownership state machines, the stim receive loop, the camera
//! guard and frame pump, the never-pruned camera meters, the bot-info and
//! jdocs-pinger stores, the resolution of Go's two on-disk layouts, and the
//! atomic replacement every state file is written through.
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
//! [`state::AppState`] is the one shared value the handlers read, replacing
//! Go's roughly thirty unsynchronized globals. The rest of the crate's
//! eventual responsibility (config, the logger ring, and the jdocs and
//! session-cert stores) arrives in later commits.
#![deny(clippy::await_holding_lock)]

pub mod clock;
pub mod config;
pub mod esn;
pub mod gofmt;
pub mod logger;
pub mod paths;
pub mod persist;
pub mod robot;
pub mod state;
pub mod store;
#[cfg(feature = "test-util")]
pub mod test_support;
pub mod timefmt;
pub mod timings;
pub mod token;
pub mod wallclock;

pub use crate::clock::{Clock, ManualClock, SystemClock};
pub use crate::config::{
    ApiConfig, BatteryConfig, BootConfig, BootOutcome, Env, Extra, GoFormatter, KnowledgeConfig,
    ServerConfig, SttConfig, WeatherConfig, create_config_from_env, go_marshal, read_config,
    write_config_to_disk,
};
pub use crate::esn::{Esn, Generation};
pub use crate::gofmt::{
    GoJsonError, go_format_f32, go_json_f32, go_json_f32_raw, go_json_f64, go_json_f64_raw,
};
pub use crate::robot::{
    BatteryLevel, BatteryReading, CamGuard, CamMeter, CamMeters, CamOwner, CameraControl,
    CameraFrame, ConnError, ConnTarget, EVENT_CONNECTION_ID, EVENT_WHITELIST, EventItem,
    EventLoopExit, EventOwner, EventReceiver, FrameOutcome, FrameSink, FrameStream, GetRobotError,
    ProtocolResult, ProtocolVerdict, PumpExit, RobotConn, RobotConnFactory, RobotEntry,
    RobotRegistry, SdkSession, StatusCode, StimEvent, StimSample, cam_stream_pump,
    run_event_stream, start_cam_stream,
};
pub use crate::state::{AppState, AppStateBuilder};
pub use crate::store::{
    BotInfo, BotInfoRobot, BotInfoWire, BotStatus, BotStatusKind, PingerState, RobotWire,
};
pub use crate::timings::Timings;
pub use crate::token::{
    GUID_B64_LEN, HASH_SIZE, HASHED_B64_LEN, HASHED_RAW_LEN, Hashed, SALT_SIZE, TOKEN_SIZE,
    TokenHashError, TokenPair, compare_hash_and_token, create_token_and_hashed_token,
    encode_token_and_hash, hash_token, new_from_hash,
};
