//! The nav map feed task against a scripted stream.
//!
//! The lease is read on a [`ManualClock`], so a test lapses it by moving the
//! clock rather than by waiting fifteen seconds. Tokio's own clock is never
//! paused: the lease tick runs in real time, at the test lease's 20 ms, under a
//! ceiling that fires only on a regression.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt;
use wirepod_core::logger::{Entry, LogLayer, LogLevel, LogRing, ManualLogClock};
use wirepod_core::robot::conn::NavMapReceiver;
use wirepod_core::robot::navmap::{NavMapFrame, NavMapInfo, NavMapQuad};
use wirepod_core::robot::navmap_feed::{MapFeed, MapFeedExit, STREAM_ENDED};
use wirepod_core::robot::observe::MapSlot;
use wirepod_core::test_support::{FakeRobotConn, install_tracing_backstop};
use wirepod_core::wallclock::{FixedWallClock, WallTime};
use wirepod_core::{
    BatteryReading, CameraControl, Clock, ConnError, Esn, EventReceiver, FrameStream, Generation,
    JdocKind, ManualClock, NamedJdoc, ProtocolVerdict, RobotConn, StatusCode, Timings,
};

const CEILING: Duration = Duration::from_secs(5);

const SERIAL: &str = "00303f28";

/// The wall clock every map is stamped on: 1790000000 s and 250 ms.
const WALL: WallTime = WallTime::new(1_790_000_000, 250_000_000);

/// A stream the test feeds by hand. Dropping the sender ends it cleanly.
struct ScriptedReceiver(mpsc::UnboundedReceiver<Result<NavMapFrame, ConnError>>);

#[async_trait]
impl NavMapReceiver for ScriptedReceiver {
    async fn next(&mut self) -> Result<Option<NavMapFrame>, ConnError> {
        self.0.recv().await.transpose()
    }
}

type Script = mpsc::UnboundedSender<Result<NavMapFrame, ConnError>>;

fn scripted() -> (Box<dyn NavMapReceiver>, Script) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (Box::new(ScriptedReceiver(receiver)), sender)
}

/// A single-leaf map in the frame `origin_id` names.
fn map(origin_id: u32, content: i32) -> NavMapFrame {
    NavMapFrame {
        origin_id,
        info: NavMapInfo {
            root_depth: 4,
            root_size_mm: 128.0,
            root_center_x: 0.0,
            root_center_y: 0.0,
        },
        quads: vec![NavMapQuad {
            content,
            depth: 4,
            rgba: 0xff,
        }],
    }
}

/// The durations the feed runs on here: a lease short enough to tick often.
fn timings() -> Timings {
    Timings {
        map_lease: Duration::from_millis(20),
        ..Timings::default()
    }
}

/// A claimed, watched slot and a feed for it.
fn claimed(clock: &Arc<ManualClock>) -> (Arc<MapSlot>, MapFeed) {
    let slot = Arc::new(MapSlot::new());
    let cancel = CancellationToken::new();
    let generation = slot.claim(cancel.clone()).expect("a fresh slot is free");
    slot.watch(clock.now());
    let feed = feed_for(&slot, generation, cancel, clock);
    (slot, feed)
}

fn feed_for(
    slot: &Arc<MapSlot>,
    generation: Generation,
    cancel: CancellationToken,
    clock: &Arc<ManualClock>,
) -> MapFeed {
    MapFeed {
        slot: Arc::clone(slot),
        generation,
        cancel,
        serial: Esn::new(SERIAL),
        clock: Arc::clone(clock) as Arc<dyn Clock>,
        wall: Arc::new(FixedWallClock::new(WALL, 0)),
        timings: timings(),
        previous_error: None,
    }
}

fn ring() -> Arc<LogRing> {
    Arc::new(LogRing::new(Arc::new(ManualLogClock::new(
        1_790_000_000_000,
        "2026.09.25 00:00:00",
    ))))
}

/// Routes this thread's log lines into `ring`. The tests run on tokio's
/// current-thread runtime, so the feed logs on this thread too.
fn watching(ring: &Arc<LogRing>) -> tracing::subscriber::DefaultGuard {
    install_tracing_backstop();
    let subscriber = tracing_subscriber::registry().with(LogLayer::new(Arc::clone(ring)));
    tracing::subscriber::set_default(subscriber)
}

fn entries(ring: &LogRing) -> Vec<Entry> {
    ring.get_entries(LogLevel::Debug, 0)
}

#[tokio::test]
async fn every_map_is_stored_with_its_arrival_time() {
    let clock = Arc::new(ManualClock::new());
    let (slot, feed) = claimed(&clock);
    let (receiver, script) = scripted();
    script.send(Ok(map(3, 1))).expect("the feed is listening");
    script.send(Ok(map(3, 7))).expect("the feed is listening");
    drop(script);

    let exit = timeout(CEILING, feed.run(receiver))
        .await
        .expect("within the ceiling");
    assert_eq!(exit, MapFeedExit::StreamEnded);

    let latest = slot.latest().expect("the last map is kept");
    assert_eq!(latest.frame, map(3, 7));
    assert_eq!(latest.received_ms, 1_790_000_000_250);
    assert!(!slot.is_running(), "the feed gave its claim back");
    assert_eq!(slot.error().as_deref(), Some(STREAM_ENDED));
}

