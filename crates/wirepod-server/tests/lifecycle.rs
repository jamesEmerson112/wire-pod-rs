//! `/api-sdk/stop_cam_stream`, `/api-sdk/disconnect`, and the idle-timer
//! asymmetry the whole prefix carries.
//!
//! The two routes are the only ones in the slice whose whole point is a side
//! effect, so almost nothing here is asserted from a body. What is asserted is
//! what the camera owner, the registry and the dial seam look like afterwards,
//! and, for the disconnect, that the body arrives only once the settle has been
//! paid.
//!
//! Every timing is injected and every idle reading comes from a `ManualClock`,
//! so no test waits on a real clock beyond one 150 millisecond settle. The
//! ceilings around the spawned request are real-clock bounds that only fire on
//! a regression; nothing pauses time.
//!
//! Four live Go responses stand behind this file, taken from the production
//! server on port 8080 with the robot connected. `stop_cam_stream` answers 200
//! `done` with `Content-Length: 4` in 0.002 seconds. `disconnect` answers the
//! same body in 3.002 seconds. A `conn_test` straight after answers `success`
//! having dialled again. And `disconnect?serial=deadbeef` answers 200 with the
//! 46 byte doubled preamble error.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use http::{Method, StatusCode, header};
use tokio_util::sync::CancellationToken;
use wirepod_core::test_support::{FakeConnFactory, FakeRobotConn, RobotCall};
use wirepod_core::{
    AppState, Clock, Esn, ManualClock, RobotConn, RobotConnFactory, RobotEntry, Timings,
};
use wirepod_server::sdkapp::cam::CAM_STREAM_PATH;
use wirepod_server::test_support::{
    CACHE_HEADERS, CORS_HEADERS, Reply, TestServer, one_robot, request, send_to,
};
use wirepod_server::{SLICE_ROUTES, literals, router, sdkapp};

/// The serial in the fixture, as the dashboard sends it.
const SERIAL: &str = "00303f28";

/// Nothing here waits on more than one 150 millisecond settle, so this only
/// ever fires on a regression.
const CEILING: Duration = Duration::from_secs(2);

/// Long enough for a spawned request to have been polled and to have reached
/// the settle it is supposed to be parked in.
const PARK_WINDOW: Duration = Duration::from_millis(50);

/// The eight slice routes the preamble is not exempt from.
///
/// This is [`SLICE_ROUTES`] minus `get_sdk_info` and `debug`, and
/// `every_slice_route_is_either_exempt_or_touches` is what keeps the two lists
/// from drifting apart.
const TOUCHING_ROUTES: [&str; 8] = [
    "conn_test",
    "net_probe",
    "begin_event_stream",
    "stop_event_stream",
    "get_stim_status",
    "begin_cam_stream",
    "stop_cam_stream",
    "disconnect",
];

/// A router over fakes, with the timings and the two handles a test reads.
///
/// `wirepod_server::test_support::TestServer` builds the same thing with the Go
/// default timings and hands back no robot. Both are needed here: the preamble
/// tests want the defaults, and these two routes want a settle a test can
/// choose and a robot whose camera calls can be read back.
struct Fixture {
    router: Router,
    state: Arc<AppState>,
    clock: Arc<ManualClock>,
    factory: Arc<FakeConnFactory>,
    robot: Arc<FakeRobotConn>,
}

impl Fixture {
    /// A fixture whose robot answers every call, waiting on `timings`.
    fn new(timings: Timings) -> Self {
        let robot = Arc::new(FakeRobotConn::new());
        let conn: Arc<dyn RobotConn> = Arc::clone(&robot) as Arc<dyn RobotConn>;
        let factory = Arc::new(FakeConnFactory::connecting_to(conn));
        let clock = Arc::new(ManualClock::new());
        let dialler: Arc<dyn RobotConnFactory> = Arc::clone(&factory) as Arc<dyn RobotConnFactory>;
        let state = AppState::builder(dialler)
            .bot_info(one_robot())
            .timings(timings)
            .clock(Arc::clone(&clock) as Arc<dyn Clock>)
            .build();
        Self {
            router: router::build_router(Arc::clone(&state)),
            state,
            clock,
            factory,
            robot,
        }
    }

