//! The robot registry: the connect path, the per-serial connect lock, the idle
//! rule and the disconnect.
//!
//! Go has no test for any of this, so these are written against the source
//! rather than ported. The behaviours they pin are the ones a handler can
//! observe: the exact `not found` text, that a connect issues one `BatteryState`
//! and nothing else, that the same serial dials once while a different serial
//! does not wait, the 300 second boundary, and that a disconnect stops both
//! streams, pays its settle and leaves the camera meter alone.
//!
//! No test pauses the clock. Idle time is driven by a [`ManualClock`], the
//! settles are milliseconds, and every async test carries a real-clock ceiling
//! that only fires on a regression.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use wirepod_core::test_support::{
    FakeConnFactory, FakeFrameStream, FakeReceiver, FakeRobotConn, RecordingSink, RobotCall,
};
use wirepod_core::{
    BotInfo, BotInfoRobot, CameraControl, Clock, ConnError, Esn, EventLoopExit, GetRobotError,
    ManualClock, PumpExit, RobotConn, RobotConnFactory, RobotRegistry, StatusCode, Timings,
    cam_stream_pump, run_event_stream, start_cam_stream,
};

/// Nothing here waits on anything real beyond a 200 millisecond settle, so this
/// only ever fires on a regression.
const CEILING: Duration = Duration::from_secs(2);

/// Long enough for a parked task to have been polled and to have reached the
/// lock it is supposed to wait on.
const PARK_WINDOW: Duration = Duration::from_millis(50);

/// The robot this machine actually has.
const ESN_A: &str = "00303f28";

/// A second serial, so the per-serial connect lock has something to not block.
const ESN_B: &str = "00e20100";

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

fn listed(esn: &str, ip: &str) -> BotInfoRobot {
    BotInfoRobot {
        esn: esn.to_string(),
        ip_address: ip.to_string(),
        activated: true,
        ..BotInfoRobot::default()
    }
}

/// Two robots with no GUID of their own, so both fall back to the global one.
fn bot_info() -> BotInfo {
    BotInfo {
        global_guid: "<guid>".to_string(),
        robots: vec![
            listed(ESN_A, "192.168.8.203"),
            listed(ESN_B, "192.168.8.204"),
        ],
        ..BotInfo::default()
    }
}

/// A robot and a factory handing it to every caller, both kept as their
/// concrete type so the test can read what they recorded.
fn fakes(robot: FakeRobotConn) -> (Arc<FakeRobotConn>, Arc<FakeConnFactory>) {
    let robot = Arc::new(robot);
    let conn: Arc<dyn RobotConn> = robot.clone();
    (robot, Arc::new(FakeConnFactory::connecting_to(conn)))
}

fn registry(factory: &Arc<FakeConnFactory>) -> RobotRegistry {
    let factory: Arc<dyn RobotConnFactory> = factory.clone();
    RobotRegistry::new(factory).with_timings(Timings::instant())
}

#[test]
fn the_registry_defaults_hold_the_go_values() {
    let (_robot, factory) = fakes(FakeRobotConn::new());
    let factory: Arc<dyn RobotConnFactory> = factory;
    let registry = RobotRegistry::new(factory);

    assert_eq!(*registry.timings(), Timings::default());
    assert_eq!(
        registry.liveness_deadline(),
        None,
        "the connect-time liveness check picked up a deadline Go does not have"
    );
    assert!(registry.is_empty());
    assert_eq!(registry.len(), 0);
}

#[test]
fn peek_never_dials() {
    let (_robot, factory) = fakes(FakeRobotConn::new());
    let registry = registry(&factory);

    assert!(registry.peek(&Esn::new(ESN_A)).is_none());
    assert_eq!(factory.connect_count(), 0, "peek dialled the robot");
    assert_eq!(
        registry.read_meter(&Esn::new(ESN_A)),
        (0, 0),
        "an ESN nobody has streamed did not read zero"
    );
}

