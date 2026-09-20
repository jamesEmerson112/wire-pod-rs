//! The injectable timing constants the SDK-app slice waits on.

use std::time::Duration;

/// Every duration the camera, probe and connection paths wait on.
///
/// Handlers read these from `AppState` rather than from constants so that tests
/// can drive the state machines with zero settles and millisecond deadlines,
/// and so that the config type arriving in P1 has somewhere to land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timings {
    /// Pause after displacing a live camera owner, so the robot drops the
    /// `CameraFeed` that was just cancelled. A first claim does not pay it.
    pub settle: Duration,
    /// Deadline on the `net_probe` round trip.
    pub probe: Duration,
    /// Deadline on the `EnableImageStreaming` RPC.
    pub enable: Duration,
    /// Pause inside a disconnect, once per matched robot.
    pub disconnect_settle: Duration,
    /// How long a robot may go untouched before the sweeper drops it.
    pub idle: Duration,
    /// One turn of `connTimer`'s loop.
    pub idle_tick: Duration,
    /// `bwPollInterval`: the battery watchdog's poll period.
    pub battery_poll: Duration,
    /// `bwRPCTimeout`.
    pub battery_rpc: Duration,
    /// `bwDockTimeout`: the bound on one go-home attempt.
    pub battery_dock: Duration,
    /// `bwCooldown`: the wait after any attempt.
    pub battery_cooldown: Duration,
    /// `bwGiveUpCooldown`: the wait after `bwMaxAttempts` failed attempts.
    pub battery_give_up_cooldown: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            settle: Duration::from_millis(500),
            probe: Duration::from_secs(5),
            enable: Duration::from_secs(5),
            disconnect_settle: Duration::from_secs(3),
            idle: Duration::from_secs(300),
            idle_tick: Duration::from_secs(1),
            battery_poll: Duration::from_secs(30),
            battery_rpc: Duration::from_secs(5),
            battery_dock: Duration::from_secs(180),
            battery_cooldown: Duration::from_secs(600),
            battery_give_up_cooldown: Duration::from_secs(1800),
        }
    }
}

impl Timings {
    /// Every duration zero, so a test never waits on a real clock.
    pub const fn instant() -> Self {
        Self {
            settle: Duration::ZERO,
            probe: Duration::ZERO,
            enable: Duration::ZERO,
            disconnect_settle: Duration::ZERO,
            idle: Duration::ZERO,
            idle_tick: Duration::ZERO,
            battery_poll: Duration::ZERO,
            battery_rpc: Duration::ZERO,
            battery_dock: Duration::ZERO,
            battery_cooldown: Duration::ZERO,
            battery_give_up_cooldown: Duration::ZERO,
        }
    }
}
