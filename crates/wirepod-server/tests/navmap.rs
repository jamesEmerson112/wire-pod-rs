//! The nav map page and its snapshot, through the router.
//!
//! The robot is `FakeRobotConn` with a nav map feed added: every call it does
//! not override goes to the fake, and each `open_nav_map_feed` takes the next
//! scripted stream and is counted.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tonic::async_trait;
use wirepod_core::robot::conn::NavMapReceiver;
use wirepod_core::robot::navmap::{NavMapFrame, NavMapInfo, NavMapQuad};
use wirepod_core::robot::observe::ReceivedMap;
use wirepod_core::test_support::FakeRobotConn;
use wirepod_core::{
    BatteryReading, CameraControl, ConnError, Esn, EventReceiver, FrameStream, JdocKind, NamedJdoc,
    ProtocolVerdict, RobotConn, RobotEntry, RobotStateSample, StatusCode,
};
use wirepod_server::test_support::{Reply, TEST_ESN, TestServer, no_robots, one_robot, wait_until};

type Script = mpsc::UnboundedSender<Result<NavMapFrame, ConnError>>;

struct ScriptedReceiver(mpsc::UnboundedReceiver<Result<NavMapFrame, ConnError>>);

#[async_trait]
impl NavMapReceiver for ScriptedReceiver {
    async fn next(&mut self) -> Result<Option<NavMapFrame>, ConnError> {
        self.0.recv().await.transpose()
    }
}

/// A fake robot with a nav map feed.
struct MapConn {
    inner: FakeRobotConn,
    feeds: Mutex<Vec<Box<dyn NavMapReceiver>>>,
    opens: AtomicUsize,
}

impl MapConn {
    /// A robot with one scripted feed, and the handle that feeds it.
    fn with_feed() -> (Arc<Self>, Script) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let conn = Arc::new(Self {
            inner: FakeRobotConn::new(),
            feeds: Mutex::new(vec![Box::new(ScriptedReceiver(receiver))]),
            opens: AtomicUsize::new(0),
        });
        (conn, sender)
    }

    fn opens(&self) -> usize {
        self.opens.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl CameraControl for MapConn {
    async fn enable_image_streaming(&self, on: bool) -> Result<(), ConnError> {
        self.inner.enable_image_streaming(on).await
    }
}

#[async_trait]
impl RobotConn for MapConn {
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
        let mut feeds = self.feeds.lock().expect("feeds");
        if feeds.is_empty() {
            return Err(ConnError::new(StatusCode::Unavailable, "no feed scripted"));
        }
        Ok(feeds.remove(0))
    }
}

/// A root split once, in origin 7.
fn split_root() -> NavMapFrame {
    NavMapFrame {
        origin_id: 7,
        info: NavMapInfo {
            root_depth: 1,
            root_size_mm: 512.0,
            root_center_x: 64.0,
            root_center_y: 0.0,
        },
        quads: [
            (1, 0xff00_00ff),
            (1, 0xff00_00ff),
            (4, 0x00ff_ffff),
            (7, 0x0000_00ff),
        ]
        .into_iter()
        .map(|(content, rgba)| NavMapQuad {
            content,
            depth: 0,
            rgba,
        })
        .collect(),
    }
}

fn parse(reply: &Reply) -> Value {
    assert_eq!(reply.status, http::StatusCode::OK);
    assert_eq!(reply.content_type(), Some("application/json"));
    serde_json::from_str(&reply.body).expect("the snapshot is JSON")
}

fn entry(server: &TestServer) -> Arc<RobotEntry> {
    server
        .state
        .registry()
        .peek(&Esn::new(TEST_ESN))
        .expect("the snapshot connected the robot")
}

const SNAPSHOT: &str = "/api-navmap/snapshot?serial=00303f28";

#[tokio::test]
async fn the_page_is_served_as_html() {
    let server = TestServer::connected(one_robot());
    let reply = server.get("/navmap?serial=00303f28").await;
    assert_eq!(reply.status, http::StatusCode::OK);
    assert_eq!(reply.content_type(), Some("text/html; charset=utf-8"));
    assert_eq!(reply.body, include_str!("../src/navmap/navmap.html"));
}