/// Go's `newRobot` fails the bot-info scan before it builds anything
/// (`robot.go:349`), and the handler prefixes the message again
/// (`server.go:61`), which is what doubles the `error: ` in the body.
#[tokio::test]
async fn an_unknown_serial_is_not_found_and_dials_nothing() {
    let (_robot, factory) = fakes(FakeRobotConn::new());
    let registry = registry(&factory);

    let err = within(registry.get_or_connect(&Esn::new("deadbeef"), &bot_info()))
        .await
        .expect_err("an unlisted serial connected");

    assert_eq!(err, GetRobotError::NotFound);
    assert_eq!(err.to_string(), "error: robot not found in SDK info file");
    assert_eq!(factory.connect_count(), 0, "an unlisted serial was dialled");
    assert!(registry.is_empty());
}

/// Go opens a second `EventStream` at connect time and never reads it
/// (`robot.go:371-382`). The port does not, so a connect is one `BatteryState`
/// and nothing else.
#[tokio::test]
async fn a_connect_issues_only_the_liveness_call_and_caches_the_entry() {
    let (robot, factory) = fakes(FakeRobotConn::new());
    let registry = registry(&factory);
    let esn = Esn::new(ESN_A);

    let entry = within(registry.get_or_connect(&esn, &bot_info()))
        .await
        .expect("the connect failed");

    assert_eq!(
        robot.calls(),
        vec![RobotCall::BatteryState],
        "the connect issued something other than the liveness call"
    );
    assert_eq!(entry.esn, esn);
    assert_eq!(entry.target.grpc_target(), "192.168.8.203:443");
    assert_eq!(
        entry.target.guid, "<guid>",
        "an empty per-robot GUID did not fall back to the global one"
    );
    assert_eq!(registry.len(), 1);

    let cached = within(registry.get_or_connect(&esn, &bot_info()))
        .await
        .expect("the cached lookup failed");
    assert!(
        Arc::ptr_eq(&entry, &cached),
        "the cache handed back a new entry"
    );
    assert_eq!(
        factory.connect_count(),
        1,
        "a cached robot was dialled again"
    );
    assert_eq!(robot.calls(), vec![RobotCall::BatteryState]);
}

#[tokio::test]
async fn a_failed_dial_propagates_and_caches_nothing() {
    let err = ConnError::new(StatusCode::Unavailable, "connection refused");
    let factory = Arc::new(FakeConnFactory::failing(err.clone()));
    let registry = registry(&factory);

    let got = within(registry.get_or_connect(&Esn::new(ESN_A), &bot_info()))
        .await
        .expect_err("a failed dial produced a robot");

    assert_eq!(got, GetRobotError::Conn(err));
    assert_eq!(
        got.to_string(),
        "rpc error: code = Unavailable desc = connection refused"
    );
    assert!(registry.is_empty(), "a failed dial cached an entry");
}

#[tokio::test]
async fn a_failed_liveness_check_propagates_and_caches_nothing() {
    let err = ConnError::new(StatusCode::Unavailable, "connection refused");
    let (robot, factory) = fakes(FakeRobotConn::new().with_battery(Err(err.clone())));
    let registry = registry(&factory);

    let got = within(registry.get_or_connect(&Esn::new(ESN_A), &bot_info()))
        .await
        .expect_err("a robot that failed its liveness check was cached");

    assert_eq!(got, GetRobotError::Conn(err));
    assert_eq!(
        got.to_string(),
        "rpc error: code = Unavailable desc = connection refused"
    );
    assert_eq!(robot.calls(), vec![RobotCall::BatteryState]);
    assert!(
        registry.is_empty(),
        "a robot that failed its liveness check was cached"
    );
    assert_eq!(factory.connect_count(), 1);
}