    /// A fixture that pays no settle anywhere, but whose camera switch still
    /// has a real deadline, so a disable that is issued is not lost to a zero
    /// timeout.
    fn instant() -> Self {
        Self::new(Timings {
            enable: Duration::from_secs(5),
            ..Timings::instant()
        })
    }

    /// The connected robot, dialled through the registry rather than through a
    /// request, so the idle timer carries only the touch the registry stamps at
    /// insert time.
    async fn connect(&self) -> Arc<RobotEntry> {
        self.state
            .get_robot(&esn())
            .await
            .expect("the fixture robot failed to connect")
    }

    /// The dashboard's own request shape: a POST declaring the urlencoded
    /// content type, with every parameter in the query.
    async fn post(&self, uri: &str) -> Reply {
        send_to(&self.router, request(Method::POST, uri, Some(""))).await
    }

    async fn get(&self, uri: &str) -> Reply {
        send_to(&self.router, request(Method::GET, uri, None)).await
    }

    /// Every `enable_image_streaming` the robot recorded, in order, with the
    /// other calls filtered out.
    fn camera_calls(&self) -> Vec<bool> {
        self.robot
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                RobotCall::EnableImageStreaming(on) => Some(on),
                _ => None,
            })
            .collect()
    }
}

fn esn() -> Esn {
    Esn::new(SERIAL)
}

fn route(name: &str) -> String {
    format!("/api-sdk/{name}?serial={SERIAL}")
}

async fn within<F: std::future::Future>(operation: F) -> F::Output {
    tokio::time::timeout(CEILING, operation)
        .await
        .expect("the operation did not finish inside the ceiling")
}

#[test]
fn every_slice_route_is_either_exempt_or_touches() {
    for name in SLICE_ROUTES {
        let path = format!("/api-sdk/{name}");
        assert_eq!(
            sdkapp::is_preamble_exempt(&path),
            !TOUCHING_ROUTES.contains(&name),
            "{name} is on both lists or on neither"
        );
    }
    // The exemption is the whole of the asymmetry inside the prefix: two paths
    // out of ten, which Go names as literals rather than by any property they
    // share (`server.go:60`).
    assert_eq!(TOUCHING_ROUTES.len(), SLICE_ROUTES.len() - 2);
}

#[tokio::test]
async fn stop_cam_stream_cancels_the_owner_and_leaves_it_to_finish() {
    let fixture = Fixture::instant();
    let entry = fixture.connect().await;

    // Stand in for a `/cam-stream` handler holding the feed. The claim is taken
    // on the owner directly rather than through `start_cam_stream`, because what
    // this route has to leave alone is the ownership entry, not the RPC.
    let cancel = CancellationToken::new();
    let (generation, displaced) = entry.session.cam.claim(cancel.clone());
    assert!(
        displaced.is_none(),
        "the fixture camera was already claimed"
    );
    assert!(entry.session.cam.is_streaming());

    let reply = fixture.post(&route("stop_cam_stream")).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, literals::DONE);
    // The live Go server answers `Content-Length: 4` for exactly this request.
    assert_eq!(reply.body.len(), 4);
    assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_TEXT));
    reply.assert_absent(&CORS_HEADERS, "stop_cam_stream");
    reply.assert_absent(&CACHE_HEADERS, "stop_cam_stream");

    // Clearing the flag alone would not end anything, because a handler samples
    // it only after a receive returns and a docked robot returns none. The
    // cancel is what stops it (`robot.go:133-144`).
    assert!(
        cancel.is_cancelled(),
        "stop_cam_stream cleared the flag and left the handler parked in its receive"
    );
    assert!(!entry.session.cam.is_streaming());

    // And the ownership entry survives, which is the half that is easy to get
    // wrong: `stopCamStream` deliberately does not delete it, so the departing
    // handler's own release still has something to release and still issues the
    // disable under the camera operation lock.
    assert_eq!(
        entry.session.cam.current(),
        Some(generation),
        "stop_cam_stream took the claim away from the handler that owns it"
    );
    assert!(
        entry.session.cam.release(generation),
        "the owner could no longer finish after a stop"
    );
    assert_eq!(entry.session.cam.current(), None);

    // The route itself sends nothing to the robot. The disable belongs to the
    // handler that was cancelled.
    assert!(fixture.camera_calls().is_empty());
}

