//! The connect-time `robot_state` stream: its receive loop against a scripted
//! receiver, and the registry opening it for each new connection behind its
//! switch.
//!
//! No test pauses the clock. Motion windows are a few hundred milliseconds,
//! and every wait carries a real-clock ceiling that only fires on a regression.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::layer::SubscriberExt;
use wirepod_core::logger::{LogLayer, LogLevel, LogRing, ManualLogClock};
use wirepod_core::robot::observe::{MotionCall, StateSlot};
use wirepod_core::robot::robotstate::RobotStateSample;
use wirepod_core::robot::state_stream::{StateLoopExit, run_state_stream};
use wirepod_core::test_support::{
    FakeConnFactory, FakeReceiver, FakeRobotConn, RobotCall, install_tracing_backstop,
};
use wirepod_core::{
    AppState, BotInfo, BotInfoRobot, ConnError, Esn, EventItem, EventReceiver, RobotConn,
    RobotRegistry, StatusCode, Timings,
};

const CEILING: Duration = Duration::from_secs(5);

/// Long enough for a spawned task to have been polled, had it been spawned.
const PARK_WINDOW: Duration = Duration::from_millis(50);

/// Long enough that a sample sent straight after a stamp lands inside it even
/// on a loaded machine.
const WINDOW: Duration = Duration::from_millis(400);

const ESN: &str = "00303f28";

const RESTING: u32 = 0x100 | 0x200;
const DRIVING: u32 = 0x1 | 0x8000 | 0x100 | 0x200;

async fn within<F: Future>(operation: F) -> F::Output {
    tokio::time::timeout(CEILING, operation)
        .await
        .expect("the operation did not finish inside the ceiling")
}

