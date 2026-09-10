//! The stim receive loop.
//!
//! Go's `runEventStream` (`server.go:644-661`) pumps events until `Recv`
//! returns an error, then hands ownership back through a deferred,
//! generation-checked release. This is that loop with one addition: it selects
//! on a [`CancellationToken`] as well as on the receiver, so a stop ends it even
//! against a receiver that ignores cancellation entirely.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::esn::Generation;
use crate::robot::conn::{ConnError, EventItem, EventReceiver};
use crate::robot::session::{EventOwner, StimSample};

/// The event types the stim stream asks for (`robot.go:378`, `server.go:487`).
pub const EVENT_WHITELIST: &[&str] = &["stimulation_info"];

/// The `connection_id` the stim stream is tagged with (`server.go:490`).
///
/// The dead stream Go opens at connect time carries no connection id at all
/// (`robot.go:371-382`), and this slice does not open it, so this literal
/// belongs to the stim stream alone.
pub const EVENT_CONNECTION_ID: &str = "wirepod";

/// Why [`run_event_stream`] returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventLoopExit {
    /// The token was cancelled, which is what a `stop_event_stream` does.
    Cancelled,
    /// The robot closed the stream cleanly.
    StreamEnded,
    /// The receive failed.
    Failed(ConnError),
}

/// Pumps stim events until the stream ends, and gives ownership back on the way
/// out.
///
/// The release is generation-checked, so a receiver that has already been
/// superseded cannot clear the state of the one that replaced it. The write is
/// generation-checked for the same reason, and is skipped entirely when the
/// velocity is zero: Go decides a value is present with
/// `strings.Contains(fmt.Sprint(stimInfo), "velocity")` (`server.go:656-659`),
/// and proto3 omits zero-valued scalars from the text form, so the rule is
/// exactly `velocity != 0`.
pub async fn run_event_stream(
    mut receiver: Box<dyn EventReceiver>,
    owner: Arc<EventOwner>,
    generation: Generation,
    cancel: CancellationToken,
) -> EventLoopExit {
    let exit = loop {
        let received = tokio::select! {
            // A pending cancellation wins over a pending event, so a stop is
            // never delayed by a robot that is still talking.
            biased;
            () = cancel.cancelled() => break EventLoopExit::Cancelled,
            received = receiver.next() => received,
        };

        match received {
            Ok(Some(EventItem::Stim(stim))) => {
                if stim.velocity != 0.0 {
                    owner.write_stim(generation, StimSample::new(stim.value, stim.velocity));
                }
            }
            Ok(Some(EventItem::Other)) => {}
            Ok(None) => break EventLoopExit::StreamEnded,
            Err(err) => {
                // Go's only visibility into teardown, at `server.go:652`.
                // `logger.Println` is DEBUG (`logger.go:234-238`).
                tracing::debug!(target: "sdkapp", "event stream: {err}");
                break EventLoopExit::Failed(err);
            }
        }
    };

    owner.release(generation);
    exit
}