#[tokio::test]
async fn stop_cam_stream_while_nobody_streams_is_a_harmless_done() {
    let fixture = Fixture::instant();
    let entry = fixture.connect().await;
    assert_eq!(entry.session.cam.current(), None);

    // Go's `stopCamStream` is a map lookup that finds nothing, and its arm
    // prints `done` either way, so the dashboard's stop button is safe to press
    // twice.
    for attempt in 0..2 {
        let reply = fixture.post(&route("stop_cam_stream")).await;
        assert_eq!(reply.status, StatusCode::OK, "attempt {attempt}");
        assert_eq!(reply.body, literals::DONE, "attempt {attempt}");
    }
    assert_eq!(entry.session.cam.current(), None);
    assert!(!entry.session.cam.is_streaming());

    // Nothing reached the robot but the connect-time liveness call, and the
    // entry is still cached.
    assert_eq!(fixture.robot.calls(), vec![RobotCall::BatteryState]);
    assert_eq!(fixture.factory.connect_count(), 1);
    assert!(fixture.state.registry().peek(&esn()).is_some());
}

#[tokio::test]
async fn disconnect_answers_done_and_drops_the_entry() {
    let fixture = Fixture::instant();
    fixture.connect().await;
    assert_eq!(fixture.state.registry().len(), 1);

    let reply = fixture.post(&route("disconnect")).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, literals::DONE);
    assert_eq!(reply.body.len(), 4);
    assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_TEXT));
    reply.assert_absent(&CORS_HEADERS, "disconnect");
    reply.assert_absent(&CACHE_HEADERS, "disconnect");

    assert!(
        fixture.state.registry().peek(&esn()).is_none(),
        "the entry survived a disconnect"
    );
    assert!(fixture.state.registry().is_empty());

    // `removeRobot` returns nothing, so a disconnect of a robot that is already
    // gone is the same `done`. It reconnects first, because the preamble runs
    // for every path.
    let reply = fixture.post(&route("disconnect")).await;
    assert_eq!(reply.body, literals::DONE);
    assert_eq!(fixture.factory.connect_count(), 2);
}

#[tokio::test]
async fn disconnect_stops_a_live_camera_and_turns_it_off() {
    let fixture = Fixture::instant();
    let entry = fixture.connect().await;

    let cancel = CancellationToken::new();
    let (generation, _) = entry.session.cam.claim(cancel.clone());

    let reply = fixture.post(&route("disconnect")).await;
    assert_eq!(reply.body, literals::DONE);

    assert!(
        cancel.is_cancelled(),
        "disconnect left the camera handler parked in its receive"
    );
    assert!(!entry.session.cam.is_streaming());
    // Deviation 16. Go leaves this to the departing handler and so turns the
    // camera off only while that handler is still alive to do it. The registry
    // issues one best-effort disable after the settle, for the owner it stopped
    // and only while that owner still holds the feed.
    assert_eq!(
        fixture.camera_calls(),
        vec![false],
        "disconnect left a claimed camera on"
    );
    assert_eq!(
        entry.session.cam.current(),
        Some(generation),
        "disconnect released a claim that `stopCamStream` leaves in place"
    );
    assert!(fixture.state.registry().peek(&esn()).is_none());
}