/// Go's liveness check runs on a bare `context.Background()` and hangs forever
/// against a robot that is powered off but still routes (`robot.go:365`). The
/// deadline is an `Option` so a test can bound it; the default stays `None`.
#[tokio::test]
async fn a_liveness_deadline_maps_a_hung_robot_to_the_go_deadline_error() {
    let (_robot, factory) = fakes(FakeRobotConn::new().with_battery_delay(Duration::from_secs(30)));
    let registry = registry(&factory).with_liveness_deadline(Some(Duration::from_millis(20)));

    let err = within(registry.get_or_connect(&Esn::new(ESN_A), &bot_info()))
        .await
        .expect_err("a hung liveness check answered");

    assert_eq!(
        err.to_string(),
        "rpc error: code = DeadlineExceeded desc = context deadline exceeded"
    );
    assert!(registry.is_empty(), "a hung dial cached an entry");
}

/// The same serial dials once. This is the half of deviation 8 that Go's
/// `inhibitCreation` flag also gets right.
#[tokio::test]
async fn two_requests_for_one_serial_dial_once() {
    let (robot, factory) = fakes(FakeRobotConn::new());
    let registry = Arc::new(registry(&factory));
    let gate = factory.arm_connect_gate();

    let first = tokio::spawn({
        let registry = Arc::clone(&registry);
        async move { registry.get_or_connect(&Esn::new(ESN_A), &bot_info()).await }
    });
    within(gate.wait_entered()).await;

    let mut second = tokio::spawn({
        let registry = Arc::clone(&registry);
        async move { registry.get_or_connect(&Esn::new(ESN_A), &bot_info()).await }
    });
    assert!(
        tokio::time::timeout(PARK_WINDOW, &mut second)
            .await
            .is_err(),
        "the second request answered while the first dial was still parked"
    );

    gate.release();
    let first = within(first)
        .await
        .expect("the first task panicked")
        .expect("the first connect failed");
    let second = within(second)
        .await
        .expect("the second task panicked")
        .expect("the second connect failed");

    assert!(
        Arc::ptr_eq(&first, &second),
        "the second request built its own entry for a serial that was already connected"
    );
    assert_eq!(factory.connect_count(), 1, "the same serial dialled twice");
    assert_eq!(robot.calls(), vec![RobotCall::BatteryState]);
    assert_eq!(registry.len(), 1);
}

/// The other half of deviation 8, which Go gets wrong: a dial for one serial
/// must not stall a request for another. Under Go's global flag the second
/// request here would spin until the first dial finished, so the ceiling is the
/// assertion.
#[tokio::test]
async fn a_dial_for_one_serial_does_not_block_another() {
    let (_robot, factory) = fakes(FakeRobotConn::new());
    let registry = Arc::new(registry(&factory));
    let gate = factory.arm_connect_gate();

    let parked = tokio::spawn({
        let registry = Arc::clone(&registry);
        async move { registry.get_or_connect(&Esn::new(ESN_A), &bot_info()).await }
    });
    within(gate.wait_entered()).await;
    assert!(
        registry.peek(&Esn::new(ESN_A)).is_none(),
        "the parked dial cached its entry before it finished"
    );

    let other = within(registry.get_or_connect(&Esn::new(ESN_B), &bot_info()))
        .await
        .expect("the second serial failed to connect while the first was parked");
    assert_eq!(other.esn, Esn::new(ESN_B));
    assert_eq!(factory.connect_count(), 2);

    gate.release();
    within(parked)
        .await
        .expect("the parked task panicked")
        .expect("the parked connect failed");
    assert_eq!(registry.len(), 2);
}

