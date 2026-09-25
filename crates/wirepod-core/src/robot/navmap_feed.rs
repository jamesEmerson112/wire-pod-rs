//! The task that holds a robot's `NavMapFeed` open while someone is watching,
//! stores each map in the session's [`MapSlot`](crate::robot::observe::MapSlot)
//! and logs a summary of it.
//!
//! Nothing here logs per map. The log ring is 500 entries shared with the voice
//! pipeline and a robot on the move sends two maps a second, so the feed writes
//! a line when it starts, when it stops, at once when the map's origin changes,
//! and otherwise at most one summary, and one line about malformed maps, per
//! `map_summary_gap`.

use std::fmt;
use std::future::{Future, pending};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::{Instant, Interval, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::clock::Clock;
use crate::esn::{Esn, Generation};
use crate::logger::COMP_SDK;
use crate::robot::conn::{ConnError, NavMapReceiver, RobotConn};
use crate::robot::navmap::{ContentCounts, NavMapFrame, ReconstructError, reconstruct};
use crate::robot::observe::{MapSlot, ReceivedMap};
use crate::timings::Timings;
use crate::wallclock::WallClock;

/// The longest the feed goes between two looks at its lease.
const LEASE_TICK: Duration = Duration::from_secs(1);

/// What the slot records when the robot ends the feed without an error.
pub const STREAM_ENDED: &str = "the robot ended the nav map feed";

/// Why the feed ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MapFeedExit {
    /// Nobody asked for a snapshot within `map_lease`.
    LeaseLapsed,
    /// The token was cancelled, which is what a disconnect does.
    Cancelled,
    /// The robot closed the stream cleanly.
    StreamEnded,
    /// Opening or reading the stream failed.
    Failed(ConnError),
}

impl MapFeedExit {
    /// What the slot records for an end the robot chose.
    fn failure(&self) -> Option<String> {
        match self {
            Self::Failed(err) => Some(err.to_string()),
            Self::StreamEnded => Some(STREAM_ENDED.to_owned()),
            Self::LeaseLapsed | Self::Cancelled => None,
        }
    }
}

impl fmt::Display for MapFeedExit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LeaseLapsed => f.write_str("lease lapsed"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::StreamEnded => f.write_str("stream ended"),
            Self::Failed(err) => write!(f, "{err}"),
        }
    }
}

/// One run of the feed, with everything it needs apart from the stream.
pub struct MapFeed {
    /// The session's slot, which this run has claimed.
    pub slot: Arc<MapSlot>,
    /// The claim.
    pub generation: Generation,
    /// Cancelled by whoever stops the slot.
    pub cancel: CancellationToken,
    /// The robot, for the log lines.
    pub serial: Esn,
    /// The registry's clock, which the lease and the summary gap are read on.
    pub clock: Arc<dyn Clock>,
    /// Stamps each map's `received_ms`.
    pub wall: Arc<dyn WallClock>,
    pub timings: Timings,
    /// The slot's error from the previous feed, read before this one claimed
    /// it.
    ///
    /// The snapshot route restarts a failed feed on every poll, so a robot that
    /// refuses the stream would otherwise write two lines a second. A feed that
    /// replaces a failed one writes its start line only when its first map
    /// arrives, and no stop line when it fails the same way without one.
    pub previous_error: Option<String>,
}

