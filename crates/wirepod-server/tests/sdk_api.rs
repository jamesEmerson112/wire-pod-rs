//! `/api-sdk/*`: the preamble, the exemption, and the routes the slice serves.

use std::sync::Arc;
use std::time::Duration;

use http::{Method, StatusCode, header};
use tokio_util::sync::CancellationToken;
use wirepod_core::test_support::{FakeReceiver, FakeRobotConn, RobotCall};
use wirepod_core::{
    BotInfo, Esn, ProtocolResult, ProtocolVerdict, RobotConn, RobotEntry, StimSample, Timings,
};
use wirepod_server::test_support::{
    CACHE_HEADERS, CEILING, CORS_HEADERS, Reply, TestServer, no_robots, one_robot, request,
    send_to, unreachable_error, wait_until,
};
use wirepod_server::{SLICE_ROUTES, literals};

/// The serial in the fixtures, as the dashboard would send it.
const SERIAL: &str = "00303f28";

/// A bot-info file carrying keys this struct does not name, at both levels.
///
/// The on-disk struct preserves them for rollback safety; `get_sdk_info` must
/// not, because Go marshals a struct that has no room for them.
fn bot_info_with_extras() -> BotInfo {
    serde_json::from_str(concat!(
        r#"{"global_guid":"global-guid-placeholder","robots":[{"esn":"00303F28","#,
        r#""ip_address":"192.168.8.203","guid":"robot-guid-placeholder","activated":true,"#,
        r#""fork_only_field":42}],"fork_top_level":"kept"}"#
    ))
    .expect("parse the extras fixture")
}

#[test]
fn the_slice_covers_the_ten_routes_the_plan_names() {
    assert_eq!(
        SLICE_ROUTES,
        [
            "conn_test",
            "net_probe",
            "begin_event_stream",
            "stop_event_stream",
            "get_stim_status",
            "begin_cam_stream",
            "stop_cam_stream",
            "disconnect",
            "get_sdk_info",
            "debug",
        ]
    );
    assert!(wirepod_server::sdkapp::is_slice_route("conn_test"));
    assert!(!wirepod_server::sdkapp::is_slice_route("say_text"));
}

#[tokio::test]
async fn an_unknown_serial_produces_the_doubled_error_prefix() {
    let server = TestServer::connected(one_robot());

    let reply = server.post("/api-sdk/conn_test?serial=nosuchbot").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, literals::ROBOT_NOT_FOUND);
    assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_TEXT));
    // The doubling is what the dashboard shows, so it is worth saying twice.
    assert_eq!(reply.body.matches("error: ").count(), 2);
    // The preamble's error write is a bare `fmt.Fprint`, so this response
    // carries the sniffed content type and nothing else.
    reply.assert_absent(&CORS_HEADERS, "the preamble error");
    reply.assert_absent(&CACHE_HEADERS, "the preamble error");

    // `serial=null` is what the dashboard sends when its own `vbEsn` is null,
    // and `serial=` and an absent `serial` are the same empty string. All three
    // are ordinary serials that match nothing.
    for uri in [
        "/api-sdk/conn_test?serial=null",
        "/api-sdk/conn_test?serial=",
        "/api-sdk/conn_test",
    ] {
        let reply = server.post(uri).await;
        assert_eq!(reply.status, StatusCode::OK, "{uri}");
        assert_eq!(reply.body, literals::ROBOT_NOT_FOUND, "{uri}");
    }

    // No dial was attempted for any of them: the serial resolves against the
    // bot-info file first.
    assert_eq!(server.factory.connect_count(), 0);
}

#[tokio::test]
async fn a_failing_dial_reaches_the_body_as_the_grpc_status_text() {
    let server = TestServer::unreachable(one_robot());

    let reply = server
        .post(&format!("/api-sdk/conn_test?serial={SERIAL}"))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.body,
        format!("{}{}", literals::ERROR_PREFIX, unreachable_error())
    );
    assert!(
        reply
            .body
            .starts_with("error: rpc error: code = Unavailable desc = ")
    );
    assert_eq!(server.factory.connect_count(), 1);
}