/// Go's timer fires on `ConnTimer >= 300` (`robot.go:446`), so 300 seconds is a
/// candidate and 299 is not. The preamble's one write is the only thing that
/// resets it (`server.go:65`).
#[tokio::test]
async fn the_idle_rule_fires_at_three_hundred_seconds_and_touch_resets_it() {
    let (_robot, factory) = fakes(FakeRobotConn::new());
    let clock = Arc::new(ManualClock::new());
    let esn = Esn::new(ESN_A);
    let registry = registry(&factory)
        .with_timings(Timings {
            idle: Duration::from_secs(300),
            ..Timings::instant()
        })
        .with_clock(Arc::clone(&clock) as Arc<dyn Clock>);

    within(registry.get_or_connect(&esn, &bot_info()))
        .await
        .expect("the connect failed");

    assert!(
        registry
            .idle_candidates(Duration::from_secs(299))
            .is_empty(),
        "a robot idle for 299 seconds was a candidate"
    );
    assert_eq!(
        registry.idle_candidates(Duration::from_secs(300)),
        vec![esn.clone()],
        "a robot idle for 300 seconds was not a candidate"
    );

    clock.set(Duration::from_secs(300));
    assert!(registry.touch(&esn), "touch did not find a connected robot");
    assert!(
        registry
            .idle_candidates(Duration::from_secs(300))
            .is_empty(),
        "touch did not reset the idle timer"
    );
    assert!(
        registry
            .idle_candidates(Duration::from_secs(599))
            .is_empty()
    );
    assert_eq!(
        registry.idle_candidates(Duration::from_secs(600)),
        vec![esn]
    );

    assert!(
        !registry.touch(&Esn::new("deadbeef")),
        "touch reported a robot that is not connected"
    );
}

/// The idle clock is the registry's injected one, not a hidden `Instant::now`.
#[tokio::test]
async fn touch_stamps_the_injected_clock() {
    let (_robot, factory) = fakes(FakeRobotConn::new());
    let clock = Arc::new(ManualClock::at(Duration::from_secs(7)));
    let esn = Esn::new(ESN_A);
    let registry = registry(&factory).with_clock(Arc::clone(&clock) as Arc<dyn Clock>);

    let entry = within(registry.get_or_connect(&esn, &bot_info()))
        .await
        .expect("the connect failed");
    assert_eq!(
        entry.last_touch(),
        Duration::from_secs(7),
        "the connect did not stamp the entry from the injected clock"
    );

    clock.set(Duration::from_secs(42));
    assert!(registry.touch(&esn));
    assert_eq!(
        entry.last_touch(),
        Duration::from_secs(42),
        "touch read something other than the injected clock"
    );
    assert_eq!(
        entry.idle_for(Duration::from_secs(50)),
        Duration::from_secs(8)
    );
}

#[tokio::test]
async fn evict_idle_drops_only_the_robots_past_the_limit() {
    let (_robot, factory) = fakes(FakeRobotConn::new());
    let clock = Arc::new(ManualClock::new());
    let kept = Esn::new(ESN_A);
    let dropped = Esn::new(ESN_B);
    let registry = registry(&factory)
        .with_timings(Timings {
            idle: Duration::from_secs(300),
            ..Timings::instant()
        })
        .with_clock(Arc::clone(&clock) as Arc<dyn Clock>);

    within(registry.get_or_connect(&kept, &bot_info()))
        .await
        .expect("the first connect failed");
    within(registry.get_or_connect(&dropped, &bot_info()))
        .await
        .expect("the second connect failed");

    clock.set(Duration::from_secs(300));
    assert!(registry.touch(&kept));

    let evicted = within(registry.evict_idle(Duration::from_secs(300))).await;
    assert_eq!(evicted, vec![dropped.clone()]);
    assert!(
        registry.peek(&kept).is_some(),
        "a touched robot was evicted"
    );
    assert!(registry.peek(&dropped).is_none());
    assert_eq!(registry.len(), 1);
}

