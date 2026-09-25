//! The nav map feed task against a scripted stream.
//!
//! The lease is read on a [`ManualClock`], so a test lapses it by moving the
//! clock rather than by waiting fifteen seconds. Tokio's own clock is never
//! paused: the lease tick runs in real time, at the test lease's 20 ms, under a
//! ceiling that fires only on a regression.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
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

fn messages(ring: &LogRing) -> Vec<String> {
    entries(ring).into_iter().map(|entry| entry.msg).collect()
}

/// Lets the feed run until `ready` holds.
async fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    timeout(CEILING, async {
        while !ready() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

fn holds(slot: &MapSlot, expected: &NavMapFrame) -> bool {
    slot.latest().is_some_and(|kept| kept.frame == *expected)
}

async fn exit_of(running: JoinHandle<MapFeedExit>) -> MapFeedExit {
    timeout(CEILING, running)
        .await
        .expect("within the ceiling")
        .expect("the feed task did not panic")
}

/// Waits for the feed to drop the stream `script` feeds.
async fn dropped(script: &Script, which: &str) {
    timeout(CEILING, script.closed())
        .await
        .unwrap_or_else(|_| panic!("the feed kept the {which} stream"));
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

/// A robot whose `NavMapFeed` opens take their answers in the order scripted,
/// and are counted. An open with no answer left is held open without one, as
/// the gateway may hold it while it has no map to send.
struct ScriptedConn {
    inner: FakeRobotConn,
    answers: Mutex<VecDeque<Result<Box<dyn NavMapReceiver>, ConnError>>>,
    opens: AtomicUsize,
}

impl ScriptedConn {
    fn new(answers: Vec<Result<Box<dyn NavMapReceiver>, ConnError>>) -> Arc<Self> {
        Arc::new(Self {
            inner: FakeRobotConn::new(),
            answers: Mutex::new(answers.into()),
            opens: AtomicUsize::new(0),
        })
    }

    fn opens(&self) -> usize {
        self.opens.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl CameraControl for ScriptedConn {
    async fn enable_image_streaming(&self, on: bool) -> Result<(), ConnError> {
        self.inner.enable_image_streaming(on).await
    }
}

#[async_trait]
impl RobotConn for ScriptedConn {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn battery_state(&self) -> Result<BatteryReading, ConnError> {
        self.inner.battery_state().await
    }

    async fn protocol_version(
        &self,
        client_version: i64,
        min_host_version: i64,
    ) -> Result<ProtocolVerdict, ConnError> {
        self.inner
            .protocol_version(client_version, min_host_version)
            .await
    }

    async fn open_event_stream(
        &self,
        whitelist: &[&str],
        connection_id: &str,
    ) -> Result<Box<dyn EventReceiver>, ConnError> {
        self.inner.open_event_stream(whitelist, connection_id).await
    }

    async fn open_camera_feed(&self) -> Result<Box<dyn FrameStream>, ConnError> {
        self.inner.open_camera_feed().await
    }

    async fn pull_jdocs(&self, kinds: &[JdocKind]) -> Result<Vec<NamedJdoc>, ConnError> {
        self.inner.pull_jdocs(kinds).await
    }

    async fn open_nav_map_feed(
        &self,
        _period: Duration,
    ) -> Result<Box<dyn NavMapReceiver>, ConnError> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        let answer = self.answers.lock().expect("answers").pop_front();
        match answer {
            Some(answer) => answer,
            None => std::future::pending().await,
        }
    }
}

fn open_and_run(feed: MapFeed, conn: &Arc<ScriptedConn>) -> JoinHandle<MapFeedExit> {
    let conn = Arc::clone(conn);
    tokio::spawn(async move { feed.open_and_run(conn.as_ref()).await })
}

#[tokio::test]
async fn a_lapsed_lease_ends_an_open_the_robot_never_answers() {
    let clock = Arc::new(ManualClock::new());
    let (slot, feed) = claimed(&clock);
    let conn = ScriptedConn::new(Vec::new());
    let running = open_and_run(feed, &conn);

    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(!running.is_finished(), "a watched open kept waiting");

    clock.advance(Duration::from_secs(1));
    let exit = exit_of(running).await;
    assert_eq!(exit, MapFeedExit::LeaseLapsed);
    assert!(!slot.is_running());
}

/// A feed whose first stream has brought one map, which opened the second.
struct TwoStreams {
    slot: Arc<MapSlot>,
    cancel: CancellationToken,
    conn: Arc<ScriptedConn>,
    first: Script,
    second: Script,
    running: JoinHandle<MapFeedExit>,
}

async fn two_streams(clock: &Arc<ManualClock>) -> TwoStreams {
    let (slot, feed) = claimed(clock);
    let cancel = feed.cancel.clone();
    let (first_receiver, first) = scripted();
    let (second_receiver, second) = scripted();
    let conn = ScriptedConn::new(vec![Ok(first_receiver), Ok(second_receiver)]);
    first.send(Ok(map(3, 1))).expect("listening");
    let running = open_and_run(feed, &conn);
    wait_until("the first map to open a second stream", || {
        conn.opens() == 2
    })
    .await;
    TwoStreams {
        slot,
        cancel,
        conn,
        first,
        second,
        running,
    }
}

#[tokio::test]
async fn the_first_map_opens_one_more_stream_and_the_first_stays_open() {
    let ring = ring();
    let _guard = watching(&ring);
    let clock = Arc::new(ManualClock::new());
    let feed = two_streams(&clock).await;

    for content in [2, 7] {
        feed.first.send(Ok(map(3, content))).expect("listening");
        wait_until("a later map on the first stream", || {
            holds(&feed.slot, &map(3, content))
        })
        .await;
    }
    assert!(!feed.first.is_closed(), "the first stream was kept");
    assert_eq!(feed.conn.opens(), 2, "later maps opened nothing more");

    feed.cancel.cancel();
    assert_eq!(exit_of(feed.running).await, MapFeedExit::Cancelled);
    assert_eq!(
        messages(&ring),
        vec![
            "nav map feed started, at most one map per 500ms",
            "nav map origin=3 root=128mm depth=4 quads=1 clear_of_obstacle=1",
            "nav map feed: reopened alongside the first stream to reassert the broadcast period, \
             which a stream the robot had not yet noticed closing resets to -1 on the next map",
            "nav map feed stopped (cancelled) after 3 maps; last origin=3 root=128mm depth=4 \
             quads=1 cliff=1",
        ]
    );
}

#[tokio::test]
async fn a_map_on_the_second_stream_is_stored() {
    let clock = Arc::new(ManualClock::new());
    let feed = two_streams(&clock).await;

    feed.second.send(Ok(map(4, 1))).expect("listening");
    wait_until("the map on the second stream", || {
        holds(&feed.slot, &map(4, 1))
    })
    .await;

    feed.cancel.cancel();
    assert_eq!(exit_of(feed.running).await, MapFeedExit::Cancelled);
}

#[tokio::test]
async fn a_cancelled_feed_drops_both_streams() {
    let clock = Arc::new(ManualClock::new());
    let feed = two_streams(&clock).await;

    feed.cancel.cancel();
    assert_eq!(exit_of(feed.running).await, MapFeedExit::Cancelled);
    dropped(&feed.first, "first").await;
    dropped(&feed.second, "second").await;
    assert!(!feed.slot.is_running());
    assert_eq!(feed.slot.error(), None);
}

#[tokio::test]
async fn a_lapsed_feed_drops_both_streams() {
    let clock = Arc::new(ManualClock::new());
    let feed = two_streams(&clock).await;

    clock.advance(Duration::from_secs(1));
    assert_eq!(exit_of(feed.running).await, MapFeedExit::LeaseLapsed);
    dropped(&feed.first, "first").await;
    dropped(&feed.second, "second").await;
    assert!(!feed.slot.is_running());
}

#[tokio::test]
async fn an_ending_second_stream_ends_the_feed() {
    let clock = Arc::new(ManualClock::new());
    let feed = two_streams(&clock).await;

    drop(feed.second);
    assert_eq!(exit_of(feed.running).await, MapFeedExit::StreamEnded);
    dropped(&feed.first, "first").await;
    assert_eq!(feed.slot.error().as_deref(), Some(STREAM_ENDED));
}

#[tokio::test]
async fn a_failing_second_stream_ends_the_feed() {
    let clock = Arc::new(ManualClock::new());
    let feed = two_streams(&clock).await;

    let err = ConnError::new(StatusCode::Internal, "NavMemoryMap engine stream died");
    feed.second.send(Err(err.clone())).expect("listening");
    assert_eq!(
        exit_of(feed.running).await,
        MapFeedExit::Failed(err.clone())
    );
    dropped(&feed.first, "first").await;
    assert_eq!(feed.slot.error(), Some(err.to_string()));
}

#[tokio::test]
async fn a_refused_second_open_leaves_the_first_stream_running() {
    let ring = ring();
    let _guard = watching(&ring);
    let clock = Arc::new(ManualClock::new());
    let (slot, feed) = claimed(&clock);
    let (receiver, script) = scripted();
    let refusal = ConnError::new(StatusCode::Unavailable, "too many streams");
    let conn = ScriptedConn::new(vec![Ok(receiver), Err(refusal)]);
    script.send(Ok(map(3, 1))).expect("listening");
    let running = open_and_run(feed, &conn);
    wait_until("the first map to try a second stream", || conn.opens() == 2).await;

    script
        .send(Ok(map(3, 2)))
        .expect("the first stream is still read");
    wait_until("the next map", || holds(&slot, &map(3, 2))).await;
    assert!(slot.is_running());

    drop(script);
    assert_eq!(exit_of(running).await, MapFeedExit::StreamEnded);
    assert_eq!(conn.opens(), 2, "the refusal was not retried");
    let refused: Vec<String> = messages(&ring)
        .into_iter()
        .filter(|msg| msg.starts_with("nav map feed: could not reopen"))
        .collect();
    assert_eq!(
        refused,
        vec![
            "nav map feed: could not reopen alongside the first stream to reassert the broadcast \
             period, so it carries on with the first alone: rpc error: code = Unavailable desc = \
             too many streams"
        ]
    );
}

#[tokio::test]
async fn a_second_open_the_robot_never_answers_holds_nothing_up() {
    let clock = Arc::new(ManualClock::new());
    let (slot, feed) = claimed(&clock);
    let cancel = feed.cancel.clone();
    let (receiver, script) = scripted();
    // Only the first open is answered.
    let conn = ScriptedConn::new(vec![Ok(receiver)]);
    script.send(Ok(map(3, 1))).expect("listening");
    let running = open_and_run(feed, &conn);
    wait_until("the first map to try a second stream", || conn.opens() == 2).await;

    script.send(Ok(map(3, 2))).expect("listening");
    wait_until("the next map", || holds(&slot, &map(3, 2))).await;

    cancel.cancel();
    assert_eq!(exit_of(running).await, MapFeedExit::Cancelled);
    dropped(&script, "first").await;
}

#[tokio::test]
async fn a_feed_after_a_failure_logs_its_start_and_stop_once_it_has_a_map() {
    let ring = ring();
    let _guard = watching(&ring);
    let clock = Arc::new(ManualClock::new());
    let (_slot, mut feed) = claimed(&clock);
    feed.previous_error = Some(STREAM_ENDED.to_owned());
    let (receiver, script) = scripted();
    script.send(Ok(map(3, 1))).expect("listening");
    drop(script);

    let exit = timeout(CEILING, feed.run(receiver))
        .await
        .expect("within the ceiling");
    assert_eq!(exit, MapFeedExit::StreamEnded);
    assert_eq!(
        messages(&ring),
        vec![
            "nav map feed started, at most one map per 500ms",
            "nav map origin=3 root=128mm depth=4 quads=1 clear_of_obstacle=1",
            "nav map feed stopped (stream ended) after 1 maps; last origin=3 root=128mm depth=4 \
             quads=1 clear_of_obstacle=1",
        ],
        "the stream ended as the one before it did, but it brought a map first"
    );
}
