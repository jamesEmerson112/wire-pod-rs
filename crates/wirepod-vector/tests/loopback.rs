//! The tonic client against a real gRPC server on loopback.
//!
//! Every test here drives `TonicConnFactory` and `TonicRobotConn` over an
//! actual HTTP/2 connection to `test_support::spawn_fake_robot`, so the codec,
//! the metadata path and the streaming adapters are all exercised for real.
//! The fake binds `127.0.0.1:0`, so no firewall prompt appears.
//!
//! Every test wraps its body in a real-clock `tokio::time::timeout`. Nothing
//! here pauses the clock: the ceiling exists to turn a regression into a
//! failure instead of a hung CI job.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use wirepod_core::robot::events::{EVENT_CONNECTION_ID, EVENT_WHITELIST, run_event_stream};
use wirepod_core::robot::session::{EventOwner, StimSample};
use wirepod_core::{
    ConnTarget, Esn, EventItem, EventLoopExit, ProtocolResult, RobotConn, RobotConnFactory,
    StatusCode, StimEvent,
};
use wirepod_vector::test_support::{FakeRobotHandle, spawn_fake_robot};
use wirepod_vector::{TonicConnFactory, plaintext_builder};

/// The ceiling every test runs under.
const CEILING: Duration = Duration::from_secs(5);

/// The GUID the tests authenticate with. Never a real one.
const GUID: &str = "<guid>";

/// Spawns the fake and connects to it through the production factory.
async fn connect() -> (Arc<dyn RobotConn>, FakeRobotHandle) {
    let (addr, handle) = spawn_fake_robot().await;
    let factory = TonicConnFactory::with_endpoint_builder(plaintext_builder());
    let target = ConnTarget {
        esn: Esn::new("00303F28"),
        ip: addr.to_string(),
        guid: GUID.to_owned(),
    };
    let conn = factory.connect(&target).await.expect("dial the fake robot");
    (conn, handle)
}