#[tokio::test]
async fn disconnect_answers_only_once_the_settle_is_paid() {
    let fixture = Fixture::new(Timings {
        disconnect_settle: Duration::from_millis(150),
        enable: Duration::from_secs(5),
        ..Timings::instant()
    });
    fixture.connect().await;

    // Go writes `done` after `removeRobot` returns, and `removeRobot` sleeps
    // three seconds for every matched robot, so the request blocks first and
    // answers second. The live server takes 3.00 seconds over this route.
    let router = fixture.router.clone();
    let mut answering = tokio::spawn(async move {
        send_to(
            &router,
            request(Method::POST, &route("disconnect"), Some("")),
        )
        .await
    });
    assert!(
        tokio::time::timeout(PARK_WINDOW, &mut answering)
            .await
            .is_err(),
        "disconnect answered without paying the settle"
    );
    let reply = within(answering)
        .await
        .expect("the disconnect request panicked");
    assert_eq!(reply.body, literals::DONE);
    assert!(fixture.state.registry().is_empty());
}

#[tokio::test]
async fn disconnect_then_conn_test_dials_a_fresh_connection() {
    let fixture = Fixture::instant();

    let reply = fixture.post(&route("conn_test")).await;
    assert_eq!(reply.body, literals::SUCCESS);
    let before = fixture
        .state
        .registry()
        .peek(&esn())
        .expect("robot connected");
    assert_eq!(fixture.factory.connect_count(), 1);

    assert_eq!(
        fixture.post(&route("disconnect")).await.body,
        literals::DONE
    );

    // This is the live sequence: the Go server answers `success` on the next
    // request, having dialled again, which is what makes the dashboard's
    // disconnect button safe to press.
    let reply = fixture.post(&route("conn_test")).await;
    assert_eq!(reply.body, literals::SUCCESS);
    assert_eq!(
        fixture.factory.connect_count(),
        2,
        "the reconnect reused a dropped entry"
    );
    let after = fixture
        .state
        .registry()
        .peek(&esn())
        .expect("robot reconnected");
    assert!(
        !Arc::ptr_eq(&before, &after),
        "the reconnect handed back the entry the disconnect removed"
    );
}

#[tokio::test]
async fn disconnect_of_an_unknown_serial_answers_the_doubled_preamble_error() {
    let server = TestServer::connected(one_robot());

    let reply = server
        .post(&format!("/api-sdk/conn_test?serial={SERIAL}"))
        .await;
    assert_eq!(reply.body, literals::SUCCESS);
    let entry = server
        .state
        .registry()
        .peek(&esn())
        .expect("robot connected");
    server.clock.advance_secs(11);

    // `deadbeef` is the serial the live probe used. The preamble resolves it
    // against the bot-info file, finds nothing, writes the doubled error and
    // returns, so the arm never runs.
    let reply = server.post("/api-sdk/disconnect?serial=deadbeef").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, literals::ROBOT_NOT_FOUND);
    assert_eq!(reply.body.len(), 46);
    assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_TEXT));

    // Nothing was disconnected, nothing was dialled, and the connected robot's
    // idle timer did not move.
    assert_eq!(server.state.registry().len(), 1);
    assert_eq!(entry.last_touch(), Duration::ZERO);
    assert_eq!(server.factory.connect_count(), 1);
}