#[tokio::test]
async fn an_origin_change_is_logged_at_once_and_a_repeat_map_is_not() {
    let ring = ring();
    let _guard = watching(&ring);
    let clock = Arc::new(ManualClock::new());
    let (_slot, feed) = claimed(&clock);
    let (receiver, script) = scripted();
    for (origin, content) in [(3, 1), (3, 2), (4, 1)] {
        script.send(Ok(map(origin, content))).expect("listening");
    }
    drop(script);

    timeout(CEILING, feed.run(receiver))
        .await
        .expect("within the ceiling");

    let lines = entries(&ring);
    let messages: Vec<&str> = lines.iter().map(|entry| entry.msg.as_str()).collect();
    assert_eq!(
        messages,
        vec![
            "nav map feed started, at most one map per 500ms",
            "nav map origin=3 root=128mm depth=4 quads=1 clear_of_obstacle=1",
            "map reset: origin 3 -> 4; origin=4 root=128mm depth=4 quads=1 clear_of_obstacle=1",
            "nav map feed stopped (stream ended) after 3 maps; last origin=4 root=128mm depth=4 \
             quads=1 clear_of_obstacle=1",
        ],
        "the second map in origin 3 is inside the summary gap and earns no line"
    );
    for entry in &lines {
        assert_eq!(entry.level, "DEBUG");
        assert_eq!(entry.comp, "sdkapp");
        assert_eq!(entry.bot, SERIAL);
    }
}

#[tokio::test]
async fn a_malformed_map_is_dropped_and_the_last_good_one_kept() {
    let ring = ring();
    let _guard = watching(&ring);
    let clock = Arc::new(ManualClock::new());
    let (slot, feed) = claimed(&clock);
    let (receiver, script) = scripted();
    // One leaf under a root one level taller than it: a quarter of a map.
    let mut truncated = map(3, 4);
    truncated.info.root_depth = 5;
    script.send(Ok(map(3, 1))).expect("listening");
    script.send(Ok(truncated.clone())).expect("listening");
    script.send(Ok(truncated)).expect("listening");
    drop(script);

    let exit = timeout(CEILING, feed.run(receiver))
        .await
        .expect("within the ceiling");
    assert_eq!(exit, MapFeedExit::StreamEnded);
    assert_eq!(
        slot.latest().map(|kept| kept.frame.clone()),
        Some(map(3, 1)),
        "the good map stays"
    );

    let lines = entries(&ring);
    let messages: Vec<&str> = lines.iter().map(|entry| entry.msg.as_str()).collect();
    assert_eq!(
        messages,
        vec![
            "nav map feed started, at most one map per 500ms",
            "nav map origin=3 root=128mm depth=4 quads=1 clear_of_obstacle=1",
            "dropped a nav map (1 so far): malformed nav map: 1 quads do not cover the root",
            "nav map feed stopped (stream ended) after 1 maps and 2 malformed; last origin=3 \
             root=128mm depth=4 quads=1 clear_of_obstacle=1",
        ],
        "the second malformed map is inside the gap and earns no line"
    );
}

#[tokio::test]
async fn the_feed_runs_while_watched_and_ends_when_the_lease_lapses() {
    let clock = Arc::new(ManualClock::new());
    let (slot, feed) = claimed(&clock);
    let (receiver, script) = scripted();
    script.send(Ok(map(5, 1))).expect("listening");
    let running = tokio::spawn(feed.run(receiver));

    // Several lease ticks go by on a clock that has not moved.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!running.is_finished(), "a watched feed kept running");
    assert!(slot.is_running());

    clock.advance(Duration::from_secs(1));
    let exit = timeout(CEILING, running)
        .await
        .expect("within the ceiling")
        .expect("the feed task did not panic");
    assert_eq!(exit, MapFeedExit::LeaseLapsed);
    assert!(!slot.is_running(), "a lapsed feed gave its claim back");
    assert_eq!(slot.error(), None, "a lapse is not an error");
    assert_eq!(
        slot.latest().map(|kept| kept.frame.origin_id),
        Some(5),
        "the map outlives the feed that brought it"
    );
    drop(script);
}

#[tokio::test]
async fn a_cancelled_feed_gives_its_claim_back() {
    let clock = Arc::new(ManualClock::new());
    let slot = Arc::new(MapSlot::new());
    let cancel = CancellationToken::new();
    let generation = slot.claim(cancel.clone()).expect("free");
    slot.watch(clock.now());
    let (receiver, _script) = scripted();
    let running = tokio::spawn(feed_for(&slot, generation, cancel.clone(), &clock).run(receiver));

    cancel.cancel();
    let exit = timeout(CEILING, running)
        .await
        .expect("within the ceiling")
        .expect("the feed task did not panic");
    assert_eq!(exit, MapFeedExit::Cancelled);
    assert!(!slot.is_running());
    assert_eq!(slot.error(), None);
}

