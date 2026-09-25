//! The per-robot registry: the connection cache Go keeps in `robots`.
//!
//! Go holds a package-level slice addressed by position, guarded by one mutex
//! that most of the mutable fields are written without, and serialises every
//! creation and removal behind a single `inhibitCreation` flag
//! (`robot.go:20-22`, `robot.go:405-419`). This is the same cache keyed by
//! [`Esn`], with the global flag replaced by a per-serial connect lock held
//! across both the dial and the removal, so that one robot's slow dial or slow
//! teardown cannot stall another robot's request. That is decision D3, recorded
//! as deviation 8.
//!
//! The directory is a [`std::sync::RwLock`], the connect locks are
//! [`tokio::sync::Mutex`] values because they are genuinely held across the
//! dial, and the map holding them is read only to clone an `Arc` out before
//! anything is awaited. No `std` guard crosses an `.await`, which the crate's
//! `deny(clippy::await_holding_lock)` enforces.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use crate::clock::{Clock, SystemClock};
use crate::esn::Esn;
use crate::robot::conn::{ConnError, ConnTarget, RobotConn, RobotConnFactory};
use crate::robot::meter::{CamMeter, CamMeters};
use crate::robot::session::SdkSession;
use crate::robot::state_stream::spawn_state_stream;
use crate::store::bot_info::BotInfo;
use crate::timings::Timings;

/// Why [`RobotRegistry::get_or_connect`] could not hand back a robot.
///
/// The `Display` text is a contract. Go writes `"error: " + err.Error()` into
/// the response body for every `/api-sdk/*` path except `get_sdk_info` and
/// `debug` (`server.go:60-66`, the write itself at `server.go:62`), so a caller
/// prefixes this text rather than building its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GetRobotError {
    /// No entry in the bot-info file carries this serial.
    NotFound,
    /// The dial or the liveness check failed.
    Conn(ConnError),
}

impl fmt::Display for GetRobotError {
    /// The `NotFound` text already starts with `error: `, because Go's own
    /// message does (`robot.go:349`) and the handler prefixes it again
    /// (`server.go:62`), which is what produces the doubled
    /// `error: error: robot not found in SDK info file` body the web UI shows.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("error: robot not found in SDK info file"),
            Self::Conn(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for GetRobotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotFound => None,
            Self::Conn(err) => Some(err),
        }
    }
}

/// One connected robot.
///
/// This is Go's `Robot` struct (`robot.go:310-322`) minus everything that went
/// stale in the copy `getRobot` returned. The mutable per-robot state lives on
/// [`RobotEntry::session`] behind its own locks, so an entry handed out by the
/// registry keeps reading live values rather than a snapshot.
pub struct RobotEntry {
    /// The normalized serial, which is also this entry's key.
    pub esn: Esn,
    /// Where the robot lives and how to authenticate to it.
    pub target: ConnTarget,
    /// The live connection.
    pub conn: Arc<dyn RobotConn>,
    /// Stream ownership, the camera operation lock and the idle clock.
    pub session: Arc<SdkSession>,
}

impl fmt::Debug for RobotEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RobotEntry")
            .field("esn", &self.esn)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl RobotEntry {
    /// Resets the idle timer to `now`.
    pub fn touch(&self, now: Duration) {
        self.session.touch(now);
    }

    /// The clock reading of the last touch.
    pub fn last_touch(&self) -> Duration {
        self.session.last_touch()
    }

    /// How long this robot has gone untouched as of `now`.
    pub fn idle_for(&self, now: Duration) -> Duration {
        self.session.idle_for(now)
    }
}

type Entries = HashMap<Esn, Arc<RobotEntry>>;
type ConnectLocks = HashMap<Esn, Arc<AsyncMutex<()>>>;