#[tokio::test]
async fn the_preamble_runs_before_the_404() {
    let server = TestServer::connected(one_robot());

    // Go runs `getRobot` for every path under the prefix, including the ones
    // about to 404, so an unknown path with an unknown serial answers the
    // preamble error at 200 and never reaches the switch. Registering the
    // routes individually in axum would 404 first and lose this ordering.
    let reply = server
        .post("/api-sdk/does_not_exist?serial=nosuchbot")
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, literals::ROBOT_NOT_FOUND);
    // The live server answers `Content-Length: 46` for exactly this request.
    assert_eq!(reply.body.len(), 46);

    // Only with a serial that resolves does the same path reach the 404.
    let reply = server
        .post(&format!("/api-sdk/does_not_exist?serial={SERIAL}"))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body, literals::NOT_FOUND);
    assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_TEXT));
    assert_eq!(
        reply.header(header::X_CONTENT_TYPE_OPTIONS),
        Some(literals::NOSNIFF)
    );
}

#[tokio::test]
async fn debug_and_get_sdk_info_ignore_a_failing_dial() {
    let server = TestServer::unreachable(one_robot());

    // Exempt from the error write, so `debug` still reaches the 404 and
    // `get_sdk_info` still answers the file, even though the dial failed.
    let reply = server
        .post(&format!("/api-sdk/debug?serial={SERIAL}"))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body, literals::NOT_FOUND);

    let reply = server
        .post(&format!("/api-sdk/get_sdk_info?serial={SERIAL}"))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.starts_with('{'));

    // Exempt from the error write and from the timer reset, never from the dial
    // itself: `getRobot` runs for every path with no exception.
    assert_eq!(server.factory.connect_count(), 2);

    // A bad serial does not change the exemption either.
    let reply = server.post("/api-sdk/debug?serial=nosuchbot").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body, literals::NOT_FOUND);
}

#[tokio::test]
async fn debug_and_get_sdk_info_skip_the_idle_timer_reset() {
    let server = TestServer::connected(one_robot());
    let esn = Esn::new(SERIAL);

    // Connect the robot and take the baseline. The registry touches on
    // creation, and the preamble touches again, both at zero.
    let reply = server
        .post(&format!("/api-sdk/conn_test?serial={SERIAL}"))
        .await;
    assert_eq!(reply.body, literals::SUCCESS);
    let entry = server.state.registry().peek(&esn).expect("robot connected");
    assert_eq!(entry.last_touch(), std::time::Duration::ZERO);

    server.clock.advance_secs(7);
    server
        .post(&format!("/api-sdk/get_sdk_info?serial={SERIAL}"))
        .await;
    assert_eq!(
        entry.last_touch(),
        std::time::Duration::ZERO,
        "get_sdk_info is exempt from `robots[robotIndex].ConnTimer = 0`"
    );

    server
        .post(&format!("/api-sdk/debug?serial={SERIAL}"))
        .await;
    assert_eq!(
        entry.last_touch(),
        std::time::Duration::ZERO,
        "debug is exempt too"
    );

    // Every other path does reset it, which is the only thing anywhere that
    // keeps a robot out of the 300 second idle sweep.
    server
        .post(&format!("/api-sdk/conn_test?serial={SERIAL}"))
        .await;
    assert_eq!(entry.last_touch(), std::time::Duration::from_secs(7));
}

#[tokio::test]
async fn conn_test_and_begin_cam_stream_answer_their_literals() {
    let server = TestServer::connected(one_robot());

    let reply = server
        .post(&format!("/api-sdk/conn_test?serial={SERIAL}"))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, literals::SUCCESS);
    assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_TEXT));
    // Nothing wraps `/api-sdk/*`: the `sdkapp` copy of
    // `DisableCachingAndSniffing` wraps only the `/sdk-app` file server
    // (`server.go:811`) and `apiHandler`'s CORS headers are on the other
    // prefix, so a successful route here carries neither.
    reply.assert_absent(&CORS_HEADERS, "conn_test");
    reply.assert_absent(&CACHE_HEADERS, "conn_test");

    // The only statement in Go's arm is commented out, so the route is a no-op
    // that answers `done`; the camera is claimed by `/cam-stream` itself.
    let reply = server
        .post(&format!("/api-sdk/begin_cam_stream?serial={SERIAL}"))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, literals::DONE);

    // The second request reused the first connection.
    assert_eq!(server.factory.connect_count(), 1);
}

