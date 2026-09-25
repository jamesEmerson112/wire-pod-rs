//! The connect-time `robot_state` stream: Go's connect-time event stream,
//! brought back and read.
//!
//! Go opens an event stream the moment it connects to a robot and never reads
//! it. This one opens at the same moment and ends with the same connection: the
//! registry starts it when it inserts a new entry, and
//! [`RobotRegistry::disconnect`](crate::robot::registry::RobotRegistry::disconnect)
//! stops it. It never dials and never touches the idle clock, so it keeps no
//! robot connected that would otherwise have been dropped. The receive loop
//! stores every sample in the session's
//! [`StateSlot`](crate::robot::observe::StateSlot) and logs only what
//! [`StateTracker`] says is worth a line.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::esn::{Esn, Generation};
use crate::logger::COMP_SDK;
use crate::robot::conn::{ConnError, EventItem, EventReceiver};
use crate::robot::observe::StateSlot;
use crate::robot::registry::RobotEntry;
use crate::robot::robotstate::StateTracker;

/// The event types the stream asks for.
///
/// Go's connect-time stream asks for `stimulation_info`; this one asks for
/// `robot_state`, which is the only way to learn whether a motion call moved
/// him, since the motion RPCs answer success either way.
pub const STATE_WHITELIST: &[&str] = &["robot_state"];

/// The `connection_id` the stream carries: none, exactly as Go's carries none.
///
/// The id is not cosmetic. The robot's gateway treats a stream as primary when
/// no other id is held (`checkConnectionID` in `cloud/message_handler.go`), and
/// when a primary stream ends it tells the engine the app has disconnected and
/// clears the id (`onDisconnect`). An empty id counts as primary but holds
/// nothing, so the dashboard's stim stream, tagged `wirepod`, still becomes
/// primary exactly as it does beside Go's stream. A new non-empty id here would
/// demote the stim stream to secondary.
pub const STATE_CONNECTION_ID: &str = "";

/// Why [`run_state_stream`] returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateLoopExit {
    /// The token was cancelled, which is what a disconnect does.
    Cancelled,
    /// The slot refused a write, because the claim was stopped or replaced
    /// before the cancellation arrived.
    Superseded,
    /// The robot closed the stream cleanly.
    StreamEnded,
    /// The receive failed.
    Failed(ConnError),
}

/// Claims `entry`'s state slot and opens the stream in a task of its own.
///
/// Answers `false` when a stream is already running. The task holds the
/// connection and the slot, never the entry, and gives the claim back on every
/// exit path. A disconnect during the open abandons it, as a stop during the
/// stim stream's open does.
pub fn spawn_state_stream(entry: &RobotEntry, window: Duration) -> bool {
    let cancel = CancellationToken::new();
    let slot = Arc::clone(&entry.session.state_stream);
    let Some(generation) = slot.claim(cancel.clone()) else {
        return false;
    };
    let conn = Arc::clone(&entry.conn);
    let esn = entry.esn.clone();
    tokio::spawn(async move {
        let opened = tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            opened = conn.open_event_stream(STATE_WHITELIST, STATE_CONNECTION_ID) => Some(opened),
        };
        match opened {
            Some(Ok(receiver)) => {
                run_state_stream(receiver, slot, generation, cancel, esn, window).await;
            }
            Some(Err(err)) => {
                tracing::debug!(
                    target: "sdkapp",
                    comp = COMP_SDK,
                    bot = esn.as_str(),
                    "state stream could not open: {err}",
                );
                slot.release(generation);
            }
            None => {
                slot.release(generation);
            }
        }
    });
    true
}

/// Stores every sample, writes the lines [`StateTracker`] asks for, and gives
/// the claim back on the way out.
///
/// The write is generation-checked, and a refused one ends the loop: the claim
/// it was writing for is gone.
pub async fn run_state_stream(
    mut receiver: Box<dyn EventReceiver>,
    slot: Arc<StateSlot>,
    generation: Generation,
    cancel: CancellationToken,
    esn: Esn,
    window: Duration,
) -> StateLoopExit {
    let bot = esn.as_str();
    let mut tracker = StateTracker::new(window);
    let exit = loop {
        let received = tokio::select! {
            biased;
            () = cancel.cancelled() => break StateLoopExit::Cancelled,
            received = receiver.next() => received,
        };

        match received {
            Ok(Some(EventItem::State(sample))) => {
                if !slot.write(generation, sample) {
                    break StateLoopExit::Superseded;
                }
                let report = tracker.note(sample, slot.motion_call(), Instant::now());
                for line in &report.lines {
                    tracing::debug!(target: "sdkapp", comp = COMP_SDK, bot = bot, "{line}");
                }
                if let Some(call) = report.finished {
                    slot.finish_motion_call(&call);
                }
            }
            Ok(Some(EventItem::Stim(_) | EventItem::Other)) => {}
            Ok(None) => {
                tracing::debug!(
                    target: "sdkapp",
                    comp = COMP_SDK,
                    bot = bot,
                    "state stream ended by the robot",
                );
                break StateLoopExit::StreamEnded;
            }
            Err(err) => {
                tracing::debug!(
                    target: "sdkapp",
                    comp = COMP_SDK,
                    bot = bot,
                    "state stream: {err}",
                );
                break StateLoopExit::Failed(err);
            }
        }
    };

    slot.release(generation);
    exit
}