/// Go's `removeRobot` stops the camera, stops the stim stream, sleeps three
/// seconds and only then drops the entry (`robot.go:455-480`). Both stops
/// cancel as well as clearing a flag, because a handler parked in a receive
/// cannot see a flag at all. The camera meter is not part of what it clears.
#[tokio::test]
async fn disconnect_stops_both_streams_pays_the_settle_and_keeps_the_meter() {
    let (robot, factory) = fakes(FakeRobotConn::new());
    let timings = Timings {
        disconnect_settle: Duration::from_millis(200),
        enable: Duration::from_secs(5),
        ..Timings::instant()
    };
    let registry = Arc::new(registry(&factory).with_timings(timings));
    let esn = Esn::new(ESN_A);

    let entry = within(registry.get_or_connect(&esn, &bot_info()))
        .await
        .expect("the connect failed");

    let (receiver, mut receiver_handle) = FakeReceiver::new();
    let event_cancel = CancellationToken::new();
    let generation = entry
        .session
        .events
        .claim(event_cancel.clone())
        .expect("the stim stream was already owned");
    let stim = tokio::spawn(run_event_stream(
        Box::new(receiver),
        Arc::clone(&entry.session.events),
        generation,
        event_cancel,
    ));
    within(receiver_handle.ready()).await;

    let camera: Arc<dyn CameraControl> = entry.conn.clone();
    let cam_cancel = CancellationToken::new();
    let guard = within(start_cam_stream(
        Arc::clone(&entry.session),
        camera,
        &timings,
        cam_cancel.clone(),
    ))
    .await
    .expect("the camera claim failed");

    let (mut frames, frame_handle) = FakeFrameStream::new();
    let (mut sink, log) = RecordingSink::new();
    let meter = registry.meter(&esn);
    let pump = tokio::spawn({
        let cancel = cam_cancel.clone();
        async move { cam_stream_pump(&mut frames, &meter, &mut sink, cancel).await }
    });
    frame_handle.send_frame(vec![0u8; 1500]);
    until(|| log.frame_count() == 1).await;
    assert_eq!(registry.read_meter(&esn), (1500, 1));

    let mut disconnecting = tokio::spawn({
        let registry = Arc::clone(&registry);
        let esn = esn.clone();
        async move { registry.disconnect(&esn).await }
    });
    assert!(
        tokio::time::timeout(PARK_WINDOW, &mut disconnecting)
            .await
            .is_err(),
        "disconnect answered without paying the settle"
    );
    assert!(
        within(disconnecting)
            .await
            .expect("the disconnect task panicked"),
        "disconnect did not report the robot as connected"
    );

    assert_eq!(
        within(stim).await.expect("the stim task panicked"),
        EventLoopExit::Cancelled,
        "disconnect left the stim receiver running"
    );
    assert_eq!(
        within(pump).await.expect("the pump task panicked"),
        PumpExit::Cancelled,
        "disconnect left the camera feed running"
    );
    assert!(cam_cancel.is_cancelled());
    assert!(!entry.session.events.is_streaming());
    assert!(!entry.session.cam.is_streaming());
    assert!(
        robot
            .calls()
            .contains(&RobotCall::EnableImageStreaming(false)),
        "disconnect left a claimed camera on"
    );

    assert!(
        registry.peek(&esn).is_none(),
        "the entry survived a disconnect"
    );
    assert!(registry.is_empty());
    assert_eq!(
        registry.read_meter(&esn),
        (1500, 1),
        "the camera meter was pruned along with the entry"
    );

    let reconnected = within(registry.get_or_connect(&esn, &bot_info()))
        .await
        .expect("the reconnect failed");
    assert_eq!(
        factory.connect_count(),
        2,
        "the reconnect reused a dropped entry"
    );
    assert_eq!(reconnected.esn, esn);
    assert_eq!(
        registry.read_meter(&esn),
        (1500, 1),
        "a reconnect reset the camera meter and looked like a server restart"
    );

    // The claim outlives the entry, because Go's `stopCamStream` deliberately
    // leaves the ownership entry for the departing handler to release
    // (`robot.go:136-144`).
    assert!(within(guard.finish()).await);
    drop(frame_handle);
    drop(receiver_handle);
}

#[tokio::test]
async fn disconnecting_an_unconnected_robot_reports_false() {
    let (_robot, factory) = fakes(FakeRobotConn::new());
    let registry = registry(&factory);

    assert!(!within(registry.disconnect(&Esn::new(ESN_A))).await);
    assert_eq!(factory.connect_count(), 0);
}
