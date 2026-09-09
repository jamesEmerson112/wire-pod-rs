//! The router against a real gRPC robot on loopback.
//!
//! Every other test in this crate fakes the seam: it hands the registry a
//! [`FakeRobotConn`](wirepod_core::test_support::FakeRobotConn) and asserts the
//! handler's body. That proves the handler and proves nothing about the client
//! underneath it. These two tests drive
//! [`build_router`](wirepod_server::build_router) with the production
//! [`TonicConnFactory`] against
//! [`spawn_fake_robot`](wirepod_vector::test_support::spawn_fake_robot), so the
//! whole path runs for real: axum's extraction, the connect preamble's dial and
//! liveness check, the tonic client, the codec, the metadata and the streaming
//! adapters.
//!
//! What is asserted here is deliberately narrow. That the probe request carries
//! `client_version = 5` and `min_host_version = 0`, that the event stream asks
//! for the `stimulation_info` whitelist under the `wirepod` connection id, and
//! that every call carries the bearer credential are pinned already by
//! `wirepod-vector`'s own loopback tests. What is new here is the HTTP body on
//! one end and the RPCs the robot actually saw on the other.
//!
//! The fake binds `127.0.0.1:0`, so this crate binds no listener of its own and
//! no firewall prompt appears. Every test runs under a real-clock ceiling;
//! nothing pauses the clock.

use std::sync::Arc;

use axum::Router;
use http::{Method, StatusCode};
use wirepod_core::{AppState, BotInfo, RobotConnFactory};
use wirepod_server::test_support::{CEILING, Reply, request, send_to};
use wirepod_server::{build_router, literals};
use wirepod_vector::test_support::{FakeRobotHandle, spawn_fake_robot};
use wirepod_vector::{TonicConnFactory, plaintext_builder};

/// The serial the fixtures use, which is this machine's robot.
const SERIAL: &str = "00303f28";

/// A router whose registry dials the fake, the handle that drives it, and the
/// `host:port` the probe body should report.
///
/// The address carries an ephemeral port, and the factory's authority rule
/// keeps a target that already has one rather than appending `:443`, which is
/// what lets the production dialling path reach a loopback fake unchanged.
async fn live_router() -> (Router, FakeRobotHandle, String) {
    let (addr, handle) = spawn_fake_robot().await;
    let target = addr.to_string();
    // The GUID is a placeholder. A real robot GUID never goes into a fixture,
    // a document or a log line.
    let bot_info: BotInfo = serde_json::from_str(&format!(
        concat!(
            r#"{{"global_guid":"global-guid-placeholder","robots":[{{"esn":"{esn}","#,
            r#""ip_address":"{ip}","guid":"robot-guid-placeholder","activated":true}}]}}"#
        ),
        esn = SERIAL,
        ip = target
    ))
    .expect("parse the loopback fixture");

    let factory: Arc<dyn RobotConnFactory> =
        Arc::new(TonicConnFactory::with_endpoint_builder(plaintext_builder()));
    let state = AppState::builder(factory).bot_info(bot_info).build();
    (build_router(state), handle, target)
}

/// Sends one GET through the router under test.
///
/// The dashboard fetches both `net_probe` and `get_stim_status` with a GET
/// (`vectorbrain.js:889-891`, `main.js:104`), so that is what these send.
async fn get(router: &Router, uri: &str) -> Reply {
    send_to(router, request(Method::GET, uri, None)).await
}