impl MapFeed {
    /// Opens the feed on `conn` and runs it, giving the claim back however it
    /// ends.
    ///
    /// The open is raced against the token and the lease rather than simply
    /// awaited, because the robot's gateway need not answer the call until it
    /// has a map to send, and a robot sitting still has none.
    ///
    /// When the first map arrives it opens a second stream beside the first,
    /// which sets the broadcast period again after any stream the robot had
    /// not yet seen close has reset it, and reads both until the feed ends.
    pub async fn open_and_run(self, conn: &dyn RobotConn) -> MapFeedExit {
        let mut log = MapLog::new(&self.slot);
        if self.previous_error.is_none() {
            self.log_start(&mut log);
        }
        let mut lease = self.lease_ticker();
        let open = conn.open_nav_map_feed(self.timings.map_period);
        tokio::pin!(open);
        let opened = loop {
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => break Err(MapFeedExit::Cancelled),
                _ = lease.tick() => {
                    if self.lease_lapsed() {
                        break Err(MapFeedExit::LeaseLapsed);
                    }
                }
                opened = &mut open => break opened.map_err(MapFeedExit::Failed),
            }
        };
        match opened {
            Ok(receiver) => {
                self.pump(receiver, Second::Unopened(conn), lease, log)
                    .await
            }
            Err(exit) => self.finish(exit, &log),
        }
    }

    /// Runs the feed over a stream that is already open, giving the claim back
    /// however it ends.
    ///
    /// With no connection it cannot open the second stream, so it does not
    /// reassert the broadcast period after its first map, and a stream the
    /// robot has not yet seen close can stop it at that map.
    pub async fn run(self, receiver: Box<dyn NavMapReceiver>) -> MapFeedExit {
        let mut log = MapLog::new(&self.slot);
        if self.previous_error.is_none() {
            self.log_start(&mut log);
        }
        let lease = self.lease_ticker();
        self.pump(receiver, Second::Absent, lease, log).await
    }

    async fn pump(
        self,
        mut first: Box<dyn NavMapReceiver>,
        mut second: Second<'_>,
        mut lease: Interval,
        mut log: MapLog,
    ) -> MapFeedExit {
        let exit = loop {
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => break MapFeedExit::Cancelled,
                _ = lease.tick() => {
                    if self.lease_lapsed() {
                        break MapFeedExit::LeaseLapsed;
                    }
                }
                received = first.next() => match received {
                    Ok(Some(frame)) => {
                        if let Second::Unopened(conn) = second {
                            second = Second::Opening(
                                conn.open_nav_map_feed(self.timings.map_period),
                            );
                        }
                        self.receive(frame, &mut log);
                    }
                    Ok(None) => break MapFeedExit::StreamEnded,
                    Err(err) => break MapFeedExit::Failed(err),
                },
                event = second.next() => match event {
                    SecondEvent::Opened(Ok(receiver)) => {
                        self.log_reopened(None);
                        second = Second::Open(receiver);
                    }
                    SecondEvent::Opened(Err(err)) => {
                        self.log_reopened(Some(&err));
                        second = Second::Absent;
                    }
                    SecondEvent::Received(Ok(Some(frame))) => self.receive(frame, &mut log),
                    SecondEvent::Received(Ok(None)) => break MapFeedExit::StreamEnded,
                    SecondEvent::Received(Err(err)) => break MapFeedExit::Failed(err),
                },
            }
        };
        drop(second);
        drop(first);
        self.finish(exit, &log)
    }

    /// Stores `frame` if its leaves tile its root.
    ///
    /// The robot's gateway reassembles each map from several engine messages
    /// and under load can pass on a truncated one. Dropping it keeps the last
    /// good map in the slot, so one bad map never blanks the page.
    fn receive(&self, frame: NavMapFrame, log: &mut MapLog) {
        if !log.started {
            self.log_start(log);
        }
        let now = self.clock.now();
        let gap = self.timings.map_summary_gap;
        let bot = self.serial.as_str();
        if let Err(err) = reconstruct(&frame) {
            log.dropped(&err, now, gap, bot);
            return;
        }
        log.note(&frame, now, gap, bot);
        let received_ms = unix_millis(self.wall.as_ref());
        self.slot
            .write(self.generation, ReceivedMap { frame, received_ms });
    }

    fn finish(self, exit: MapFeedExit, log: &MapLog) -> MapFeedExit {
        let failure = exit.failure();
        // `fail` needs the claim, so it comes before the release.
        if let Some(failure) = &failure {
            self.slot.fail(self.generation, failure.clone());
        }
        self.slot.release(self.generation);

        let silent = log.maps == 0 && log.dropped == 0;
        if silent && failure.is_some() && failure == self.previous_error {
            return exit;
        }
        let bot = self.serial.as_str();
        let dropped = match log.dropped {
            0 => String::new(),
            dropped => format!(" and {dropped} malformed"),
        };
        match &log.last {
            Some(last) => tracing::debug!(
                target: "sdkapp",
                comp = COMP_SDK,
                bot = bot,
                "nav map feed stopped ({exit}) after {} maps{dropped}; last {last}",
                log.maps,
            ),
            None => tracing::debug!(
                target: "sdkapp",
                comp = COMP_SDK,
                bot = bot,
                "nav map feed stopped ({exit}) before any good map{dropped}",
            ),
        }
        exit
    }

    fn log_start(&self, log: &mut MapLog) {
        log.started = true;
        tracing::debug!(
            target: "sdkapp",
            comp = COMP_SDK,
            bot = self.serial.as_str(),
            "nav map feed started, at most one map per {:?}",
            self.timings.map_period,
        );
    }

    fn log_reopened(&self, failure: Option<&ConnError>) {
        let bot = self.serial.as_str();
        match failure {
            None => tracing::debug!(
                target: "sdkapp",
                comp = COMP_SDK,
                bot = bot,
                "nav map feed: reopened alongside the first stream to reassert the broadcast \
                 period, which a stream the robot had not yet noticed closing resets to -1 on \
                 the next map",
            ),
            Some(err) => tracing::debug!(
                target: "sdkapp",
                comp = COMP_SDK,
                bot = bot,
                "nav map feed: could not reopen alongside the first stream to reassert the \
                 broadcast period, so it carries on with the first alone: {err}",
            ),
        }
    }

    /// Ticks once a second, or at the lease's own length when that is shorter,
    /// and never at zero, which `interval` refuses.
    fn lease_ticker(&self) -> Interval {
        let period = self
            .timings
            .map_lease
            .min(LEASE_TICK)
            .max(Duration::from_millis(1));
        let mut ticker = tokio::time::interval_at(Instant::now() + period, period);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        ticker
    }

    fn lease_lapsed(&self) -> bool {
        self.slot.lapsed(self.clock.now(), self.timings.map_lease)
    }
}

/// An open of the nav map feed that has not answered yet.
type Opening<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn NavMapReceiver>, ConnError>> + Send + 'a>>;

