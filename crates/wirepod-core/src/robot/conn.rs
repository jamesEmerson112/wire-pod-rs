//! The robot seam: what the SDK app needs a connected robot to do, expressed in
//! domain types.
//!
//! Nothing here mentions tonic, prost or `wirepod-proto`. That is deliberate.
//! `wirepod-vector` implements these traits over the generated gRPC client and
//! `wirepod-server` consumes them, so core sits at the bottom of the dependency
//! graph and can hold `AppState` and the registry without a cycle. Go reaches
//! the same place with its one-method `eventReceiver` interface
//! (`server.go:637`) and its `enableImageStreaming` function variable
//! (`server.go:670`), both of which exist purely so a test can stand in for the
//! robot.
//!
//! Every trait uses [`async_trait`] rather than a native `async fn`, because a
//! native one is not dyn-compatible and the registry stores `Arc<dyn RobotConn>`.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use crate::esn::Esn;

/// A gRPC status code, in grpc-go's spelling.
///
/// The names are what `codes.Code.String()` prints, because they reach the web
/// UI through [`ConnError`]'s `Display` and the dashboard shows that text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StatusCode {
    /// Not an error.
    Ok,
    /// The operation was cancelled, typically by the caller.
    Canceled,
    /// An error whose code the server did not set.
    Unknown,
    /// The caller sent an argument the server rejected.
    InvalidArgument,
    /// The deadline passed before the operation finished.
    DeadlineExceeded,
    /// The requested entity was not found.
    NotFound,
    /// The entity the caller tried to create already exists.
    AlreadyExists,
    /// The caller is authenticated but not authorized.
    PermissionDenied,
    /// A quota or a per-server resource is exhausted.
    ResourceExhausted,
    /// The system is not in the state the operation requires.
    FailedPrecondition,
    /// The operation was aborted, typically by a concurrency conflict.
    Aborted,
    /// The operation was attempted past the valid range.
    OutOfRange,
    /// The operation is not implemented or not supported.
    Unimplemented,
    /// An internal invariant was broken.
    Internal,
    /// The service is unavailable, which is what a dropped robot looks like.
    Unavailable,
    /// Unrecoverable data loss or corruption.
    DataLoss,
    /// The caller did not present valid credentials.
    Unauthenticated,
}

impl StatusCode {
    /// The name grpc-go prints for this code.
    pub const fn go_name(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Canceled => "Canceled",
            Self::Unknown => "Unknown",
            Self::InvalidArgument => "InvalidArgument",
            Self::DeadlineExceeded => "DeadlineExceeded",
            Self::NotFound => "NotFound",
            Self::AlreadyExists => "AlreadyExists",
            Self::PermissionDenied => "PermissionDenied",
            Self::ResourceExhausted => "ResourceExhausted",
            Self::FailedPrecondition => "FailedPrecondition",
            Self::Aborted => "Aborted",
            Self::OutOfRange => "OutOfRange",
            Self::Unimplemented => "Unimplemented",
            Self::Internal => "Internal",
            Self::Unavailable => "Unavailable",
            Self::DataLoss => "DataLoss",
            Self::Unauthenticated => "Unauthenticated",
        }
    }

    /// The numeric code, which is what the wire carries.
    pub const fn as_wire(self) -> i32 {
        match self {
            Self::Ok => 0,
            Self::Canceled => 1,
            Self::Unknown => 2,
            Self::InvalidArgument => 3,
            Self::DeadlineExceeded => 4,
            Self::NotFound => 5,
            Self::AlreadyExists => 6,
            Self::PermissionDenied => 7,
            Self::ResourceExhausted => 8,
            Self::FailedPrecondition => 9,
            Self::Aborted => 10,
            Self::OutOfRange => 11,
            Self::Unimplemented => 12,
            Self::Internal => 13,
            Self::Unavailable => 14,
            Self::DataLoss => 15,
            Self::Unauthenticated => 16,
        }
    }

    /// The code a numeric wire value names. Anything outside the 17 defined
    /// codes becomes [`StatusCode::Unknown`], which is what grpc-go does too.
    pub const fn from_wire(code: i32) -> Self {
        match code {
            0 => Self::Ok,
            1 => Self::Canceled,
            3 => Self::InvalidArgument,
            4 => Self::DeadlineExceeded,
            5 => Self::NotFound,
            6 => Self::AlreadyExists,
            7 => Self::PermissionDenied,
            8 => Self::ResourceExhausted,
            9 => Self::FailedPrecondition,
            10 => Self::Aborted,
            11 => Self::OutOfRange,
            12 => Self::Unimplemented,
            13 => Self::Internal,
            14 => Self::Unavailable,
            15 => Self::DataLoss,
            16 => Self::Unauthenticated,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for StatusCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.go_name())
    }
}

