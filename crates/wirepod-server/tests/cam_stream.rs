//! `/cam-stream` against a real gRPC robot on loopback, and a handover between
//! two viewers against the in-process fake.
//!
//! The loopback fake binds `127.0.0.1:0`, so nothing here binds a fixed port or
//! reaches the robot on the network. The payload bytes are outside the parity
//! contract, because no Rust JPEG encoder produces Go's bytes for the same
//! picture, so what is asserted is the framing around them and that each
//! payload decodes. The loopback fake hands out one camera feed, so the
//! handover runs against `FakeRobotConn`, which queues two.

use std::sync::Arc;
use std::time::Duration;

use http::{Method, StatusCode, header};
use http_body_util::BodyExt;
use image::ExtendedColorType;
use image::codecs::jpeg::JpegEncoder;
use tower::ServiceExt;
use wirepod_core::test_support::{FakeConnFactory, FakeFrameStream, FakeRobotConn, RobotCall};
use wirepod_core::{AppState, BotInfo, RobotConn, RobotConnFactory, Timings};
use wirepod_server::build_router;
use wirepod_server::test_support::{CEILING, one_robot, request};
use wirepod_vector::test_support::spawn_fake_robot;
use wirepod_vector::{TonicConnFactory, plaintext_builder};

/// The serial the fixtures use.
const SERIAL: &str = "00303f28";

/// The handover's settle. Long enough that a camera turned on after it cannot
/// be mistaken for one turned on before the first feed closed.
const SETTLE: Duration = Duration::from_millis(300);

/// The bytes Go writes before every frame (`server.go:783`).
const PART_HEADER: &[u8] = b"--boundary\r\nContent-Type: image/jpeg\r\n\r\n";

/// A small JPEG, built at run time rather than committed.
fn jpeg(shade: u8) -> Vec<u8> {
    let mut buffer = Vec::new();
    JpegEncoder::new_with_quality(&mut buffer, 90)
        .encode(&[shade; 3 * 4], 2, 2, ExtendedColorType::Rgb8)
        .expect("encode the fixture");
    buffer
}

#[tokio::test]
async fn two_pushed_frames_come_out_between_boundary_markers() {
    tokio::time::timeout(CEILING, async {
        let (addr, handle) = spawn_fake_robot().await;
        // The GUID is a placeholder. A real robot GUID never goes into a
        // fixture, a document or a log line.
        let bot_info: BotInfo = serde_json::from_str(&format!(
            concat!(
                r#"{{"global_guid":"global-guid-placeholder","robots":[{{"esn":"{esn}","#,
                r#""ip_address":"{ip}","guid":"robot-guid-placeholder","activated":true}}]}}"#
            ),
            esn = SERIAL,
            ip = addr
        ))
        .expect("parse the loopback fixture");
        let factory: Arc<dyn RobotConnFactory> =
            Arc::new(TonicConnFactory::with_endpoint_builder(plaintext_builder()));
        let state = AppState::builder(factory).bot_info(bot_info).build();
        let router = build_router(state);

        let response = router
            .oneshot(request(
                Method::GET,
                &format!("/cam-stream?serial={SERIAL}&_=1"),
                None,
            ))
            .await
            .expect("the router is infallible");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("multipart/x-mixed-replace; boundary=--boundary")
        );

        // The camera was turned on before the feed was opened.
        assert_eq!(handle.image_streaming_calls(), vec![true]);

        handle.push_frame(jpeg(40));
        handle.push_frame(jpeg(200));
        // A truncated frame is counted and skipped rather than encoded, which
        // in Go panicked the process (`server.go:777-782`).
        handle.push_frame(b"not a jpeg".to_vec());
        handle.end_camera_feed();

        let body = response
            .into_body()
            .collect()
            .await
            .expect("collect the stream")
            .to_bytes();

        let mut payloads = Vec::new();
        let mut rest: &[u8] = &body;
        while let Some(start) = find(rest, PART_HEADER) {
            assert_eq!(start, 0, "a part did not begin at the boundary");
            rest = &rest[PART_HEADER.len()..];
            let end = find(rest, PART_HEADER).unwrap_or(rest.len());
            payloads.push(rest[..end].to_vec());
            rest = &rest[end..];
        }
        assert!(rest.is_empty(), "trailing bytes after the last part");
        assert_eq!(payloads.len(), 2, "two decodable frames, two parts");
        for payload in &payloads {
            image::load_from_memory(payload).expect("the payload decodes as a JPEG");
        }
        // No closing boundary and no terminator: the stream simply ends.
        assert!(!body.ends_with(b"--boundary--"));

        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

/// Go's claim cancels the displaced viewer's context, and grpc-go closes that
/// viewer's `CameraFeed` the moment it is cancelled (`robot.go:112-114`). The
/// robot's gateway turns the camera off when a feed closes, so the close has to
/// land inside the new viewer's settle, before its enable, or the new viewer is
/// left with the camera off and never gets a frame.
#[tokio::test]
async fn a_second_viewer_closes_the_first_feed_before_turning_the_camera_on() {
    tokio::time::timeout(CEILING, async {
        let (first, mut first_handle) = FakeFrameStream::new();
        let (second, _second_handle) = FakeFrameStream::new();
        let robot = Arc::new(
            FakeRobotConn::new()
                .with_camera_feed(Ok(Box::new(first)))
                .with_camera_feed(Ok(Box::new(second))),
        );
        let conn: Arc<dyn RobotConn> = Arc::clone(&robot) as Arc<dyn RobotConn>;
        let factory: Arc<dyn RobotConnFactory> = Arc::new(FakeConnFactory::connecting_to(conn));
        let state = AppState::builder(factory)
            .bot_info(one_robot())
            .timings(Timings {
                settle: SETTLE,
                enable: Duration::from_secs(5),
                ..Timings::instant()
            })
            .build();
        let router = build_router(state);
        let uri = format!("/cam-stream?serial={SERIAL}");

        // Held until the end, because dropping the body is the first viewer
        // leaving, which would end its feed for a different reason.
        let first_viewer = router
            .clone()
            .oneshot(request(Method::GET, &uri, None))
            .await
            .expect("the router is infallible");
        assert_eq!(first_viewer.status(), StatusCode::OK);
        assert_eq!(camera_calls(&robot), vec![true]);

        let second_viewer = tokio::spawn(router.oneshot(request(Method::GET, &uri, None)));
        first_handle.wait_dropped().await;
        assert_eq!(
            camera_calls(&robot),
            vec![true],
            "the first viewer's feed was still open when the second viewer turned the camera on"
        );

        let second_viewer = second_viewer
            .await
            .expect("the second request panicked")
            .expect("the router is infallible");
        assert_eq!(second_viewer.status(), StatusCode::OK);
        // The first viewer's release finds the claim gone and sends no disable.
        assert_eq!(camera_calls(&robot), vec![true, true]);
        drop(first_viewer);
    })
    .await
    .expect("within the ceiling");
}

/// Every `enable_image_streaming` the robot recorded, in order.
fn camera_calls(robot: &FakeRobotConn) -> Vec<bool> {
    robot
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            RobotCall::EnableImageStreaming(on) => Some(on),
            _ => None,
        })
        .collect()
}

/// The first offset of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