#[tokio::test]
async fn a_superseded_feed_cannot_write() {
    let clock = Arc::new(ManualClock::new());
    let slot = Arc::new(MapSlot::new());
    let stale_cancel = CancellationToken::new();
    let stale = slot.claim(stale_cancel.clone()).expect("free");
    slot.watch(clock.now());
    // A disconnect stops the slot and the next snapshot claims it again, all
    // before the old feed has noticed.
    let _ = slot.stop();
    let current = slot.claim(CancellationToken::new()).expect("free again");

    let (receiver, script) = scripted();
    script.send(Ok(map(9, 1))).expect("listening");
    drop(script);
    let exit = timeout(
        CEILING,
        feed_for(&slot, stale, stale_cancel, &clock).run(receiver),
    )
    .await
    .expect("within the ceiling");
    assert_eq!(exit, MapFeedExit::StreamEnded);

    assert_eq!(slot.latest(), None, "the stale feed wrote nothing");
    assert_eq!(slot.error(), None, "nor recorded its end");
    assert!(slot.is_running(), "nor freed the new claim");
    assert!(slot.release(current));
}

#[tokio::test]
async fn a_robot_without_the_feed_is_reported_once() {
    let ring = ring();
    let _guard = watching(&ring);
    let clock = Arc::new(ManualClock::new());
    let conn = FakeRobotConn::new();

    let (slot, feed) = claimed(&clock);
    let exit = timeout(CEILING, feed.open_and_run(&conn))
        .await
        .expect("within the ceiling");
    let MapFeedExit::Failed(err) = exit else {
        panic!("the open should fail, not {exit:?}");
    };
    assert_eq!(err.code, StatusCode::Unimplemented);
    assert!(!slot.is_running(), "a failed open gave its claim back");
    let error = slot.error().expect("the failure is recorded");
    assert_eq!(error, err.to_string());
    assert_eq!(entries(&ring).len(), 2, "one start line and one stop line");

    // The next poll restarts it, and it fails the same way without a word.
    let previous_error = slot.error();
    let cancel = CancellationToken::new();
    let generation = slot.claim(cancel.clone()).expect("free again");
    let mut retry = feed_for(&slot, generation, cancel, &clock);
    retry.previous_error = previous_error;
    let exit = timeout(CEILING, retry.open_and_run(&conn))
        .await
        .expect("within the ceiling");
    assert!(matches!(exit, MapFeedExit::Failed(_)));
    assert_eq!(slot.error(), Some(error));
    assert_eq!(entries(&ring).len(), 2, "a repeated failure is not logged");
}

/// A robot whose gateway holds the `NavMapFeed` call open without answering,
/// as it may while it has no map to send.
struct SilentConn(FakeRobotConn);

#[async_trait]
impl CameraControl for SilentConn {
    async fn enable_image_streaming(&self, on: bool) -> Result<(), ConnError> {
        self.0.enable_image_streaming(on).await
    }
}

#[async_trait]
impl RobotConn for SilentConn {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn battery_state(&self) -> Result<BatteryReading, ConnError> {
        self.0.battery_state().await
    }

    async fn protocol_version(
        &self,
        client_version: i64,
        min_host_version: i64,
    ) -> Result<ProtocolVerdict, ConnError> {
        self.0
            .protocol_version(client_version, min_host_version)
            .await
    }

    async fn open_event_stream(
        &self,
        whitelist: &[&str],
        connection_id: &str,
    ) -> Result<Box<dyn EventReceiver>, ConnError> {
        self.0.open_event_stream(whitelist, connection_id).await
    }

    async fn open_camera_feed(&self) -> Result<Box<dyn FrameStream>, ConnError> {
        self.0.open_camera_feed().await
    }

    async fn pull_jdocs(&self, kinds: &[JdocKind]) -> Result<Vec<NamedJdoc>, ConnError> {
        self.0.pull_jdocs(kinds).await
    }

    async fn open_nav_map_feed(
        &self,
        _period: Duration,
    ) -> Result<Box<dyn NavMapReceiver>, ConnError> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn a_lapsed_lease_ends_an_open_the_robot_never_answers() {
    let clock = Arc::new(ManualClock::new());
    let (slot, feed) = claimed(&clock);
    let conn = SilentConn(FakeRobotConn::new());
    let running = tokio::spawn(async move { feed.open_and_run(&conn).await });

    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(!running.is_finished(), "a watched open kept waiting");

    clock.advance(Duration::from_secs(1));
    let exit = timeout(CEILING, running)
        .await
        .expect("within the ceiling")
        .expect("the feed task did not panic");
    assert_eq!(exit, MapFeedExit::LeaseLapsed);
    assert!(!slot.is_running());
}