#[tokio::test]
async fn get_sdk_info_marshals_the_file_in_gos_key_order_and_drops_the_extras() {
    let server = TestServer::connected(bot_info_with_extras());

    let reply = server.post("/api-sdk/get_sdk_info").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.body,
        concat!(
            r#"{"global_guid":"global-guid-placeholder","robots":[{"esn":"00303F28","#,
            r#""ip_address":"192.168.8.203","guid":"robot-guid-placeholder","activated":true}]}"#
        )
    );
    // `fmt.Fprint` writes the marshalled bytes as they are, with no newline.
    assert!(!reply.body.ends_with('\n'));
    assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_TEXT));
    // The preserved keys stay on disk and stay out of the body.
    assert!(!reply.body.contains("fork_only_field"));
    assert!(!reply.body.contains("fork_top_level"));
}

#[tokio::test]
async fn get_sdk_info_is_a_500_with_a_newline_when_no_robot_is_authenticated() {
    let server = TestServer::connected(no_robots());

    let reply = server.post("/api-sdk/get_sdk_info").await;
    assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(reply.body, literals::NO_BOTS_AUTHENTICATED);
    assert!(reply.body.ends_with('\n'), "http.Error appends a newline");
    assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_TEXT));
    assert_eq!(
        reply.header(header::X_CONTENT_TYPE_OPTIONS),
        Some(literals::NOSNIFF)
    );
}

#[tokio::test]
async fn the_serial_comes_from_gos_form_value_merge() {
    let server = TestServer::connected(one_robot());

    // The dashboard's own shape: a POST declaring the urlencoded content type,
    // an empty body, and the serial in the query.
    let reply = server
        .post(&format!("/api-sdk/conn_test?serial={SERIAL}"))
        .await;
    assert_eq!(reply.body, literals::SUCCESS);

    // A body parameter shadows the same-named query parameter, because Go
    // merges the body first and `FormValue` returns the first value.
    let reply = server
        .post_form(
            "/api-sdk/conn_test?serial=nosuchbot",
            &format!("serial={SERIAL}"),
        )
        .await;
    assert_eq!(reply.body, literals::SUCCESS);

    // The shadowing holds even when the body's value is empty, which then
    // resolves to no robot at all.
    let reply = server
        .post_form(&format!("/api-sdk/conn_test?serial={SERIAL}"), "serial=")
        .await;
    assert_eq!(reply.body, literals::ROBOT_NOT_FOUND);

    // The dashboard's cache buster and any other unknown parameter are ignored.
    let reply = server
        .post(&format!(
            "/api-sdk/conn_test?serial={SERIAL}&_=1700000000000"
        ))
        .await;
    assert_eq!(reply.body, literals::SUCCESS);

    // A GET's body is never parsed, so the query still decides.
    let reply = server
        .get(&format!("/api-sdk/conn_test?serial={SERIAL}"))
        .await;
    assert_eq!(reply.body, literals::SUCCESS);
}

/// A server dialling a robot whose answers this test scripts.
fn scripted(conn: Arc<FakeRobotConn>) -> TestServer {
    TestServer::with_robot(one_robot(), conn as Arc<dyn RobotConn>)
}

/// The same, waiting on `timings`.
fn scripted_with(conn: Arc<FakeRobotConn>, timings: Timings) -> TestServer {
    TestServer::with_robot_and_timings(one_robot(), conn as Arc<dyn RobotConn>, timings)
}

/// The connected robot's entry, which is where ownership and the meters live.
fn entry(server: &TestServer) -> Arc<RobotEntry> {
    server
        .state
        .registry()
        .peek(&Esn::new(SERIAL))
        .expect("robot connected")
}

/// How many event streams the robot has been asked to open.
fn opens(conn: &FakeRobotConn) -> usize {
    conn.call_count(|call| matches!(call, RobotCall::OpenEventStream { .. }))
}

/// Sends `uri` through a clone of the router on its own task, so a test can
/// hold the request open while it moves the clock or drops the caller.
fn detached(server: &TestServer, uri: String) -> tokio::task::JoinHandle<Reply> {
    let router = server.router.clone();
    tokio::spawn(async move { send_to(&router, request(Method::GET, &uri, None)).await })
}

