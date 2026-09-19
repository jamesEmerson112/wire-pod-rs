//! Core domain state for the wire-pod SDK app: robot identity, the injectable
//! timing constants, Go-compatible number formatting, the robot seam, the
//! per-robot stream ownership state machines, the stim receive loop, the camera
//! guard and frame pump, the never-pruned camera meters, the bot-info, jdocs,
//! jdocs-pinger, session-certificate and SDK-ini stores, the transient token
//! stores, the token hashing and the token server's JWT, the `apiConfig.json`
//! layer, the logger ring and the `tracing` layer that fills it, the monotonic
//! and calendar clocks and Go's two time layouts, the resolution of Go's two
//! on-disk layouts, the `encoding/json` encoder and decoder every state file
//! goes through, and the atomic replacement every state file is written
//! through.
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
//! Go's roughly thirty unsynchronized globals, and it now carries every store
//! Phase 1 has landed: the resolved paths, the configuration and the one gate
//! its writers share, the jdocs, session-certificate, SDK-ini and transient
//! token stores, the logger ring and the calendar clock, beside the bot-info
//! file, the pinger, the registry, the timings and the monotonic clock the P4
//! slice brought. The server-config store is the one piece of the crate's
//! eventual responsibility still outstanding, and it arrives in a later
//! commit.
#![deny(clippy::await_holding_lock)]

pub mod clock;
pub mod config;
pub mod esn;
pub mod gofmt;
pub mod gojson;
pub mod intents;
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
    ApiConfig, BatteryConfig, BootConfig, BootOutcome, DecodeError, Env, KnowledgeConfig,
    ServerConfig, SttConfig, WeatherConfig, create_config_from_env, read_config,
    write_config_to_disk,
};
pub use crate::esn::{Esn, Generation};
pub use crate::gofmt::{
    GoJsonError, go_format_f32, go_json_f32, go_json_f32_raw, go_json_f64, go_json_f64_raw,
};
pub use crate::gojson::{DecodeFault, Extra, GoFormatter, go_marshal};
pub use crate::robot::{
    BatteryLevel, BatteryReading, CamGuard, CamMeter, CamMeters, CamOwner, CameraControl,
    CameraFrame, ConnError, ConnTarget, EVENT_CONNECTION_ID, EVENT_WHITELIST, EventItem,
    EventLoopExit, EventOwner, EventReceiver, FrameOutcome, FrameSink, FrameStream, GetRobotError,
    JdocKind, NamedJdoc, ProtocolResult, ProtocolVerdict, PumpExit, RobotConn, RobotConnFactory,
    RobotEntry, RobotRegistry, SdkSession, StatusCode, StimEvent, StimSample, cam_stream_pump,
    run_event_stream, start_cam_stream,
};
pub use crate::state::{AppState, AppStateBuilder, Paths, WallLogClock};
pub use crate::store::{
    AddOutcome, BotInfo, BotInfoRobot, BotInfoWire, BotJdoc, BotStatus, BotStatusKind,
    DEFAULT_SECTION, IniEdit, IniError, IniFile, IniKey, IniSection, JDOCS_FILE_MODE, Jdoc,
    JdocsDecodeError, JdocsLoadOutcome, JdocsStore, LINE_BREAK, LoadedJdocs, LoadedSessionCerts,
    PLACEHOLDER_NAME, PingerState, ReadSessionCertsOutcome, RecurringInfo, RecurringInfoLoad,
    RobotWire, SDK_CERT_FILE_MODE, SDK_CONFIG_FILE, SDK_INI_DIR_MODE, SDK_INI_FILE_MODE,
    SESSION_CERT_FILE_MODE, SdkIniStore, SecondaryOutcome, SessionCertStore, cert_file_path,
    cert_value, certificate_der, issuer_common_name, marshal_jdocs, parse_jdocs,
    read_session_certs, sdk_config_path, session_cert_gate, session_cert_read_path,
    write_session_cert,
};
pub use crate::timings::Timings;
pub use crate::token::{
    Claims, ClientToken, ClientTokenManager, GUID_B64_LEN, HASH_SIZE, HASHED_B64_LEN,
    HASHED_RAW_LEN, Hashed, PrimaryEntry, PrimaryWalk, RandomError, Requestor, SALT_SIZE,
    SecondaryEntry, SessionEntry, SessionMatch, TOKEN_SIZE, TokenBundle, TokenHashError, TokenPair,
    TokenStores, compare_hash_and_token, create_token_and_hashed_token, encode_token_and_hash,
    generate_token_id, hash_token, host_of, issue_token, new_from_hash, write_token_hash,
};