async fn until(mut ready: impl FnMut() -> bool) {
    within(async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
}

fn sample(status: u32, x_mm: f32) -> RobotStateSample {
    RobotStateSample {
        status,
        x_mm,
        origin_id: 3,
        localized_to_object_id: -1,
        ..RobotStateSample::default()
    }
}

fn timings() -> Timings {
    Timings {
        disconnect_settle: Duration::from_millis(1),
        motion_window: WINDOW,
        ..Timings::instant()
    }
}

fn bot_info() -> BotInfo {
    BotInfo {
        global_guid: "<guid>".to_string(),
        robots: vec![BotInfoRobot {
            esn: ESN.to_string(),
            ip_address: "192.168.8.203".to_string(),
            activated: true,
            ..BotInfoRobot::default()
        }],
        ..BotInfo::default()
    }
}

/// A factory handing `robot` to every caller, while the test keeps the
/// concrete type to read what it recorded.
fn factory(robot: &Arc<FakeRobotConn>) -> Arc<FakeConnFactory> {
    let conn: Arc<dyn RobotConn> = robot.clone();
    Arc::new(FakeConnFactory::connecting_to(conn))
}

fn state_stream_call() -> RobotCall {
    RobotCall::OpenEventStream {
        whitelist: vec!["robot_state".to_string()],
        connection_id: String::new(),
    }
}

fn is_event_stream(call: &RobotCall) -> bool {
    matches!(call, RobotCall::OpenEventStream { .. })
}

type Scripted = Result<Option<EventItem>, ConnError>;

/// An [`EventReceiver`] that can carry state samples, which the shared
/// `FakeReceiver` handle has no way to queue.
struct StateReceiver {
    events: mpsc::UnboundedReceiver<Scripted>,
}

#[async_trait]
impl EventReceiver for StateReceiver {
    async fn next(&mut self) -> Result<Option<EventItem>, ConnError> {
        self.events.recv().await.unwrap_or(Ok(None))
    }
}

struct Robot {
    events: mpsc::UnboundedSender<Scripted>,
}

impl Robot {
    fn send(&self, sample: RobotStateSample) {
        self.events
            .send(Ok(Some(EventItem::State(sample))))
            .expect("the loop is gone");
    }

    fn send_raw(&self, event: Scripted) {
        self.events.send(event).expect("the loop is gone");
    }
}

fn scripted() -> (Box<dyn EventReceiver>, Robot) {
    let (events, receiver) = mpsc::unbounded_channel();
    (
        Box::new(StateReceiver { events: receiver }),
        Robot { events },
    )
}

/// A claimed slot and a loop running against it.
fn running_loop() -> (
    Arc<StateSlot>,
    CancellationToken,
    Robot,
    tokio::task::JoinHandle<StateLoopExit>,
) {
    let slot = Arc::new(StateSlot::new());
    let cancel = CancellationToken::new();
    let generation = slot.claim(cancel.clone()).expect("the first claim");
    let (receiver, robot) = scripted();
    let task = tokio::spawn(run_state_stream(
        receiver,
        Arc::clone(&slot),
        generation,
        cancel.clone(),
        Esn::new(ESN),
        WINDOW,
    ));
    (slot, cancel, robot, task)
}

#[tokio::test]
async fn every_sample_is_stored_and_the_claim_is_given_back_when_the_robot_ends_the_stream() {
    let (slot, _cancel, robot, task) = running_loop();

    for x_mm in [1.0, 2.0, 3.0] {
        let sample = sample(RESTING, x_mm);
        robot.send(sample);
        until(|| slot.latest() == Some(sample)).await;
    }
    robot.send_raw(Ok(Some(EventItem::Other)));
    robot.send_raw(Ok(None));

    assert_eq!(
        within(task).await.expect("the loop panicked"),
        StateLoopExit::StreamEnded
    );
    assert!(!slot.is_running(), "the loop kept its claim");
    assert_eq!(
        slot.latest(),
        Some(sample(RESTING, 3.0)),
        "only a stop forgets the last sample"
    );
    assert!(slot.claim(CancellationToken::new()).is_some());
}

#[tokio::test]
async fn a_failed_receive_and_a_cancellation_both_give_the_claim_back() {
    let (slot, _cancel, robot, task) = running_loop();
    let err = ConnError::new(StatusCode::Unavailable, "transport is closing");
    robot.send_raw(Err(err.clone()));
    assert_eq!(
        within(task).await.expect("the loop panicked"),
        StateLoopExit::Failed(err)
    );
    assert!(!slot.is_running());

    let (slot, cancel, _robot, task) = running_loop();
    cancel.cancel();
    assert_eq!(
        within(task).await.expect("the loop panicked"),
        StateLoopExit::Cancelled
    );
    assert!(!slot.is_running());
}

#[tokio::test]
async fn a_loop_whose_claim_was_taken_cannot_write() {
    let (slot, _cancel, robot, task) = running_loop();

    // The claim is stopped, but the cancellation has not reached the loop, and
    // a new stream claims the slot before it does.
    let _uncancelled = slot.stop().expect("the loop held the claim");
    let current = slot
        .claim(CancellationToken::new())
        .expect("a stopped slot can be claimed again");

    robot.send(sample(DRIVING, 9.0));
    assert_eq!(
        within(task).await.expect("the loop panicked"),
        StateLoopExit::Superseded
    );
    assert_eq!(slot.latest(), None, "the stale loop's sample landed");
    assert!(slot.is_running(), "the stale loop released the new claim");
    assert!(slot.write(current, sample(RESTING, 1.0)));
}

#[tokio::test]
async fn the_loop_writes_what_the_tracker_reports_and_finishes_the_window() {
    install_tracing_backstop();
    let ring = Arc::new(LogRing::new(Arc::new(ManualLogClock::new(
        1,
        "2026.09.25 00:00:00",
    ))));
    let subscriber = tracing_subscriber::registry().with(LogLayer::new(Arc::clone(&ring)));

    let slot = Arc::new(StateSlot::new());
    let cancel = CancellationToken::new();
    let generation = slot.claim(cancel.clone()).expect("the first claim");
    let (receiver, robot) = scripted();
    let task = tokio::spawn(
        run_state_stream(
            receiver,
            Arc::clone(&slot),
            generation,
            cancel.clone(),
            Esn::new(ESN),
            WINDOW,
        )
        .with_subscriber(subscriber),
    );
    let lines = || -> Vec<String> {
        ring.get_entries(LogLevel::Debug, 0)
            .into_iter()
            .map(|entry| entry.msg)
            .collect()
    };
    let call = |rpc: &'static str, args: &str| MotionCall {
        rpc,
        args: args.to_owned(),
        at: Instant::now(),
    };

    // Idle: stored, and nothing written.
    robot.send(sample(RESTING, 0.0));
    until(|| slot.latest() == Some(sample(RESTING, 0.0))).await;

    slot.note_motion_call(call("DriveWheels", "lw=50 rw=50"));
    robot.send(sample(DRIVING, 1.0));
    until(|| lines().len() == 1).await;

    slot.note_motion_call(call("DriveWheels", "lw=0 rw=0"));
    robot.send(sample(RESTING, 2.0));
    until(|| lines().len() == 2).await;

    // A call he ignores: the window opens on one sample and closes on the
    // first one after it.
    slot.note_motion_call(call("MoveHead", "speed=2"));
    robot.send(sample(RESTING, 2.0));
    tokio::time::sleep(WINDOW + Duration::from_millis(50)).await;
    robot.send(sample(RESTING, 2.0));
    until(|| lines().len() == 3).await;
    assert_eq!(
        slot.motion_call(),
        None,
        "the loop did not finish the window"
    );

    assert_eq!(
        lines(),
        [
            "state +[moving wheels_moving] pose=(1.0, 0.0) heading=0.000rad origin=3",
            "state -[moving wheels_moving] pose=(2.0, 0.0) heading=0.000rad origin=3",
            "no movement after MoveHead(speed=2)",
        ]
    );
    for entry in ring.get_entries(LogLevel::Debug, 0) {
        assert_eq!(
            (
                entry.level.as_str(),
                entry.comp.as_str(),
                entry.bot.as_str()
            ),
            ("DEBUG", "sdkapp", ESN)
        );
    }

    cancel.cancel();
    assert_eq!(
        within(task).await.expect("the loop panicked"),
        StateLoopExit::Cancelled
    );
}