#[tokio::test]
async fn net_probe_answers_the_six_keys_in_gos_order() {
    // Go discards the verdict, so an UNSUPPORTED answer is still a completed
    // round trip and still produces a body. A port that read it would fail
    // here rather than in front of the robot.
    let conn = Arc::new(FakeRobotConn::new().with_protocol(Ok(ProtocolVerdict {
        result: ProtocolResult::Unsupported,
        host_version: 0,
    })));
    let server = scripted(Arc::clone(&conn));

    let reply = server
        .get(&format!("/api-sdk/net_probe?serial={SERIAL}"))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.body,
        concat!(
            r#"{"rttMs":0,"probe":"ProtocolVersion","target":"192.168.8.203:443","#,
            r#""camBytes":0,"camFrames":0,"camOn":false}"#
        )
    );
    // `fmt.Fprint(w, string(jsonBytes))`, so no trailing newline and a sniffed
    // content type rather than an explicit JSON one.
    assert!(!reply.body.ends_with('\n'));
    assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_TEXT));
    reply.assert_absent(&CORS_HEADERS, "net_probe");
    reply.assert_absent(&CACHE_HEADERS, "net_probe");

    // Zero prints as `0`, never as `0.0`. `serde_json`'s own f64 writer would
    // produce the second, which is the whole reason `rttMs` is a `RawValue`.
    assert!(reply.body.starts_with(r#"{"rttMs":0,"#));
    assert!(!reply.body.contains("0.0"));

    // One `ProtocolVersion` at Go's two constants, and nothing beyond the
    // connect-time liveness check.
    assert_eq!(
        conn.calls(),
        vec![
            RobotCall::BatteryState,
            RobotCall::ProtocolVersion {
                client_version: 5,
                min_host_version: 0,
            },
        ]
    );
}

#[tokio::test]
async fn net_probe_reports_the_round_trip_gos_way() {
    let conn = Arc::new(FakeRobotConn::new());
    let gate = conn.arm_protocol_gate();
    let server = scripted(Arc::clone(&conn));

    // Connect first, so the dial is not inside the measured window.
    server
        .post(&format!("/api-sdk/conn_test?serial={SERIAL}"))
        .await;
    let probe = detached(&server, format!("/api-sdk/net_probe?serial={SERIAL}"));

    // The round trip is genuinely in flight, so moving the clock here moves
    // exactly the interval Go measures with `time.Since(start)`.
    gate.wait_entered().await;
    server.clock.advance(Duration::from_micros(13_482));
    gate.release();

    let reply = tokio::time::timeout(CEILING, probe)
        .await
        .expect("within the ceiling")
        .expect("the probe task did not panic");
    // Go's own example figure, rendered by `encoding/json`'s rules rather than
    // by `serde_json`'s.
    assert!(
        reply.body.starts_with(r#"{"rttMs":13.482,"#),
        "{}",
        reply.body
    );
}

#[tokio::test]
async fn net_probe_truncates_the_round_trip_to_whole_microseconds() {
    let conn = Arc::new(FakeRobotConn::new());
    let gate = conn.arm_protocol_gate();
    let server = scripted(Arc::clone(&conn));
    server
        .post(&format!("/api-sdk/conn_test?serial={SERIAL}"))
        .await;
    let probe = detached(&server, format!("/api-sdk/net_probe?serial={SERIAL}"));

    gate.wait_entered().await;
    // `float64(rtt.Microseconds()) / 1000` discards the nanoseconds before it
    // divides, so 999 of them are a flat zero rather than 0.000999.
    server.clock.advance(Duration::from_nanos(999));
    gate.release();

    let reply = tokio::time::timeout(CEILING, probe)
        .await
        .expect("within the ceiling")
        .expect("the probe task did not panic");
    assert!(reply.body.starts_with(r#"{"rttMs":0,"#), "{}", reply.body);
}

#[tokio::test]
async fn a_probe_that_runs_out_of_time_is_a_lost_probe() {
    let conn = Arc::new(FakeRobotConn::new());
    // Armed and never released, so the round trip never completes.
    let _gate = conn.arm_protocol_gate();
    let server = scripted_with(Arc::clone(&conn), Timings::instant());

    let reply = tokio::time::timeout(
        CEILING,
        server.get(&format!("/api-sdk/net_probe?serial={SERIAL}")),
    )
    .await
    .expect("the probe deadline bounds the round trip");

    assert_eq!(reply.status, StatusCode::OK);
    // grpc-go's rendering of an expired context, which is what the
    // connectivity panel shows once it has stripped the `error: ` prefix.
    assert_eq!(
        reply.body,
        "error: rpc error: code = DeadlineExceeded desc = context deadline exceeded"
    );
    // A lost probe carries no number at all, so the page cannot average the
    // deadline into the latency figure.
    assert!(!reply.body.contains("rttMs"));
    assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_TEXT));
}

#[tokio::test]
async fn a_probe_that_fails_reports_the_status_text() {
    let conn = Arc::new(FakeRobotConn::new().with_protocol(Err(unreachable_error())));
    let server = scripted(conn);

    let reply = server
        .get(&format!("/api-sdk/net_probe?serial={SERIAL}"))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.body,
        format!("{}{}", literals::ERROR_PREFIX, unreachable_error())
    );
    assert!(!reply.body.contains("rttMs"));
}

#[tokio::test]
async fn a_probe_abandoned_by_its_client_leaves_no_second_call_behind() {
    let conn = Arc::new(FakeRobotConn::new());
    let gate = conn.arm_protocol_gate();
    let server = scripted(Arc::clone(&conn));
    server
        .post(&format!("/api-sdk/conn_test?serial={SERIAL}"))
        .await;
    let probe = detached(&server, format!("/api-sdk/net_probe?serial={SERIAL}"));

    // Parked inside the RPC, which is where a client that closes its
    // connection leaves the handler.
    gate.wait_entered().await;
    probe.abort();
    assert!(
        tokio::time::timeout(CEILING, probe)
            .await
            .expect("within the ceiling")
            .expect_err("the task was aborted")
            .is_cancelled(),
        "the handler future is dropped rather than run to completion"
    );

    // The RPC is awaited inline rather than spawned, so dropping the handler
    // dropped it too: nothing retried it and nothing else ran.
    assert_eq!(
        conn.call_count(|call| matches!(call, RobotCall::ProtocolVersion { .. })),
        1
    );
    // And nothing is wedged: the next probe answers normally.
    let reply = server
        .get(&format!("/api-sdk/net_probe?serial={SERIAL}"))
        .await;
    assert!(reply.body.starts_with(r#"{"rttMs":0,"#), "{}", reply.body);
}

#[tokio::test]
async fn an_abandoned_probe_leaves_no_rpc_in_flight() {
    let conn = Arc::new(FakeRobotConn::new());
    let gate = conn.arm_protocol_gate();
    let server = scripted(Arc::clone(&conn));
    server
        .post(&format!("/api-sdk/conn_test?serial={SERIAL}"))
        .await;

    // The registry entry, the dial seam and this test hold the only references
    // while nothing is in flight, so any extra one belongs to a round trip.
    let settled = Arc::strong_count(&conn);
    let probe = detached(&server, format!("/api-sdk/net_probe?serial={SERIAL}"));
    gate.wait_entered().await;
    probe.abort();
    let _ = probe.await;

    // The call log cannot tell these two apart: the fake records the call
    // before it parks, so a dropped round trip and one still parked in the gate
    // both read as exactly one `ProtocolVersion`. The reference count can. The
    // RPC is awaited inline, so the handler's future owns it and the drop takes
    // it; a probe that had been spawned instead would still be parked here,
    // holding the owned `Arc` that `tokio::spawn` forces it to take, and this
    // would never come back down. That is the property deviation 19 accepts the
    // missing `Canceled` body for.
    wait_until("the robot connection to be let go", || {
        Arc::strong_count(&conn) == settled
    })
    .await;
    gate.release();
    assert_eq!(Arc::strong_count(&conn), settled);
}

#[tokio::test]
async fn cam_on_follows_camera_ownership() {
    let server = scripted(Arc::new(FakeRobotConn::new()));
    server
        .post(&format!("/api-sdk/conn_test?serial={SERIAL}"))
        .await;
    let entry = entry(&server);

    let (_generation, displaced) = entry.session.cam.claim(CancellationToken::new());
    assert!(displaced.is_none(), "nobody held the feed");
    let reply = server
        .get(&format!("/api-sdk/net_probe?serial={SERIAL}"))
        .await;
    assert!(reply.body.ends_with(r#""camOn":true}"#), "{}", reply.body);

    entry.session.cam.stop();
    let reply = server
        .get(&format!("/api-sdk/net_probe?serial={SERIAL}"))
        .await;
    assert!(reply.body.ends_with(r#""camOn":false}"#), "{}", reply.body);
}

#[tokio::test]
async fn the_camera_counters_are_monotone_across_a_disconnect() {
    // Only the settle a disconnect pays is zeroed; the probe deadline stays at
    // Go's five seconds, so the round trip itself is unaffected.
    let timings = Timings {
        disconnect_settle: Duration::ZERO,
        ..Timings::default()
    };
    let server = scripted_with(Arc::new(FakeRobotConn::new()), timings);
    let esn = Esn::new(SERIAL);
    server
        .post(&format!("/api-sdk/conn_test?serial={SERIAL}"))
        .await;

    server.state.registry().meter(&esn).record(1024);
    server.state.registry().meter(&esn).record(1024);
    let reply = server
        .get(&format!("/api-sdk/net_probe?serial={SERIAL}"))
        .await;
    assert!(
        reply.body.contains(r#""camBytes":2048,"camFrames":2,"#),
        "{}",
        reply.body
    );

    // The entry goes; the meter does not. A reconnect that resumed from zero
    // would look like a server restart to the page, which reads a counter that
    // went backwards as one and drops the sample.
    assert!(server.state.registry().disconnect(&esn).await);
    assert_eq!(server.state.registry().len(), 0);
    server.state.registry().meter(&esn).record(1024);

    let reply = server
        .get(&format!("/api-sdk/net_probe?serial={SERIAL}"))
        .await;
    assert!(
        reply.body.contains(r#""camBytes":3072,"camFrames":3,"#),
        "{}",
        reply.body
    );
    // The camera flag belongs to the new session, so it starts clear.
    assert!(reply.body.ends_with(r#""camOn":false}"#), "{}", reply.body);
}

#[tokio::test]
async fn net_probe_creates_no_meter_for_a_robot_that_has_never_streamed() {
    let server = scripted(Arc::new(FakeRobotConn::new()));

    let reply = server
        .get(&format!("/api-sdk/net_probe?serial={SERIAL}"))
        .await;
    assert!(
        reply.body.contains(r#""camBytes":0,"camFrames":0,"#),
        "{}",
        reply.body
    );
    // Go reaches the value through `getCamMeter`, which inserts on a read
    // (`robot.go:182-185`). This deliberately does not, which is deviation 7.
    assert_eq!(server.state.registry().meters_len(), 0);
}

#[tokio::test]
async fn get_stim_status_answers_a_non_json_sentinel_while_idle() {
    let server = scripted(Arc::new(FakeRobotConn::new()));

    let reply = server
        .get(&format!("/api-sdk/get_stim_status?serial={SERIAL}"))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, literals::MUST_START_EVENT_STREAM);
    assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_TEXT));
    assert!(!reply.body.ends_with('\n'));

    // The dashboard's circuit breaker is a failing `response.json()`. A body
    // that parsed would reset `stimFails`, the breaker would never trip, and
    // the chart would plot an undefined value twice a second for the life of
    // the page.
    serde_json::from_str::<serde_json::Value>(&reply.body)
        .expect_err("the idle sentinel must not parse as JSON");
}

#[tokio::test]
async fn begin_event_stream_opens_one_stream_and_answers_done() {
    let (receiver, mut handle) = FakeReceiver::new();
    let conn = Arc::new(FakeRobotConn::new().with_event_stream(Ok(Box::new(receiver))));
    let server = scripted(Arc::clone(&conn));

    let reply = server
        .post(&format!("/api-sdk/begin_event_stream?serial={SERIAL}"))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, literals::DONE);
    assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_TEXT));

    // The receiver is parked in its first receive, so the stream is genuinely
    // open rather than merely spawned.
    tokio::time::timeout(CEILING, handle.ready())
        .await
        .expect("the receiver reached its receive");
    assert_eq!(
        conn.calls().last(),
        Some(&RobotCall::OpenEventStream {
            whitelist: vec!["stimulation_info".to_owned()],
            connection_id: "wirepod".to_owned(),
        })
    );

    // Answering `done` is not the same as reporting a reading: the flag is now
    // set, so the status route answers a number rather than the sentinel.
    let reply = server
        .get(&format!("/api-sdk/get_stim_status?serial={SERIAL}"))
        .await;
    assert_eq!(reply.body, "0");
}

#[tokio::test]
async fn a_second_begin_answers_done_and_stacks_no_receiver() {
    let (first, mut first_handle) = FakeReceiver::new();
    let (second, _second_handle) = FakeReceiver::new();
    let conn = Arc::new(
        FakeRobotConn::new()
            .with_event_stream(Ok(Box::new(first)))
            .with_event_stream(Ok(Box::new(second))),
    );
    let server = scripted(Arc::clone(&conn));

    server
        .post(&format!("/api-sdk/begin_event_stream?serial={SERIAL}"))
        .await;
    tokio::time::timeout(CEILING, first_handle.ready())
        .await
        .expect("the first receiver reached its receive");
    assert_eq!(opens(&conn), 1);

    // A second begin is a double click, not a restart. The claim is refused
    // before anything is spawned, so no second stream is ever opened and the
    // graph is not blanked.
    let reply = server
        .post(&format!("/api-sdk/begin_event_stream?serial={SERIAL}"))
        .await;
    assert_eq!(reply.body, literals::DONE);

    // Asserted the positive way rather than by counting straight after the
    // second request, because a begin that displaced the incumbent would open
    // its replacement on a task and the count would still read 1 for an
    // instant. The incumbent is untouched exactly when its readings still land:
    // a displaced receiver has been cancelled and its generation superseded, so
    // this would never arrive.
    let owner = Arc::clone(&entry(&server).session.events);
    first_handle.send_stim(0.5, 0.25);
    wait_until("the incumbent's reading", || owner.stim().value == 0.5).await;
    assert_eq!(opens(&conn), 1, "the second begin opened nothing");
    assert!(owner.is_streaming());
}

#[tokio::test]
async fn a_begin_straight_after_a_stop_is_admitted() {
    let (first, mut first_handle) = FakeReceiver::new();
    let (second, mut second_handle) = FakeReceiver::new();
    let conn = Arc::new(
        FakeRobotConn::new()
            .with_event_stream(Ok(Box::new(first)))
            .with_event_stream(Ok(Box::new(second))),
    );
    let server = scripted(Arc::clone(&conn));

    server
        .post(&format!("/api-sdk/begin_event_stream?serial={SERIAL}"))
        .await;
    tokio::time::timeout(CEILING, first_handle.ready())
        .await
        .expect("the first receiver reached its receive");

    // The stop releases ownership in the same critical section that clears the
    // flag, so the very next begin succeeds rather than waiting for the old
    // receiver to wake up. Waiting would leave the poller reading the sentinel
    // until it gave up.
    let reply = server
        .post(&format!("/api-sdk/stop_event_stream?serial={SERIAL}"))
        .await;
    assert_eq!(reply.body, literals::DONE);
    assert!(!entry(&server).session.events.is_streaming());

    let reply = server
        .post(&format!("/api-sdk/begin_event_stream?serial={SERIAL}"))
        .await;
    assert_eq!(reply.body, literals::DONE);
    tokio::time::timeout(CEILING, second_handle.ready())
        .await
        .expect("the second receiver reached its receive");
    assert_eq!(opens(&conn), 2);

    // And the first receiver was let go rather than left parked, which is what
    // the cancellation is for: it has no idea the token exists.
    tokio::time::timeout(CEILING, first_handle.wait_dropped())
        .await
        .expect("the stopped receiver was dropped");
}

#[tokio::test]
async fn a_stop_with_no_stream_still_answers_done() {
    let server = scripted(Arc::new(FakeRobotConn::new()));

    // Go's `stopEventStream` deletes unconditionally and the arm answers `done`
    // either way, so an ESN that never started one is not an error.
    let reply = server
        .post(&format!("/api-sdk/stop_event_stream?serial={SERIAL}"))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, literals::DONE);
    assert!(!entry(&server).session.events.is_streaming());
}

#[tokio::test]
async fn a_stream_setup_failure_never_reaches_the_body() {
    // No receiver queued, so `open_event_stream` fails.
    let conn = Arc::new(FakeRobotConn::new());
    let server = scripted(Arc::clone(&conn));

    let reply = server
        .post(&format!("/api-sdk/begin_event_stream?serial={SERIAL}"))
        .await;
    // The request returned long before the task had an answer, so the only
    // place the error can go is the log.
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, literals::DONE);

    // The failing task hands the claim straight back, so the stream is not left
    // marked as running and a later begin can take it.
    let owner = Arc::clone(&entry(&server).session.events);
    wait_until("the failed claim to be released", || !owner.is_streaming()).await;
    let reply = server
        .get(&format!("/api-sdk/get_stim_status?serial={SERIAL}"))
        .await;
    assert_eq!(reply.body, literals::MUST_START_EVENT_STREAM);
    assert_eq!(opens(&conn), 1);
}

#[tokio::test]
async fn the_stim_value_is_the_one_the_receiver_published_and_zeroes_on_a_stop() {
    let (receiver, mut handle) = FakeReceiver::new();
    let conn = Arc::new(FakeRobotConn::new().with_event_stream(Ok(Box::new(receiver))));
    let server = scripted(conn);

    server
        .post(&format!("/api-sdk/begin_event_stream?serial={SERIAL}"))
        .await;
    tokio::time::timeout(CEILING, handle.ready())
        .await
        .expect("the receiver reached its receive");

    let owner = Arc::clone(&entry(&server).session.events);
    handle.send_stim(0.75, 0.1);
    wait_until("the published reading", || owner.stim().value == 0.75).await;

    // Go's `%v` on a float32: shortest round-trip digits, no quotes, no
    // newline.
    let reply = server
        .get(&format!("/api-sdk/get_stim_status?serial={SERIAL}"))
        .await;
    assert_eq!(reply.body, "0.75");
    assert!(!reply.body.ends_with('\n'));
    assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_TEXT));

    // A second reading replaces the first and is rendered the same way. The
    // zero-velocity rule is pinned by a test of its own rather than here,
    // because waiting for a later value cannot tell a write that was skipped
    // from one that was overwritten.
    handle.send_stim(0.1, 0.25);
    wait_until("the second published reading", || owner.stim().value == 0.1).await;
    let reply = server
        .get(&format!("/api-sdk/get_stim_status?serial={SERIAL}"))
        .await;
    assert_eq!(reply.body, "0.1");

    // The stop zeroes the reading in the same critical section that clears the
    // flag, so there is no moment where a stale non-zero value is readable.
    server
        .post(&format!("/api-sdk/stop_event_stream?serial={SERIAL}"))
        .await;
    assert_eq!(owner.stim(), StimSample::ZERO);
    let reply = server
        .get(&format!("/api-sdk/get_stim_status?serial={SERIAL}"))
        .await;
    assert_eq!(reply.body, literals::MUST_START_EVENT_STREAM);
}

#[tokio::test]
async fn a_zero_velocity_event_is_not_a_reading() {
    let (receiver, mut handle) = FakeReceiver::new();
    let conn = Arc::new(FakeRobotConn::new().with_event_stream(Ok(Box::new(receiver))));
    let server = scripted(conn);

    server
        .post(&format!("/api-sdk/begin_event_stream?serial={SERIAL}"))
        .await;
    tokio::time::timeout(CEILING, handle.ready())
        .await
        .expect("the receiver reached its receive");

    let owner = Arc::clone(&entry(&server).session.events);
    handle.send_stim(0.75, 0.1);
    wait_until("the published reading", || owner.stim().value == 0.75).await;

    // proto3 omits a zero scalar from the text form, so Go's presence test,
    // `strings.Contains(fmt.Sprint(stimInfo), "velocity")` (`server.go:656-659`),
    // is false for such an event and the value never reaches the state.
    //
    // The skipped event is queued last and the clean end of stream behind it is
    // the barrier. The channel is ordered and the loop drops its receiver only
    // once it has consumed everything ahead of the end, so an event that had
    // been published would already have replaced the 0.75 by the time this
    // resolves.
    handle.send_stim(0.5, 0.0);
    handle.end();
    tokio::time::timeout(CEILING, handle.wait_dropped())
        .await
        .expect("the loop drained the queue and returned");
    assert_eq!(
        owner.stim(),
        StimSample::new(0.75, 0.1),
        "a zero-velocity event is not a reading"
    );

    // The loop released the claim on its way out, so the status route is back
    // to the sentinel; the release leaves the reading alone, which is why the
    // assertion above is made against the owner rather than through the route.
    assert!(!owner.is_streaming());
    let reply = server
        .get(&format!("/api-sdk/get_stim_status?serial={SERIAL}"))
        .await;
    assert_eq!(reply.body, literals::MUST_START_EVENT_STREAM);
}

#[tokio::test]
async fn the_four_routes_reset_the_idle_timer_like_every_other_path() {
    let (receiver, _handle) = FakeReceiver::new();
    let conn = Arc::new(FakeRobotConn::new().with_event_stream(Ok(Box::new(receiver))));
    let server = scripted(conn);
    server
        .post(&format!("/api-sdk/conn_test?serial={SERIAL}"))
        .await;
    let entry = entry(&server);

    // None of the four is preamble-exempt, so each one is a
    // `robots[robotIndex].ConnTimer = 0`.
    for (seconds, route) in [
        (11u64, "net_probe"),
        (22, "begin_event_stream"),
        (33, "get_stim_status"),
        (44, "stop_event_stream"),
    ] {
        server.clock.set(Duration::from_secs(seconds));
        server
            .post(&format!("/api-sdk/{route}?serial={SERIAL}"))
            .await;
        assert_eq!(
            entry.last_touch(),
            Duration::from_secs(seconds),
            "{route} must reset the idle timer"
        );
    }
}
