//! `/api-sdk/*`: the preamble, the exemption, and the routes the slice serves.

use http::{StatusCode, header};
use wirepod_core::{BotInfo, Esn};
use wirepod_server::test_support::{
    CACHE_HEADERS, CORS_HEADERS, TestServer, no_robots, one_robot, unreachable_error,
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