/// A failed robot call.
///
/// The `Display` text is a contract, not a convenience. Go writes
/// `"error: " + err.Error()` into the response body on nearly every `/api-sdk/*`
/// failure path, and for a gRPC failure `err.Error()` is grpc-go's
/// `rpc error: code = <Code> desc = <message>`. `tonic::Status`'s own
/// `to_string` produces a different shape, which would look wrong in the
/// dashboard's connectivity panel, so the conversion in `wirepod-vector` lands
/// here rather than being formatted at the edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnError {
    /// The status code, whose name is printed verbatim.
    pub code: StatusCode,
    /// The status message, printed verbatim after `desc = `.
    pub desc: String,
}

impl ConnError {
    /// A failure with the given code and message.
    pub fn new(code: StatusCode, desc: impl Into<String>) -> Self {
        Self {
            code,
            desc: desc.into(),
        }
    }

    /// The failure a call that ran out of time produces.
    ///
    /// grpc-go turns an expired context into `DeadlineExceeded` with the
    /// message Go's `context` package supplies, so the whole rendering is
    /// `rpc error: code = DeadlineExceeded desc = context deadline exceeded`.
    /// It is by far the most common failure the dashboard shows, so it is
    /// byte-exact rather than approximated.
    pub fn deadline_exceeded() -> Self {
        Self::new(StatusCode::DeadlineExceeded, "context deadline exceeded")
    }
}

impl fmt::Display for ConnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rpc error: code = {} desc = {}", self.code, self.desc)
    }
}

impl std::error::Error for ConnError {}

/// Where a robot lives and how to authenticate to it.
///
/// Go builds `Target` as `robot.IPAddress + ":443"` and passes the GUID as the
/// bearer token (`robot.go:333-346`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnTarget {
    /// The normalized serial.
    pub esn: Esn,
    /// The robot's address, without a port.
    pub ip: String,
    /// The robot's GUID, which is the bearer token.
    pub guid: String,
}

impl ConnTarget {
    /// The `host:port` Go stores as `Robot.Target` and reports through
    /// `net_probe`.
    pub fn grpc_target(&self) -> String {
        format!("{}:443", self.ip)
    }
}

/// How full the robot says its battery is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BatteryLevel {
    /// The robot did not say.
    #[default]
    Unknown,
    /// Low enough that the robot wants its charger.
    Low,
    /// A normal charge.
    Nominal,
    /// Fully charged.
    Full,
}

impl BatteryLevel {
    /// The level a `BatteryLevel` enum value names.
    pub const fn from_wire(level: i32) -> Self {
        match level {
            1 => Self::Low,
            2 => Self::Nominal,
            3 => Self::Full,
            _ => Self::Unknown,
        }
    }

    /// The numeric enum value.
    pub const fn as_wire(self) -> i32 {
        match self {
            Self::Unknown => 0,
            Self::Low => 1,
            Self::Nominal => 2,
            Self::Full => 3,
        }
    }
}

/// What a `BatteryState` call answers.
///
/// Only the two fields the slice reads are carried. The full response also has
/// a charging flag, a charger-platform flag, a suggested charge time and the
/// cube's own battery, which `/api-sdk/get_battery` marshals wholesale; that
/// route is deferred, and the type grows when it lands.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BatteryReading {
    /// The coarse level.
    pub level: BatteryLevel,
    /// The measured voltage.
    pub volts: f32,
}

/// Whether the robot accepts the SDK protocol version offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolResult {
    /// The robot will not talk this version.
    Unsupported,
    /// The robot accepted.
    Success,
}

/// What a `ProtocolVersion` call answers.
///
/// `net_probe` times this call and never reads the verdict (`server.go:92-102`),
/// so the fields exist for completeness rather than because a handler branches
/// on them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtocolVerdict {
    /// The robot's answer.
    pub result: ProtocolResult,
    /// The protocol version the robot itself speaks.
    pub host_version: i64,
}