/// The second stream, which the feed opens beside the first when its first map
/// arrives.
///
/// The robot's gateway sets the engine's broadcast period when a `NavMapFeed`
/// opens and sets it to -1 when the call returns, but it waits only on the
/// engine, so it sees that its client has gone only when it next sends a map.
/// The period is one value for every client. A stream dropped earlier, by this
/// server or anyone else, therefore stops the feed for everybody at the next
/// map. Every such stream wakes on the same map, so opening another stream after
/// the first map sets the period again once they have all reset it. The feed
/// never drops a stream while it runs, since that stream would do the same, and
/// it reads both, since the robot stops sending on one that is not read.
enum Second<'a> {
    /// No map yet, and the connection to open the stream on.
    Unopened(&'a dyn RobotConn),
    Opening(Opening<'a>),
    Open(Box<dyn NavMapReceiver>),
    /// `run`, which has no connection, or an open that failed.
    Absent,
}

enum SecondEvent {
    Opened(Result<Box<dyn NavMapReceiver>, ConnError>),
    Received(Result<Option<NavMapFrame>, ConnError>),
}

impl Second<'_> {
    /// Waits for the open to answer or the stream to deliver, and forever when
    /// there is neither.
    async fn next(&mut self) -> SecondEvent {
        match self {
            Self::Opening(open) => SecondEvent::Opened(open.as_mut().await),
            Self::Open(receiver) => SecondEvent::Received(receiver.next().await),
            Self::Unopened(_) | Self::Absent => pending().await,
        }
    }
}

/// What the feed has said about its maps.
#[derive(Debug)]
struct MapLog {
    /// Whether the start line has gone out.
    started: bool,
    /// The origin of the last map, starting from the one the slot kept from an
    /// earlier feed, so a pick-up while nobody watched still reads as a reset.
    origin: Option<u32>,
    /// When the last summary or reset line went out, on the registry's clock.
    summarised_at: Option<Duration>,
    maps: u64,
    last: Option<Summary>,
    /// Malformed maps dropped, and when the last line about one went out.
    dropped: u64,
    dropped_at: Option<Duration>,
}

impl MapLog {
    fn new(slot: &MapSlot) -> Self {
        Self {
            started: false,
            origin: slot.latest().map(|map| map.frame.origin_id),
            summarised_at: None,
            maps: 0,
            last: None,
            dropped: 0,
            dropped_at: None,
        }
    }

    /// Counts a malformed map, and says so at most once per `gap`.
    fn dropped(&mut self, err: &ReconstructError, now: Duration, gap: Duration, bot: &str) {
        self.dropped += 1;
        if self
            .dropped_at
            .is_some_and(|at| now.saturating_sub(at) < gap)
        {
            return;
        }
        self.dropped_at = Some(now);
        tracing::debug!(
            target: "sdkapp",
            comp = COMP_SDK,
            bot = bot,
            "dropped a nav map ({} so far): {err}",
            self.dropped,
        );
    }

    fn note(&mut self, frame: &NavMapFrame, now: Duration, gap: Duration, bot: &str) {
        let summary = Summary::of(frame);
        self.maps += 1;
        self.last = Some(summary);
        let previous = self.origin.replace(frame.origin_id);
        if let Some(previous) = previous.filter(|previous| *previous != frame.origin_id) {
            tracing::debug!(
                target: "sdkapp",
                comp = COMP_SDK,
                bot = bot,
                "map reset: origin {previous} -> {}; {summary}",
                frame.origin_id,
            );
        } else if self
            .summarised_at
            .is_none_or(|at| now.saturating_sub(at) >= gap)
        {
            tracing::debug!(
                target: "sdkapp",
                comp = COMP_SDK,
                bot = bot,
                "nav map {summary}",
            );
        } else {
            return;
        }
        self.summarised_at = Some(now);
    }
}

/// What a log line says about one map.
#[derive(Clone, Copy, Debug)]
struct Summary {
    origin_id: u32,
    root_size_mm: f32,
    root_depth: i32,
    quads: usize,
    counts: ContentCounts,
}

impl Summary {
    fn of(frame: &NavMapFrame) -> Self {
        Self {
            origin_id: frame.origin_id,
            root_size_mm: frame.info.root_size_mm,
            root_depth: frame.info.root_depth,
            quads: frame.quads.len(),
            counts: ContentCounts::of(&frame.quads),
        }
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "origin={} root={}mm depth={} quads={}",
            self.origin_id, self.root_size_mm, self.root_depth, self.quads
        )?;
        for (content, count) in self.counts.iter().filter(|(_, count)| *count > 0) {
            write!(f, " {}={count}", content.name())?;
        }
        if self.counts.unrecognised() > 0 {
            write!(f, " unrecognised={}", self.counts.unrecognised())?;
        }
        Ok(())
    }
}

/// Unix milliseconds now, on `wall`.
fn unix_millis(wall: &dyn WallClock) -> i64 {
    let now = wall.now();
    now.unix_secs
        .saturating_mul(1000)
        .saturating_add(i64::from(now.nanos / 1_000_000))
}
