//! The three stim routes: `begin_event_stream`, `stop_event_stream` and
//! `get_stim_status`.
//!
//! All three are cheap, and two of them answer `done` unconditionally. What
//! they actually do is drive the exclusive ownership state machine in
//! [`EventOwner`](wirepod_core::EventOwner), which is where the interesting
//! rules live: a second begin is refused rather than allowed to displace the
//! incumbent, and a stop releases ownership, clears the flag and zeroes the
//! reading in one critical section so that a begin arriving straight after a
//! stop is admitted (`robot.go:208-263`).

use std::sync::Arc;

use axum::response::Response;
use tokio_util::sync::CancellationToken;
use wirepod_core::{
    EVENT_CONNECTION_ID, EVENT_WHITELIST, RobotEntry, go_format_f32, run_event_stream,
};

use crate::{literals, reply};

/// `/api-sdk/begin_event_stream`: claim the stream, detach a receiver, answer
/// `done`.
///
/// ```text
/// streamCtx, cancel := context.WithCancel(robotObj.Ctx)
/// gen, ok := claimEventStream(robotObj.ESN, cancel)
/// if !ok {
///     cancel()
///     fmt.Fprint(w, "done")
///     return
/// }
/// go func() { ... }()
/// fmt.Fprint(w, "done")
/// ```
///
/// `server.go:467-505`. The body is `done` on every path, immediately, and a
/// stream setup failure never reaches it: the request has returned long before
/// the receiver is opened, so the only place an error can go is the log. That
/// is why the spawned task must not borrow anything from the request.
///
/// A second begin while the stream is owned changes nothing and still answers
/// `done`. Go's comment calls it a double click rather than a request to
/// restart, and the dashboard makes one easy: the drawer allows re-selecting
/// Stim and the tiles' inline `onclick` allows it twice in a row. Tearing the
/// working stream down would blank the graph for no reason.
pub fn begin(entry: &RobotEntry) -> Response {
    let cancel = CancellationToken::new();
    let owner = Arc::clone(&entry.session.events);
    let Some(generation) = owner.claim(cancel.clone()) else {
        // Already owned. The token is dropped rather than cancelled, which is
        // Go's `cancel()` on a context nothing is attached to.
        return reply::text(literals::DONE);
    };

    let conn = Arc::clone(&entry.conn);
    tokio::spawn(async move {
        // Go opens the stream on `streamCtx`, so a stop that lands during the
        // dial aborts the dial rather than waiting it out. Selecting on the
        // token is that, and it is biased so a token already cancelled by the
        // time this task is first polled wins outright.
        let opened = tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            opened = conn.open_event_stream(EVENT_WHITELIST, EVENT_CONNECTION_ID) => Some(opened),
        };
        match opened {
            Some(Ok(receiver)) => {
                // Takes the token and the ownership handle with it, and gives
                // ownership back, generation checked, on every exit path.
                run_event_stream(receiver, owner, generation, cancel).await;
            }
            Some(Err(err)) => {
                // Go's `logger.Println("event stream: " + err.Error())`
                // (`server.go:498`), which is DEBUG with an *empty* component
                // (`logger.go:234-238`); the explicit `comp` field is what keeps
                // the component column empty, and the target stays `sdkapp` so
                // `RUST_LOG` still names this module. On this path the receiver
                // is nil and the loop would panic on its first receive, so the
                // claim is handed straight back instead.
                tracing::debug!(target: "sdkapp", comp = "", "event stream: {err}");
                owner.release(generation);
            }
            // Abandoned before the stream existed. A stop already took
            // ownership away, so this release is a no-op; it is unconditional
            // because a cancellation that came from somewhere else, such as a
            // disconnect, must still not leave the claim behind.
            None => {
                owner.release(generation);
            }
        }
    });

    reply::text(literals::DONE)
}

/// `/api-sdk/stop_event_stream`: `stopEventStream(robotObj.ESN)` and `done`
/// (`server.go:506-509`).
///
/// Go deletes the registry entry unconditionally, whether or not an ESN has a
/// stream, and answers `done` either way. [`EventOwner::stop`] is that: it
/// releases ownership, clears the flag and zeroes the reading in one critical
/// section and hands the owner's token back, and the cancel happens here, once
/// that lock is gone, exactly as `stopEventStream` cancels after its unlock
/// (`robot.go:253-263`).
///
/// Releasing inside the stop rather than leaving it to the receiver is what
/// makes a begin arriving straight after a stop succeed. Waiting for the
/// receiver to wake up would refuse it, and the poller would read
/// `error: must start event stream` until it gave up.
///
/// [`EventOwner::stop`]: wirepod_core::EventOwner::stop
pub fn stop(entry: &RobotEntry) -> Response {
    if let Some(cancel) = entry.session.events.stop() {
        cancel.cancel();
    }
    reply::text(literals::DONE)
}

/// `/api-sdk/get_stim_status`: the reading, or the sentinel.
///
/// ```text
/// if isEventStreaming(robotObj.ESN) {
///     fmt.Fprint(w, stimState(robotObj.ESN))
///     return
/// }
/// fmt.Fprint(w, "error: must start event stream")
/// ```
///
/// `server.go:510-516`. The endpoint makes no RPC at all; it reads the value
/// the receiver publishes.
///
/// Both bodies are contract. The success body is a bare JSON number, Go's `%v`
/// on a `float32`, which [`go_format_f32`] reproduces: `0`, `0.1`, `0.75`, `1`,
/// with no quotes and no newline. The failure body must **not** parse as JSON,
/// because the dashboard's circuit breaker is a failing `response.json()`; see
/// [`literals::MUST_START_EVENT_STREAM`].
///
/// The flag is what is tested, not the reading, so a receiver that exited on
/// its own and left a stale value behind reports the sentinel rather than the
/// stale number.
pub fn status(entry: &RobotEntry) -> Response {
    if entry.session.events.is_streaming() {
        return reply::text(go_format_f32(entry.session.events.stim().value));
    }
    reply::text(literals::MUST_START_EVENT_STREAM)
}