/// One stimulation event as it arrives from the robot.
///
/// Both fields matter. The value is what the graph plots, and the velocity is
/// what decides whether the reading counts at all: Go tests for presence with
/// `strings.Contains(fmt.Sprint(stimInfo), "velocity")` (`server.go:656-659`),
/// and proto3 omits zero-valued scalars from the text form, so a zero velocity
/// is silently skipped.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StimEvent {
    /// The stimulation level.
    pub value: f32,
    /// The rate of change, and the presence flag.
    pub velocity: f32,
}

/// One item off the robot's event stream.
///
/// The whitelist asks for stimulation events only, but the robot is free to
/// send anything, and Go handles that by reading a nil `StimulationInfo` whose
/// text rendering contains no `"velocity"`. [`EventItem::Other`] is that case.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EventItem {
    /// A stimulation event.
    Stim(StimEvent),
    /// Anything else, which the stim loop ignores.
    Other,
}

/// One JPEG frame off the robot's camera feed.
///
/// The bytes are what the robot sent. Go counts them before decoding, because
/// a frame that fails to decode still crossed the wire (`server.go:770-776`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CameraFrame {
    /// The encoded frame.
    pub data: Vec<u8>,
}

/// Whether a frame reached the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameOutcome {
    /// The frame was written.
    Sent,
    /// The frame could not be turned into a part and nothing was written, but
    /// the feed is healthy and the pump carries on. This is Go's `continue` on
    /// an undecodable image (`server.go:777-782`), which is a crash fix: a
    /// truncated frame used to reach the encoder as a nil image and panic the
    /// process. The frame is still counted by the meter, because it crossed the
    /// wire.
    Skipped,
    /// The client is gone; the pump should stop.
    Closed,
}

/// The reading half of an open event stream.
///
/// `Ok(None)` means the robot closed the stream cleanly. This is the Rust
/// analogue of Go's `eventReceiver` interface (`server.go:637`).
#[async_trait]
pub trait EventReceiver: Send {
    /// Waits for the next event.
    async fn next(&mut self) -> Result<Option<EventItem>, ConnError>;
}

/// The reading half of an open camera feed.
#[async_trait]
pub trait FrameStream: Send {
    /// Waits for the next frame.
    async fn next(&mut self) -> Result<Option<CameraFrame>, ConnError>;
}

/// Where the frame pump writes. The HTTP response body implements this; a test
/// substitutes a recorder.
#[async_trait]
pub trait FrameSink: Send {
    /// Writes one already-encoded frame.
    async fn send(&mut self, jpeg: &[u8]) -> FrameOutcome;
}

/// The robot's camera on/off switch.
///
/// This is the seam Go opens by declaring `enableImageStreaming` as a package
/// variable rather than as a function, so a test can substitute a fake and
/// assert the ordering of the on and off calls (`server.go:670`).
#[async_trait]
pub trait CameraControl: Send + Sync {
    /// Turns image streaming on or off.
    async fn enable_image_streaming(&self, on: bool) -> Result<(), ConnError>;
}

/// A live connection to one robot.
///
/// [`CameraControl`] is a supertrait so that the camera code can take a
/// `&dyn CameraControl` and be driven by a recorder in tests, while a handler
/// holding an `Arc<dyn RobotConn>` still has the switch.
#[async_trait]
pub trait RobotConn: CameraControl + Send + Sync {
    /// The liveness call. Go uses it as the connect-time check
    /// (`robot.go:365`) and `/api-sdk/get_battery` reads it for real.
    async fn battery_state(&self) -> Result<BatteryReading, ConnError>;

    /// The call `net_probe` times. Deliberately not `BatteryState`, which the
    /// robot answers off its engine tick (`server.go:76-92`).
    async fn protocol_version(
        &self,
        client_version: i64,
        min_host_version: i64,
    ) -> Result<ProtocolVerdict, ConnError>;

    /// Opens an event stream filtered to `whitelist` and tagged with
    /// `connection_id`.
    async fn open_event_stream(
        &self,
        whitelist: &[&str],
        connection_id: &str,
    ) -> Result<Box<dyn EventReceiver>, ConnError>;

    /// Opens the camera feed.
    async fn open_camera_feed(&self) -> Result<Box<dyn FrameStream>, ConnError>;
}

/// Dials robots. The registry holds one of these and nothing else knows how a
/// connection is made.
#[async_trait]
pub trait RobotConnFactory: Send + Sync {
    /// Dials `target` and returns a connection that has already answered at
    /// least one call, so a returned connection means a live robot.
    async fn connect(&self, target: &ConnTarget) -> Result<Arc<dyn RobotConn>, ConnError>;
}
