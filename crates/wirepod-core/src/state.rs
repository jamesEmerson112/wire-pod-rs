//! The shared server state the SDK-app handlers read.
//!
//! Go keeps this as roughly thirty unsynchronized package-level globals spread
//! across `vars`, `sdkapp` and `config-ws`. This is the same state as one value
//! shared as an `Arc`, holding only what the early slice's handlers need: the
//! bot-info file, the jdocs pinger, the robot registry, the timings and the
//! clock. Everything else the plan lists for `wirepod-core` (config, path
//! resolution, the logger ring, the jdocs and session-cert stores) arrives in
//! P1 and lands here.
//!
//! The bot-info store is a [`std::sync::RwLock`], and the only way to read it
//! across an `.await` is [`AppState::bot_info_snapshot`], which clones. That is
//! deliberate: the crate's `deny(clippy::await_holding_lock)` does not follow a
//! guard handed out to `wirepod-server`, so no guard is handed out.

use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use crate::clock::{Clock, SystemClock};
use crate::esn::Esn;
use crate::robot::conn::RobotConnFactory;
use crate::robot::registry::{GetRobotError, RobotEntry, RobotRegistry};
use crate::store::bot_info::BotInfo;
use crate::store::bot_status::PingerState;
use crate::timings::Timings;

/// Everything a handler needs, shared as an `Arc`.
pub struct AppState {
    bot_info: RwLock<BotInfo>,
    pinger: PingerState,
    registry: RobotRegistry,
    timings: Timings,
    clock: Arc<dyn Clock>,
}

impl AppState {
    /// A builder dialling robots through `factory`.
    pub fn builder(factory: Arc<dyn RobotConnFactory>) -> AppStateBuilder {
        AppStateBuilder::new(factory)
    }

    /// Reads the bot-info file under its lock.
    ///
    /// For the synchronous readers: `/api/get_bot_status` and
    /// `/api-sdk/get_sdk_info` both project it and answer without awaiting
    /// anything.
    pub fn with_bot_info<R>(&self, read: impl FnOnce(&BotInfo) -> R) -> R {
        read(&self.bot_info.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// A copy of the bot-info file, for a caller that has to hold it across an
    /// `.await`.
    ///
    /// The connect preamble is that caller: it resolves a serial against the
    /// file and then dials. The file holds one entry per robot ever
    /// authenticated against this server, so the copy is cheap next to the
    /// dial it precedes.
    pub fn bot_info_snapshot(&self) -> BotInfo {
        self.with_bot_info(Clone::clone)
    }

    /// Replaces the bot-info file, which the jdocs and token servers do as
    /// robots authenticate.
    pub fn set_bot_info(&self, info: BotInfo) {
        *self
            .bot_info
            .write()
            .unwrap_or_else(PoisonError::into_inner) = info;
    }

    /// The cached robot, dialling it first if it is not connected yet.
    ///
    /// This is Go's `getRobot` as the preamble calls it (`server.go:57`),
    /// resolved against the current bot-info file. It lives here rather than
    /// on the handler so that the snapshot-then-dial discipline is written once
    /// inside the crate that denies holding a lock across an await.
    pub async fn get_robot(&self, esn: &Esn) -> Result<Arc<RobotEntry>, GetRobotError> {
        let bot_info = self.bot_info_snapshot();
        self.registry.get_or_connect(esn, &bot_info).await
    }

    /// The jdocs pinger's record of which robots have checked in.
    pub fn pinger(&self) -> &PingerState {
        &self.pinger
    }

    /// Every connected robot.
    pub fn registry(&self) -> &RobotRegistry {
        &self.registry
    }

    /// The durations the handlers wait on.
    pub fn timings(&self) -> &Timings {
        &self.timings
    }

    /// The clock the pinger and the idle rule measure against.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }
}

/// Builds an [`AppState`].
///
/// The connection factory is the one thing with no sensible default, because it
/// decides whether the server talks to a real robot. Everything else defaults:
/// [`Timings::default`], a [`SystemClock`], an empty [`BotInfo`] and no
/// liveness deadline.
pub struct AppStateBuilder {
    factory: Arc<dyn RobotConnFactory>,
    bot_info: BotInfo,
    timings: Timings,
    clock: Option<Arc<dyn Clock>>,
    liveness_deadline: Option<Duration>,
}

impl AppStateBuilder {
    /// A builder dialling robots through `factory`.
    pub fn new(factory: Arc<dyn RobotConnFactory>) -> Self {
        Self {
            factory,
            bot_info: BotInfo::default(),
            timings: Timings::default(),
            clock: None,
            liveness_deadline: None,
        }
    }

    /// Starts from a loaded bot-info file rather than an empty one.
    pub fn bot_info(mut self, bot_info: BotInfo) -> Self {
        self.bot_info = bot_info;
        self
    }

    /// Waits on `timings` rather than the Go defaults.
    pub fn timings(mut self, timings: Timings) -> Self {
        self.timings = timings;
        self
    }

    /// Measures elapsed time against `clock` rather than the system one.
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Bounds the connect-time liveness call. `None`, the default,
    /// reproduces Go's undeadlined `BatteryState` (`robot.go:365`).
    pub fn liveness_deadline(mut self, deadline: Option<Duration>) -> Self {
        self.liveness_deadline = deadline;
        self
    }

    /// The finished state, ready to be handed to the router.
    pub fn build(self) -> Arc<AppState> {
        let clock: Arc<dyn Clock> = self.clock.unwrap_or_else(|| Arc::new(SystemClock::new()));
        let registry = RobotRegistry::new(self.factory)
            .with_timings(self.timings)
            .with_clock(Arc::clone(&clock))
            .with_liveness_deadline(self.liveness_deadline);
        Arc::new(AppState {
            bot_info: RwLock::new(self.bot_info),
            pinger: PingerState::new(),
            registry,
            timings: self.timings,
            clock,
        })
    }
}