/// Polls `uri` until it answers `want`, or fails at [`CEILING`].
///
/// The receiver is a detached task, so the reading it publishes arrives on its
/// own schedule and a test that asserted straight after pushing an event would
/// be racing it.
async fn wait_for_body(router: &Router, uri: &str, want: &str) {
    tokio::time::timeout(CEILING, async {
        loop {
            if get(router, uri).await.body == want {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {uri} to answer {want}"));
}

/// Polls until the robot has answered `method`, or fails at [`CEILING`].
async fn wait_for_rpc(handle: &FakeRobotHandle, method: &str) {
    tokio::time::timeout(CEILING, async {
        while !handle.methods().contains(&method) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for the robot to answer {method}"));
}

#[tokio::test]
async fn net_probe_over_the_real_client_answers_the_documented_shape() {
    tokio::time::timeout(CEILING, async {
        let (router, handle, target) = live_router().await;

        let reply = get(&router, &format!("/api-sdk/net_probe?serial={SERIAL}")).await;
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_TEXT));
        assert!(!reply.body.ends_with('\n'));

        // The round trip is a real one, so the digits are whatever the loopback
        // took. Everything either side of them is fixed, and splitting at the
        // first comma is what lets the two halves be asserted separately.
        let (rtt, rest) = reply
            .body
            .split_once(',')
            .unwrap_or_else(|| panic!("the body has only one field: {}", reply.body));
        let rtt = rtt
            .strip_prefix(r#"{"rttMs":"#)
            .unwrap_or_else(|| panic!("rttMs is not the first key: {}", reply.body));
        // The page throws `bad probe` and counts the sample lost unless this is
        // a number (`vectorbrain.js:1057-1068`).
        let rtt: f64 = rtt
            .parse()
            .unwrap_or_else(|_| panic!("rttMs is not a number: {rtt}"));
        assert!(rtt >= 0.0, "a round trip cannot be negative: {rtt}");
        // `target` is Go's `robot.IPAddress + ":443"` (`robot.go:336`), a plain
        // concatenation with no inspection of what is already there. The
        // fixture's address carries the fake's ephemeral port, so the reported
        // string carries both, and reproducing that is the point: the dialling
        // path is the one that special-cases an address that already has a
        // port, and the reported field is not.
        assert_eq!(
            rest,
            format!(
                concat!(
                    r#""probe":"ProtocolVersion","target":"{target}:443","#,
                    r#""camBytes":0,"camFrames":0,"camOn":false}}"#
                ),
                target = target
            )
        );

        // The robot saw the connect preamble's liveness check and then exactly
        // one timed round trip. It answers UNSUPPORTED by default, which the
        // handler never reads.
        assert_eq!(handle.methods(), vec!["BatteryState", "ProtocolVersion"]);
        let sent = handle.last_protocol_request().expect("request recorded");
        assert_eq!(sent.client_version, 5);
        assert_eq!(sent.min_host_version, 0);

        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn the_stim_routes_drive_a_real_event_stream() {
    tokio::time::timeout(CEILING, async {
        let (router, handle, _target) = live_router().await;
        let status = format!("/api-sdk/get_stim_status?serial={SERIAL}");

        // Idle first, so the sentinel is an observed starting state rather than
        // an assumption.
        assert_eq!(
            get(&router, &status).await.body,
            literals::MUST_START_EVENT_STREAM
        );

        let reply = get(
            &router,
            &format!("/api-sdk/begin_event_stream?serial={SERIAL}"),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(reply.body, literals::DONE);

        // The request answered before the detached task had reached the robot,
        // which is the whole shape of this route: the robot recording the RPC
        // is what says the stream is really open.
        wait_for_rpc(&handle, "EventStream").await;
        let opened = handle.last_event_request().expect("request recorded");
        assert_eq!(opened.whitelist, Some(vec!["stimulation_info".to_owned()]));
        assert_eq!(opened.connection_id, "wirepod");

        // A real event, over a real stream, through the real streaming adapter,
        // formatted by Go's `%v` on a float32.
        handle.push_stim(0.75, 0.1);
        wait_for_body(&router, &status, "0.75").await;

        let reply = get(
            &router,
            &format!("/api-sdk/stop_event_stream?serial={SERIAL}"),
        )
        .await;
        assert_eq!(reply.body, literals::DONE);
        assert_eq!(
            get(&router, &status).await.body,
            literals::MUST_START_EVENT_STREAM
        );

        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}