/// Every connected robot, keyed by serial.
///
/// Entries live until `/api-sdk/disconnect` removes one or the idle rule drops
/// it, exactly as Go's cache is emptied only by `removeRobot`
/// (`robot.go:455-480`). The camera meters deliberately sit beside the
/// directory rather than inside an entry, so their totals survive an eviction
/// and a later reconnect the way Go's `camMeters` survives `removeRobot`.
pub struct RobotRegistry {
    entries: RwLock<Entries>,
    connect_locks: Mutex<ConnectLocks>,
    meters: CamMeters,
    factory: Arc<dyn RobotConnFactory>,
    timings: Timings,
    clock: Arc<dyn Clock>,
    liveness_deadline: Option<Duration>,
    state_stream: bool,
}

impl fmt::Debug for RobotRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RobotRegistry")
            .field("len", &self.len())
            .field("timings", &self.timings)
            .field("liveness_deadline", &self.liveness_deadline)
            .field("state_stream", &self.state_stream)
            .finish_non_exhaustive()
    }
}

impl RobotRegistry {
    /// An empty registry dialling through `factory`, with the default timings,
    /// a [`SystemClock`] and no liveness deadline.
    pub fn new(factory: Arc<dyn RobotConnFactory>) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            connect_locks: Mutex::new(HashMap::new()),
            meters: CamMeters::new(),
            factory,
            timings: Timings::default(),
            clock: Arc::new(SystemClock::new()),
            liveness_deadline: None,
            state_stream: false,
        }
    }

    /// The same, waiting on `timings` instead of the defaults.
    pub fn with_timings(mut self, timings: Timings) -> Self {
        self.timings = timings;
        self
    }

    /// The same, measuring idle time against `clock`.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The same, bounding the connect-time liveness call.
    ///
    /// `None` is the default and reproduces Go, whose `BatteryState` liveness
    /// check runs on a fresh `context.Background()` with no deadline at all
    /// (`robot.go:365`), so a robot that is powered off but whose IP still
    /// routes hangs the dial indefinitely. It is a field rather than a
    /// hardcoded absence so that a test can bound it and P1 can set it in one
    /// line.
    pub fn with_liveness_deadline(mut self, deadline: Option<Duration>) -> Self {
        self.liveness_deadline = deadline;
        self
    }

    /// The same, opening the connect-time `robot_state` stream for every new
    /// connection.
    ///
    /// Off by default, so a registry built for a test opens no stream it did
    /// not ask for. The server turns it on.
    pub fn with_state_stream(mut self, on: bool) -> Self {
        self.state_stream = on;
        self
    }

    /// The durations this registry waits on.
    pub fn timings(&self) -> &Timings {
        &self.timings
    }

    /// The clock the idle rule and [`RobotRegistry::touch`] read.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// The bound on the connect-time liveness call, if any.
    pub fn liveness_deadline(&self) -> Option<Duration> {
        self.liveness_deadline
    }

    /// The cached robot, dialling it first if it is not connected yet.
    ///
    /// This is Go's `getRobot` folded together with `newRobot`
    /// (`robot.go:405-419`, `robot.go:324-403`). The target comes from
    /// `bot_info` by [`BotInfo::resolve`], which is case-insensitive,
    /// last-match-wins and falls back to the global GUID; no match is
    /// [`GetRobotError::NotFound`] and dials nothing.
    ///
    /// Go serialises every creation behind one process-wide flag that
    /// `getRobot` spins on, so one slow dial stalls every request for every
    /// robot (`robot.go:407-412`). This takes a lock keyed by serial instead:
    /// the same serial still dials exactly once, while robot A's dial never
    /// blocks robot B. The map of locks is read only to clone an `Arc` out, and
    /// is released before the dial lock is taken, so it is never held across an
    /// await. The directory is re-read after the lock is acquired, which is
    /// what makes the second caller reuse the first caller's connection rather
    /// than dial again.
    ///
    /// A failed dial or a failed liveness check inserts nothing, so the next
    /// request retries rather than caching a dead connection.
    pub async fn get_or_connect(
        &self,
        esn: &Esn,
        bot_info: &BotInfo,
    ) -> Result<Arc<RobotEntry>, GetRobotError> {
        if let Some(entry) = self.peek(esn) {
            return Ok(entry);
        }
        let target = bot_info.resolve(esn).ok_or(GetRobotError::NotFound)?;

        let connect_lock = self.connect_lock(esn);
        let _dialling = connect_lock.lock().await;
        if let Some(entry) = self.peek(esn) {
            return Ok(entry);
        }

        let conn = self
            .factory
            .connect(&target)
            .await
            .map_err(GetRobotError::Conn)?;
        self.check_liveness(conn.as_ref())
            .await
            .map_err(GetRobotError::Conn)?;

        let session = Arc::new(SdkSession::new(esn.clone()));
        session.touch(self.clock.now());
        let entry = Arc::new(RobotEntry {
            esn: esn.clone(),
            target,
            conn,
            session,
        });
        self.entries_mut().insert(esn.clone(), Arc::clone(&entry));
        // Go opens this stream inline and fails the connect when it cannot;
        // here it opens in a task of its own, so a robot that refuses it is
        // still connected and the refusal is a log line.
        if self.state_stream {
            spawn_state_stream(&entry, self.timings.motion_window);
        }
        Ok(entry)
    }

    /// The cached robot, or `None`. Never dials.
    pub fn peek(&self, esn: &Esn) -> Option<Arc<RobotEntry>> {
        self.entries().get(esn).map(Arc::clone)
    }

    /// Resets the robot idle timer, and reports whether it was connected.
    ///
    /// Go resets the timer in the `/api-sdk/*` preamble, on every path except
    /// `get_sdk_info` and `debug` (`server.go:57-65`). `/cam-stream` runs its
    /// own preamble and discards the index (`server.go:710`), so a page showing
    /// only the camera is dropped after 300 seconds even while frames are
    /// flowing; that asymmetry is deliberate and is reproduced by simply not
    /// calling this from the camera route.
    pub fn touch(&self, esn: &Esn) -> bool {
        match self.peek(esn) {
            Some(entry) => {
                entry.touch(self.clock.now());
                true
            }
            None => false,
        }
    }

    /// Drops the robot, and reports whether it was connected.
    ///
    /// This is Go's `removeRobot` (`robot.go:455-480`). Go holds the global
    /// `inhibitCreation` flag across the whole removal, setting it at
    /// `robot.go:456` and clearing it at `robot.go:479`, so every `/api-sdk/*`
    /// lookup for every serial stalls for the three second settle and then
    /// finds the robot gone and dials afresh. This takes the same per-serial
    /// connect lock [`RobotRegistry::get_or_connect`] takes, which is the
    /// removal half of deviation 8: a request for this serial serialises behind
    /// the removal, and a request for any other serial is not stalled at all.
    ///
    /// The entry leaves the directory before the settle rather than after it,
    /// so the serialised caller resumes into exactly the state Go's caller
    /// resumes into. A handler that had already taken the entry keeps its `Arc`
    /// and its connection for as long as it holds them, which is what a Go
    /// handler holding the `Robot` copy `getRobot` returned also does.
    ///
    /// The order inside is Go's: stop the camera, stop the stim stream, then
    /// sleep the settle. Both stops cancel as well as clearing their flag,
    /// because a handler parked in a receive on a robot that sends nothing
    /// cannot see a flag at all. The camera stop deliberately leaves the
    /// ownership entry in place, as `stopCamStream` does (`robot.go:136-144`).
    ///
    /// Go leaves the camera itself on. Its departing handler is a goroutine
    /// that runs to completion, so the deferred `finishCamStream` issues the
    /// disable eventually; but if the handler is already gone, nothing does,
    /// and the robot streams to nobody until the next claim. After the settle
    /// this issues one best-effort disable, under the camera operation lock and
    /// only when the owner it stopped still holds the feed. That lock and that
    /// generation check are what make it the analogue of `finishCamStream`
    /// (`server.go:700-707`): a `/cam-stream` request that claims the feed
    /// during the settle keeps its camera, where an unconditional disable would
    /// switch it off underneath a live owner. The call is bounded by
    /// `timings.enable` and its result is discarded, exactly as Go discards the
    /// result of its own (`server.go:673-678`). Recorded as deviation 16.
    ///
    /// What survives is the camera meter, whose totals a reconnect resumes
    /// from, and the connect lock, which is never removed for the same reason
    /// Go never removes a camera operation lock: freeing one another caller
    /// already holds a pointer to would need a refcount, and the key set is
    /// bounded by the serials this process has seen (`robot.go:57-61`).
    pub async fn disconnect(&self, esn: &Esn) -> bool {
        let connect_lock = self.connect_lock(esn);
        let _removing = connect_lock.lock().await;
        let Some(entry) = self.peek(esn) else {
            return false;
        };
        self.entries_mut().remove(esn);

        entry.session.cam.stop();
        if let Some(cancel) = entry.session.events.stop() {
            cancel.cancel();
        }
        // The two observation streams hold the entry's connection open for as
        // long as they run, so an eviction that left them running would keep
        // the old connection alive while the next request dialled a new one.
        for cancel in [
            entry.session.state_stream.stop(),
            entry.session.map_feed.stop(),
        ]
        .into_iter()
        .flatten()
        {
            cancel.cancel();
        }
        let stopped = entry.session.cam.current();
        tokio::time::sleep(self.timings.disconnect_settle).await;

        let _op = entry.session.lock_cam_op().await;
        if stopped.is_some() && entry.session.cam.current() == stopped {
            let _ = tokio::time::timeout(
                self.timings.enable,
                entry.conn.enable_image_streaming(false),
            )
            .await;
        }
        true
    }

    /// Every robot that has gone untouched for longer than `timings.idle`.
    ///
    /// Pure, so the rule is testable without the sweeper task that will drive
    /// it. The comparison is `>` rather than `>=`, because Go tests its counter
    /// before it increments it. `connTimer` zeroes `ConnTimer` and then loops
    /// on "sleep one second, test `ConnTimer >= 300`, increment"
    /// (`robot.go:429-452`), so the test reads 0 one second after the reset and
    /// first reads 300 three hundred and one seconds after it. A robot idle for
    /// exactly 300 seconds is therefore still connected under Go, and is not a
    /// candidate here either. Go's own boundary drifts later still, because
    /// `time.Sleep(time.Second)` sleeps at least a second; the port does not
    /// reproduce that drift.
    ///
    /// The result is sorted, so a caller evicts in a stable order rather than
    /// in hash order.
    pub fn idle_candidates(&self, now: Duration) -> Vec<Esn> {
        let mut candidates: Vec<Esn> = self
            .entries()
            .values()
            .filter(|entry| self.is_idle(entry, now))
            .map(|entry| entry.esn.clone())
            .collect();
        candidates.sort();
        candidates
    }

    /// Disconnects every [`RobotRegistry::idle_candidates`] entry, and reports
    /// which ones were dropped.
    ///
    /// The rule is re-checked entry by entry rather than trusted from the
    /// snapshot, because every disconnect pays `timings.disconnect_settle` and
    /// the last of several candidates is therefore reached seconds after the
    /// decision was taken. Go runs one `connTimer` goroutine per robot
    /// (`robot.go:423-452`), so a robot touched while another one is being
    /// removed survives; re-checking is what reproduces that. Checking against
    /// the same `now` still works, because a touch stamps the entry with a
    /// clock reading at or past `now` and `idle_for` saturates to zero.
    pub async fn evict_idle(&self, now: Duration) -> Vec<Esn> {
        let candidates = self.idle_candidates(now);
        let mut evicted = Vec::with_capacity(candidates.len());
        for esn in candidates {
            let still_idle = self
                .peek(&esn)
                .is_some_and(|entry| self.is_idle(&entry, now));
            if still_idle && self.disconnect(&esn).await {
                evicted.push(esn);
            }
        }
        evicted
    }

    /// Go's `connTimer` (`robot.go:423-452`): one sweeper for every robot
    /// rather than one goroutine each, because the registry is keyed by serial
    /// and no index can go stale.
    pub async fn run_conn_timer(&self, cancel: CancellationToken) {
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(self.timings.idle_tick) => {}
            }
            for esn in self.idle_candidates(self.clock.now()) {
                tracing::debug!(target: "sdkapp", bot = %esn, "closing SDK connection, source: connTimer");
                self.disconnect(&esn).await;
            }
        }
    }

    /// The robot camera totals, without creating a meter.
    ///
    /// An ESN nobody has streamed reads `(0, 0)`, which is what `net_probe`
    /// needs for a robot whose camera has never been opened. Go reaches the
    /// same value through `getCamMeter` and so inserts an entry on a read
    /// (`robot.go:182-185`); this deliberately does not, which is deviation 7,
    /// and [`RobotRegistry::meters_len`] is how a test sees the difference.
    pub fn read_meter(&self, esn: &Esn) -> (u64, u64) {
        self.meters.read(esn)
    }

    /// How many robots have a camera meter.
    ///
    /// Meters outlive their entries, so this is not `len` by another name: it
    /// counts the robots whose camera has ever been opened in this process,
    /// which is the only way to observe that [`RobotRegistry::read_meter`]
    /// creates nothing.
    pub fn meters_len(&self) -> usize {
        self.meters.len()
    }

    /// The robot camera meter, created on first use.
    ///
    /// This is the frame pump's handle: it resolves the meter once before it
    /// starts receiving and then counts without touching any map again.
    pub fn meter(&self, esn: &Esn) -> Arc<CamMeter> {
        self.meters.get(esn)
    }

    /// How many robots are connected.
    pub fn len(&self) -> usize {
        self.entries().len()
    }

    /// Whether no robot is connected.
    pub fn is_empty(&self) -> bool {
        self.entries().is_empty()
    }

    /// The connect-time liveness call, which Go makes inline in `newRobot`
    /// (`robot.go:365`) and whose failure is what turns a powered-off robot
    /// into an `error:` body rather than a cached dead connection.
    async fn check_liveness(&self, conn: &dyn RobotConn) -> Result<(), ConnError> {
        let Some(deadline) = self.liveness_deadline else {
            return conn.battery_state().await.map(drop);
        };
        match tokio::time::timeout(deadline, conn.battery_state()).await {
            Ok(result) => result.map(drop),
            Err(_) => Err(ConnError::deadline_exceeded()),
        }
    }

    /// Whether this entry has gone untouched for longer than `timings.idle`.
    ///
    /// One place, so [`RobotRegistry::idle_candidates`] and the re-check inside
    /// [`RobotRegistry::evict_idle`] cannot drift apart at the boundary.
    fn is_idle(&self, entry: &RobotEntry, now: Duration) -> bool {
        entry.idle_for(now) > self.timings.idle
    }

    /// This serial's dial lock, created on demand and cloned out so the map
    /// lock is gone before the caller awaits the one it just took.
    ///
    /// [`RobotRegistry::disconnect`] takes the same lock, which is what stops a
    /// concurrent request for the serial being removed from being handed the
    /// entry that is being torn down.
    fn connect_lock(&self, esn: &Esn) -> Arc<AsyncMutex<()>> {
        let mut locks = self
            .connect_locks
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Arc::clone(locks.entry(esn.clone()).or_default())
    }

    fn entries(&self) -> RwLockReadGuard<'_, Entries> {
        // Poisoning is ignored, for the reason given on `CamOwner::lock`: Go's
        // mutex has no such concept, and a panic elsewhere must not turn every
        // later request into a panic of its own.
        self.entries.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn entries_mut(&self) -> RwLockWriteGuard<'_, Entries> {
        self.entries.write().unwrap_or_else(PoisonError::into_inner)
    }
}
