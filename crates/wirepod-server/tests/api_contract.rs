//! `/api/*`: the CORS headers, the 404, and `get_bot_status`.

use http::{StatusCode, header};
use wirepod_core::BotInfo;
use wirepod_server::literals;
use wirepod_server::test_support::{TEST_IP, TestServer, no_robots};

/// Two robots, the first with the serial stored in upper case.
///
/// The stored case is contract: the dashboard matches this `esn` against the
/// raw `?serial=` value from its own URL with `===` (`vectorbrain.js:262`), so
/// emitting a normalised spelling would kill the status card and the camera
/// retry loop while leaving the camera's first attempt working.
fn two_robots() -> BotInfo {
    serde_json::from_str(concat!(
        r#"{"global_guid":"global-guid-placeholder","robots":["#,
        r#"{"esn":"00303F28","ip_address":"192.168.8.203","guid":"robot-guid-placeholder","#,
        r#""activated":true},"#,
        r#"{"esn":"00e20100","ip_address":"192.168.8.77","guid":"","activated":false}"#,
        "]}"
    ))
    .expect("parse the two-robot fixture")
}

/// The first element of the `get_bot_status` body, which is the robot the
/// pinger has an entry for.
async fn first_status(server: &TestServer) -> String {
    let body = server.get("/api/get_bot_status").await.body;
    body.split("},{").next().expect("one element").to_owned()
}

/// Records a conn check from the first robot, the way `/ok` will once P1 wires
/// the pinger up.
fn note_check(server: &TestServer) {
    let seen = server.state.with_bot_info(|info| {
        server
            .state
            .pinger()
            .note_check(info, TEST_IP, server.state.clock().as_ref())
    });
    assert!(seen, "the peer IP is in the fixture");
}

#[tokio::test]
async fn get_bot_status_is_an_empty_array_and_a_newline_when_no_robot_is_known() {
    let server = TestServer::connected(no_robots());

    let reply = server.get("/api/get_bot_status").await;
    assert_eq!(reply.status, StatusCode::OK);
    // Never `null`: Go builds the slice as `[]BotStatus{}` on purpose, and
    // `json.Encoder.Encode` appends the newline.
    assert_eq!(reply.body, literals::EMPTY_BOT_STATUS);
    assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_JSON));
}

#[tokio::test]
async fn get_bot_status_reports_every_robot_in_file_order_with_gos_key_order() {
    let server = TestServer::connected(two_robots());
    note_check(&server);

    let reply = server.get("/api/get_bot_status").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.body,
        concat!(
            r#"[{"esn":"00303F28","ip":"192.168.8.203","status":"online","timesince":0},"#,
            r#"{"esn":"00e20100","ip":"192.168.8.77","status":"disconnected","timesince":-1}]"#,
            "\n"
        )
    );
    // The serial is echoed in the case the file stores, not normalised.
    assert!(reply.body.contains("00303F28"));
    // A robot the pinger has never heard from reports -1, never 0.
    assert!(reply.body.contains(r#""timesince":-1"#));
}

#[tokio::test]
async fn get_bot_status_switches_status_at_gos_thresholds() {
    let server = TestServer::connected(two_robots());
    note_check(&server);

    assert!(first_status(&server).await.contains(r#""status":"online""#));

    // `online` is `TimeSinceLastCheck <= 15`, so 15 is still online.
    server.clock.advance_secs(15);
    assert!(first_status(&server).await.contains(r#""status":"online""#));

    // The ticker latches `Stopped` once the counter passes 15.
    server.clock.advance_secs(1);
    assert!(
        first_status(&server)
            .await
            .contains(r#""status":"offline""#)
    );

    // `offline` is `Stopped && TimeSinceLastCheck < 120`, so 119 is the last.
    server.clock.advance_secs(103);
    assert!(
        first_status(&server)
            .await
            .contains(r#""status":"offline""#)
    );

    server.clock.advance_secs(1);
    assert!(
        first_status(&server)
            .await
            .contains(r#""status":"disconnected""#)
    );
    assert!(first_status(&server).await.contains(r#""timesince":120"#));
}

#[tokio::test]
async fn every_api_response_carries_both_cors_headers_including_the_404() {
    let server = TestServer::connected(two_robots());

    for uri in [
        "/api/get_bot_status",
        "/api/does_not_exist",
        "/api/",
        "/api/get_logs_json?level=info",
    ] {
        let reply = server.get(uri).await;
        assert_eq!(
            reply.header(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(literals::CORS_ANY),
            "{uri}"
        );
        assert_eq!(
            reply.header(header::ACCESS_CONTROL_ALLOW_HEADERS),
            Some(literals::CORS_ANY),
            "{uri}"
        );
    }

    // The 404 from `apiHandler`'s default is `not found\n` with the CORS
    // headers, which is what distinguishes it from the file-server 404 a path
    // that missed the prefix entirely would get.
    let reply = server.get("/api/does_not_exist").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body, literals::NOT_FOUND);
    assert_eq!(
        reply.header(header::X_CONTENT_TYPE_OPTIONS),
        Some(literals::NOSNIFF)
    );
}

#[tokio::test]
async fn get_bot_status_reads_no_parameter_of_any_kind() {
    // `handleGetBotStatus(w http.ResponseWriter)` takes only the writer, so
    // there is nothing for a parameter to change.
    let server = TestServer::connected(two_robots());
    note_check(&server);

    let plain = server.get("/api/get_bot_status").await.body;
    let with_query = server
        .get("/api/get_bot_status?serial=00303f28&esn=nonsense")
        .await
        .body;
    let with_body = server
        .post_form("/api/get_bot_status", "serial=nosuchbot")
        .await
        .body;

    assert_eq!(plain, with_query);
    assert_eq!(plain, with_body);
}