#[tokio::test]
async fn every_route_but_the_two_exempt_ones_resets_the_idle_timer() {
    // Asserted from the timer alone, never from a body, because four of these
    // eight arms are still stubs. The reset is the preamble's
    // (`server.go:65`), so it belongs to none of them and the assertion
    // survives an arm being filled in.
    for name in TOUCHING_ROUTES {
        let fixture = Fixture::instant();
        let entry = fixture.connect().await;
        assert_eq!(entry.last_touch(), Duration::ZERO, "{name}");

        fixture.clock.advance_secs(11);
        fixture.post(&route(name)).await;
        assert_eq!(
            entry.last_touch(),
            Duration::from_secs(11),
            "{name} did not run `robots[robotIndex].ConnTimer = 0`"
        );
    }

    // The other two are exempt from the reset as well as from the error write,
    // and an unknown path under the prefix is exempt from neither.
    let fixture = Fixture::instant();
    let entry = fixture.connect().await;
    fixture.clock.advance_secs(11);
    for name in ["get_sdk_info", "debug"] {
        fixture.post(&route(name)).await;
        assert_eq!(entry.last_touch(), Duration::ZERO, "{name} is exempt");
    }
    fixture.post(&route("does_not_exist")).await;
    assert_eq!(
        entry.last_touch(),
        Duration::from_secs(11),
        "an unknown path under the prefix still pays the preamble in full"
    );
}

#[tokio::test]
async fn a_request_whose_preamble_fails_touches_nothing() {
    let fixture = Fixture::instant();
    let entry = fixture.connect().await;
    fixture.clock.advance_secs(11);

    // A serial the bot-info file does not carry never reaches the timer reset,
    // and cannot reach another robot's either.
    for name in TOUCHING_ROUTES {
        let reply = fixture
            .post(&format!("/api-sdk/{name}?serial=nosuchbot"))
            .await;
        assert_eq!(reply.body, literals::ROBOT_NOT_FOUND, "{name}");
        assert_eq!(entry.last_touch(), Duration::ZERO, "{name}");
    }
    assert_eq!(fixture.state.registry().len(), 1);
    assert_eq!(
        fixture.factory.connect_count(),
        1,
        "an unknown serial resolves against the file before anything is dialled"
    );

    // The other half of a failing preamble: a serial the file does carry, whose
    // dial then fails. Nothing is cached, so there is nothing to touch, and the
    // next request dials again rather than reusing a dead connection.
    let server = TestServer::unreachable(one_robot());
    let reply = server.post(&route("disconnect")).await;
    assert!(reply.body.starts_with(literals::ERROR_PREFIX));
    assert!(
        reply
            .body
            .starts_with("error: rpc error: code = Unavailable")
    );
    assert!(server.state.registry().is_empty());
    server.post(&route("stop_cam_stream")).await;
    assert_eq!(server.factory.connect_count(), 2);
}

/// The idle-timer asymmetry outside the prefix.
///
/// Go's `camStreamHandler` runs its own preamble and throws the robot index
/// away (`server.go:709`), so `/cam-stream` never writes
/// `robots[robotIndex].ConnTimer = 0`, and a page showing only the camera is
/// dropped after 300 seconds while frames are still flowing. The route is P4
/// work; this pins the rule while it is still absent, so that whatever lands
/// keeps it deliberately rather than by accident.
#[tokio::test]
async fn the_cam_stream_route_is_outside_the_prefix_and_touches_nothing() {
    // The path is not under `/api-sdk/`, so nothing the shared preamble does
    // can reach it. That is the structural half of the rule.
    assert_eq!(CAM_STREAM_PATH, "/cam-stream");
    assert!(!CAM_STREAM_PATH.starts_with(sdkapp::PREFIX));
    assert!(!sdkapp::is_slice_route("cam-stream"));

    let fixture = Fixture::instant();
    let entry = fixture.connect().await;
    fixture.clock.advance_secs(11);

    // Today it reaches the router fallback, which is the file server's 404.
    let reply = fixture
        .get(&format!("{CAM_STREAM_PATH}?serial={SERIAL}"))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body, literals::FILE_NOT_FOUND);
    assert_eq!(
        reply.header(header::X_CONTENT_TYPE_OPTIONS),
        Some(literals::NOSNIFF)
    );

    assert_eq!(
        entry.last_touch(),
        Duration::ZERO,
        "/cam-stream reset the idle timer"
    );
    assert_eq!(fixture.factory.connect_count(), 1);
}