#[tokio::test]
async fn an_unknown_serial_answers_the_error_as_its_status() {
    let server = TestServer::connected(no_robots());
    for (uri, serial) in [
        ("/api-navmap/snapshot?serial=deadbeef", "deadbeef"),
        ("/api-navmap/snapshot", ""),
    ] {
        let body = parse(&server.get(uri).await);
        assert_eq!(
            body,
            json!({
                "serial": serial,
                "status": "error: robot not found in SDK info file",
                "map": null,
                "robot": null,
            }),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn a_snapshot_starts_the_feed_once_and_answers_the_placed_map() {
    let (conn, script) = MapConn::with_feed();
    let server = TestServer::with_robot(one_robot(), Arc::clone(&conn) as Arc<dyn RobotConn>);

    let first = parse(&server.get(SNAPSHOT).await);
    assert_eq!(first["serial"], TEST_ESN);
    assert_eq!(first["status"], "starting");
    assert_eq!(first["map"], Value::Null);
    assert_eq!(first["robot"], Value::Null);

    let robot = entry(&server);
    script
        .send(Ok(split_root()))
        .expect("the feed is listening");
    wait_until("the map to arrive", || {
        robot.session.map_feed.latest().is_some()
    })
    .await;

    // Nothing in this crate starts the state stream, so its slot is claimed
    // here to stand in for it.
    let state = &robot.session.state_stream;
    let generation = state
        .claim(CancellationToken::new())
        .expect("the state slot is free");
    assert!(state.write(
        generation,
        RobotStateSample {
            status: 0x1 | 0x8000,
            x_mm: 12.5,
            y_mm: -3.0,
            angle_rad: 0.125,
            origin_id: 7,
            localized_to_object_id: 0,
        },
    ));

    let second = parse(&server.get(SNAPSHOT).await);
    assert_eq!(second["status"], "streaming");
    let map = &second["map"];
    assert_eq!(map["origin_id"], 7);
    assert_eq!(
        map["root"],
        json!({ "cx": 64.0, "cy": 0.0, "size_mm": 512.0, "depth": 1 })
    );
    assert_eq!(
        map["quads"],
        json!([
            [192.0, 128.0, 256.0, 1, 0xff00_00ff_u32],
            [192.0, -128.0, 256.0, 1, 0xff00_00ff_u32],
            [-64.0, 128.0, 256.0, 4, 0x00ff_ffff_u32],
            [-64.0, -128.0, 256.0, 7, 0x0000_00ff_u32],
        ])
    );
    assert_eq!(
        map["counts"],
        json!({
            "unknown": 0, "clear_of_obstacle": 2, "clear_of_cliff": 0,
            "obstacle_cube": 0, "obstacle_proximity": 1,
            "obstacle_proximity_explored": 0, "obstacle_unrecognized": 0,
            "cliff": 1, "interesting_edge": 0, "non_interesting_edge": 0,
        })
    );
    assert!(map["received_ms"].as_i64().is_some_and(|ms| ms > 0));
    assert_eq!(
        second["robot"],
        json!({
            "x": 12.5, "y": -3.0, "angle": 0.125, "origin_id": 7,
            "localized_to": 0, "flags": ["moving", "wheels_moving"],
        })
    );

    assert_eq!(
        conn.opens(),
        1,
        "the second snapshot started no second feed"
    );
    assert!(robot.session.map_feed.is_running());
}

#[tokio::test]
async fn a_malformed_map_is_an_error_status_not_a_failure() {
    let server = TestServer::connected(one_robot());
    let robot = server
        .state
        .get_robot(&Esn::new(TEST_ESN))
        .await
        .expect("connect the robot");

    // The feed drops a malformed map before it reaches the slot, so one is
    // written here directly, under a claim that also keeps the snapshot from
    // starting a feed of its own. A root split once, with one child of four.
    let mut short = split_root();
    short.quads.truncate(1);
    let slot = &robot.session.map_feed;
    let generation = slot.claim(CancellationToken::new()).expect("free");
    assert!(slot.write(
        generation,
        ReceivedMap {
            frame: short,
            received_ms: 1,
        },
    ));

    let body = parse(&server.get(SNAPSHOT).await);
    assert_eq!(
        body["status"],
        "malformed nav map: 1 quads do not cover the root"
    );
    assert_eq!(body["map"], Value::Null);
}

#[tokio::test]
async fn a_restart_after_a_failure_reports_the_failure() {
    // No feed scripted, so every open fails.
    let conn = Arc::new(MapConn {
        inner: FakeRobotConn::new(),
        feeds: Mutex::new(Vec::new()),
        opens: AtomicUsize::new(0),
    });
    let server = TestServer::with_robot(one_robot(), Arc::clone(&conn) as Arc<dyn RobotConn>);
    assert_eq!(parse(&server.get(SNAPSHOT).await)["status"], "starting");

    let robot = entry(&server);
    wait_until("the open to fail", || {
        !robot.session.map_feed.is_running() && robot.session.map_feed.error().is_some()
    })
    .await;
    let body = parse(&server.get(SNAPSHOT).await);
    assert_eq!(
        body["status"],
        "rpc error: code = Unavailable desc = no feed scripted"
    );
    wait_until("the next poll to restart the feed", || conn.opens() == 2).await;
}