#[tokio::test]
async fn connect_issues_no_rpc() {
    tokio::time::timeout(CEILING, async {
        let (_conn, handle) = connect().await;
        assert_eq!(handle.methods(), Vec::<&str>::new());
        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn every_call_carries_the_bearer_credential() {
    tokio::time::timeout(CEILING, async {
        let (conn, handle) = connect().await;

        conn.battery_state().await.expect("battery state");
        conn.protocol_version(5, 0).await.expect("protocol version");
        conn.enable_image_streaming(true)
            .await
            .expect("enable image streaming");
        conn.open_event_stream(EVENT_WHITELIST, EVENT_CONNECTION_ID)
            .await
            .expect("event stream");
        conn.open_camera_feed().await.expect("camera feed");

        let calls = handle.calls();
        assert_eq!(
            calls.iter().map(|call| call.method).collect::<Vec<_>>(),
            vec![
                "BatteryState",
                "ProtocolVersion",
                "EnableImageStreaming",
                "EventStream",
                "CameraFeed",
            ]
        );
        for call in &calls {
            assert_eq!(
                call.authorization.as_deref(),
                Some("Bearer <guid>"),
                "{} carried the wrong credential",
                call.method
            );
        }
        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn protocol_version_sends_the_net_probe_fields_and_maps_the_verdict() {
    tokio::time::timeout(CEILING, async {
        let (conn, handle) = connect().await;

        // The default verdict is UNSUPPORTED, which `net_probe` never reads.
        let verdict = conn.protocol_version(5, 0).await.expect("protocol version");
        assert_eq!(verdict.result, ProtocolResult::Unsupported);
        assert_eq!(verdict.host_version, 0);

        let request = handle.last_protocol_request().expect("request recorded");
        assert_eq!(request.client_version, 5);
        assert_eq!(request.min_host_version, 0);

        handle.set_protocol_verdict(ProtocolResult::Success, 7);
        let verdict = conn.protocol_version(5, 0).await.expect("protocol version");
        assert_eq!(verdict.result, ProtocolResult::Success);
        assert_eq!(verdict.host_version, 7);

        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn a_failed_call_renders_the_way_grpc_go_prints_it() {
    tokio::time::timeout(CEILING, async {
        let (conn, handle) = connect().await;
        handle.fail_battery(tonic::Status::unavailable("robot is asleep"));

        let err = conn.battery_state().await.expect_err("scripted failure");
        assert_eq!(err.code, StatusCode::Unavailable);
        assert_eq!(err.desc, "robot is asleep");
        assert_eq!(
            err.to_string(),
            "rpc error: code = Unavailable desc = robot is asleep"
        );

        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn battery_state_reads_the_level_and_the_voltage() {
    tokio::time::timeout(CEILING, async {
        let (conn, handle) = connect().await;
        handle.set_battery(2, 4.05);

        let reading = conn.battery_state().await.expect("battery state");
        assert_eq!(reading.level, wirepod_core::BatteryLevel::Nominal);
        assert!((reading.volts - 4.05).abs() < f32::EPSILON, "{reading:?}");

        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn the_event_stream_request_is_the_stim_shape() {
    tokio::time::timeout(CEILING, async {
        let (conn, handle) = connect().await;
        let receiver = conn
            .open_event_stream(EVENT_WHITELIST, EVENT_CONNECTION_ID)
            .await
            .expect("event stream");

        let request = handle.last_event_request().expect("request recorded");
        assert_eq!(request.whitelist, Some(vec!["stimulation_info".to_owned()]));
        assert_eq!(request.connection_id, "wirepod");

        // A graceful shutdown drains open streams, so let this one go first.
        drop(receiver);
        handle.end_event_stream();
        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn the_event_adapter_classifies_and_ends() {
    tokio::time::timeout(CEILING, async {
        let (conn, handle) = connect().await;
        let mut receiver = conn
            .open_event_stream(EVENT_WHITELIST, EVENT_CONNECTION_ID)
            .await
            .expect("event stream");

        handle.push_stim(0.5, 1.25);
        handle.push_other_event();
        handle.end_event_stream();

        assert_eq!(
            receiver.next().await.expect("first event"),
            Some(EventItem::Stim(StimEvent {
                value: 0.5,
                velocity: 1.25
            }))
        );
        assert_eq!(
            receiver.next().await.expect("second event"),
            Some(EventItem::Other)
        );
        assert_eq!(receiver.next().await.expect("clean end"), None);

        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn the_camera_adapter_yields_the_bytes_the_robot_sent() {
    tokio::time::timeout(CEILING, async {
        let (conn, handle) = connect().await;
        let mut frames = conn.open_camera_feed().await.expect("camera feed");

        handle.push_frame(vec![0xff, 0xd8, 0xff]);
        handle.push_frame(vec![1, 2, 3, 4]);
        handle.end_camera_feed();

        assert_eq!(
            frames.next().await.expect("first frame").map(|f| f.data),
            Some(vec![0xff, 0xd8, 0xff])
        );
        assert_eq!(
            frames.next().await.expect("second frame").map(|f| f.data),
            Some(vec![1, 2, 3, 4])
        );
        assert_eq!(frames.next().await.expect("clean end"), None);

        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn enable_image_streaming_reaches_the_robot() {
    tokio::time::timeout(CEILING, async {
        let (conn, handle) = connect().await;

        conn.enable_image_streaming(true).await.expect("on");
        conn.enable_image_streaming(false).await.expect("off");
        assert_eq!(handle.image_streaming_calls(), vec![true, false]);

        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

/// C5's stim loop driven over C9's real adapter, which is the only place the
/// two halves of the seam meet before the server crate exists.
#[tokio::test]
async fn the_stim_loop_runs_over_the_real_adapter() {
    tokio::time::timeout(CEILING, async {
        let (conn, handle) = connect().await;
        let receiver = conn
            .open_event_stream(EVENT_WHITELIST, EVENT_CONNECTION_ID)
            .await
            .expect("event stream");

        let owner = Arc::new(EventOwner::new());
        let cancel = CancellationToken::new();
        let generation = owner.claim(cancel.clone()).expect("nobody owns the stream");

        // A zero velocity is not a reading: Go decides presence with
        // `strings.Contains(fmt.Sprint(stimInfo), "velocity")` and proto3 omits
        // a zero scalar from the text form (`server.go:656-659`).
        handle.push_stim(0.9, 0.0);
        handle.push_stim(0.42, 2.0);
        handle.end_event_stream();

        let exit = run_event_stream(receiver, Arc::clone(&owner), generation, cancel).await;
        assert_eq!(exit, EventLoopExit::StreamEnded);
        assert_eq!(owner.stim(), StimSample::new(0.42, 2.0));

        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}
