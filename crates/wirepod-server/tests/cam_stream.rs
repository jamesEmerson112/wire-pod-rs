//! `/cam-stream` against a real gRPC robot on loopback.
//!
//! The fake binds `127.0.0.1:0`, so nothing here binds a fixed port or reaches
//! the robot on the network. The payload bytes are outside the parity contract,
//! because no Rust JPEG encoder produces Go's bytes for the same picture, so
//! what is asserted is the framing around them and that each payload decodes.

use std::sync::Arc;

use http::{Method, StatusCode, header};
use http_body_util::BodyExt;
use image::ExtendedColorType;
use image::codecs::jpeg::JpegEncoder;
use tower::ServiceExt;
use wirepod_core::{AppState, BotInfo, RobotConnFactory};
use wirepod_server::build_router;
use wirepod_server::test_support::{CEILING, request};
use wirepod_vector::test_support::spawn_fake_robot;
use wirepod_vector::{TonicConnFactory, plaintext_builder};

/// The serial the fixtures use.
const SERIAL: &str = "00303f28";

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

/// The first offset of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
