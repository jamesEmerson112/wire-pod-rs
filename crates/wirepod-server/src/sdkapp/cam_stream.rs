//! `/cam-stream`: the camera feed as a `multipart/x-mixed-replace` response.
//!
//! The claim, the settle, the enable, the frame pump and the meters are
//! `start_cam_stream` and `cam_stream_pump` in `wirepod-core`. What is here is
//! Go's `camStreamHandler` around them: the preamble, the response, and the
//! per-frame decode and re-encode.
//!
//! Go's loop writes each part into the response as it goes, which an axum
//! handler cannot do because it returns the response before the body is read.
//! The pump therefore runs in a task that writes into a channel feeding the
//! body, and that task owns the guard, so every exit path still releases the
//! feed.

use std::convert::Infallible;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::response::Response;
use http::{HeaderValue, header};
use image::codecs::jpeg::JpegEncoder;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use wirepod_core::{
    AppState, CameraControl, Esn, FrameOutcome, FrameSink, cam_stream_pump, start_cam_stream,
};

use crate::form;
use crate::{literals, reply};

/// The declared boundary token itself begins with two dashes, which is Go's.
const CONTENT_TYPE_MULTIPART: &str = "multipart/x-mixed-replace; boundary=--boundary";

/// The separator deliberately does not match the declared boundary, and no CRLF
/// follows the payload.
const PART_HEADER: &[u8] = b"--boundary\r\nContent-Type: image/jpeg\r\n\r\n";

const JPEG_QUALITY: u8 = 50;

/// How many finished parts may wait for the client before the pump blocks.
const PART_QUEUE: usize = 1;

/// Answers the feed.
pub async fn handle(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let (_parts, form) = form::read(req).await;
    // Go throws the robot index away here, so nothing on this route resets the
    // idle timer.
    let serial = Esn::new(form.get("serial"));
    let entry = match state.get_robot(&serial).await {
        Ok(entry) => entry,
        Err(err) => return reply::text(format!("{}{err}", literals::ERROR_PREFIX)),
    };

    let cancel = CancellationToken::new();
    let camera: Arc<dyn CameraControl> = Arc::clone(&entry.conn) as Arc<dyn CameraControl>;
    let guard = match start_cam_stream(
        Arc::clone(&entry.session),
        camera,
        state.timings(),
        cancel.clone(),
    )
    .await
    {
        Ok(guard) => guard,
        // Go's `enableImageStreaming` discards its error and the handler opens
        // the feed anyway; the seam hands the error back instead, and it is
        // reported the way the feed's own error is.
        Err(err) => return reply::text(format!("{}{err}", literals::ERROR_PREFIX)),
    };

    let mut frames = match entry.conn.open_camera_feed().await {
        Ok(frames) => frames,
        // Dropping the guard releases the claim and turns the camera off, which
        // is Go's deferred `finishCamStream`.
        Err(err) => return reply::text(format!("{}{err}", literals::ERROR_PREFIX)),
    };

    let (frames_tx, frames_rx) = mpsc::channel::<Result<Bytes, Infallible>>(PART_QUEUE);
    let meter = state.registry().meter(&entry.esn);
    let pump_cancel = cancel.clone();
    tokio::spawn(async move {
        let mut sink = ChannelSink {
            frames: frames_tx.clone(),
        };
        // Go's watcher goroutine on the request context. The body's receiver
        // going away is the request ending, and a robot that is sending no
        // frames would otherwise leave the pump parked in its receive for ever.
        tokio::select! {
            () = frames_tx.closed() => cancel.cancel(),
            _exit = cam_stream_pump(frames.as_mut(), &meter, &mut sink, pump_cancel) => {}
        }
        guard.finish().await;
    });

    let mut response = Response::new(Body::from_stream(ReceiverStream::new(frames_rx)));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(CONTENT_TYPE_MULTIPART),
    );
    response
}

/// The pump's other end: one finished part per send, into the response body.
struct ChannelSink {
    frames: mpsc::Sender<Result<Bytes, Infallible>>,
}

#[tonic::async_trait]
impl FrameSink for ChannelSink {
    async fn send(&mut self, jpeg: &[u8]) -> FrameOutcome {
        let Some(part) = encode_part(jpeg) else {
            return FrameOutcome::Skipped;
        };
        match self.frames.send(Ok(part)).await {
            Ok(()) => FrameOutcome::Sent,
            Err(_) => FrameOutcome::Closed,
        }
    }
}

/// One part: the boundary header, then the frame re-encoded at quality 50.
///
/// `None` is Go's `continue` on an undecodable frame. Only JPEG is registered
/// in Go, and only JPEG is compiled in here.
fn encode_part(frame: &[u8]) -> Option<Bytes> {
    let image = image::load_from_memory(frame).ok()?;
    let mut part = Vec::with_capacity(PART_HEADER.len() + frame.len());
    part.extend_from_slice(PART_HEADER);
    // Go writes the part header before the encoder runs and then discards the
    // encode error, so a frame the encoder refuses leaves a headed part with no
    // payload rather than no part at all.
    let _ = JpegEncoder::new_with_quality(&mut part, JPEG_QUALITY).encode(
        image.as_bytes(),
        image.width(),
        image.height(),
        image.color().into(),
    );
    Some(Bytes::from(part))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_part_is_the_boundary_header_and_a_jpeg_and_a_bad_frame_is_skipped() {
        let mut fixture = Vec::new();
        JpegEncoder::new_with_quality(&mut fixture, JPEG_QUALITY)
            .encode(&[7, 8, 9], 1, 1, image::ExtendedColorType::Rgb8)
            .expect("encode the fixture");

        let part = encode_part(&fixture).expect("a decodable frame makes a part");
        assert!(part.starts_with(PART_HEADER));
        // The payload is re-encoded rather than copied, so what is asserted is
        // that it decodes, never that it equals any byte string.
        image::load_from_memory(&part[PART_HEADER.len()..]).expect("the payload decodes");

        assert!(encode_part(b"").is_none());
        assert!(encode_part(b"not a jpeg at all").is_none());
    }
}