#[tokio::test]
async fn a_new_connection_opens_one_state_stream_and_a_disconnect_ends_it() {
    let (receiver, mut handle) = FakeReceiver::new();
    let robot = Arc::new(FakeRobotConn::new().with_event_stream(Ok(Box::new(receiver))));
    let registry = RobotRegistry::new(factory(&robot))
        .with_timings(timings())
        .with_state_stream(true);
    let esn = Esn::new(ESN);

    let entry = within(registry.get_or_connect(&esn, &bot_info()))
        .await
        .expect("the connect failed");
    within(handle.ready()).await;
    within(registry.get_or_connect(&esn, &bot_info()))
        .await
        .expect("the cached lookup failed");

    assert_eq!(
        robot.calls(),
        [RobotCall::BatteryState, state_stream_call()],
        "a connection opens exactly one state stream, and a cached lookup none"
    );
    assert!(entry.session.state_stream.is_running());

    assert!(within(registry.disconnect(&esn)).await);
    within(handle.wait_dropped()).await;
    assert!(!entry.session.state_stream.is_running());
}

#[tokio::test]
async fn with_the_switch_off_a_connection_opens_no_stream() {
    let robot = Arc::new(FakeRobotConn::new());
    let registry = RobotRegistry::new(factory(&robot)).with_timings(timings());

    let entry = within(registry.get_or_connect(&Esn::new(ESN), &bot_info()))
        .await
        .expect("the connect failed");
    tokio::time::sleep(PARK_WINDOW).await;

    assert_eq!(robot.calls(), [RobotCall::BatteryState]);
    assert!(!entry.session.state_stream.is_running());
}

#[tokio::test]
async fn a_stream_that_cannot_open_gives_its_claim_back() {
    // Nothing queued, so the open fails with `Unavailable`.
    let robot = Arc::new(FakeRobotConn::new());
    let registry = RobotRegistry::new(factory(&robot))
        .with_timings(timings())
        .with_state_stream(true);

    let entry = within(registry.get_or_connect(&Esn::new(ESN), &bot_info()))
        .await
        .expect("the connect failed");
    until(|| robot.call_count(is_event_stream) == 1 && !entry.session.state_stream.is_running())
        .await;
}

#[tokio::test]
async fn the_app_state_builder_carries_the_switch() {
    for (on, opened) in [(false, 0), (true, 1)] {
        let (receiver, _handle) = FakeReceiver::new();
        let robot = Arc::new(FakeRobotConn::new().with_event_stream(Ok(Box::new(receiver))));
        let state = AppState::builder(factory(&robot))
            .bot_info(bot_info())
            .timings(timings())
            .state_stream(on)
            .build();

        within(state.get_robot(&Esn::new(ESN)))
            .await
            .expect("the connect failed");
        tokio::time::sleep(PARK_WINDOW).await;
        until(|| robot.call_count(is_event_stream) == opened).await;
        assert_eq!(robot.call_count(is_event_stream), opened, "switch {on}");
    }
}
