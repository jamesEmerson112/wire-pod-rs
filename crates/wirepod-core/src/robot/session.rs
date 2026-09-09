//! The two per-robot stream ownership state machines.
//!
//! The camera is preemptive and the stim stream is exclusive, which is a
//! deliberate asymmetry: the camera has no stop protocol, so a reloaded image
//! element must be able to take the feed, whereas the stim graph has an explicit
//! `stop_event_stream` and a second begin is a double click that should cost
//! nothing (`robot.go:208-217`).
//!
//! Both wrap a [`std::sync::Mutex`] and are `Send + Sync`. No method awaits, no
//! method takes a closure, and every method returns owned values, so a guard has
//! nowhere to escape to.
//!
//! [`SdkSession`] is the per-robot record those two hang off, together with the
//! camera operation lock the guard in [`crate::robot::cam`] holds across the
//! settle and the enable RPC.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use tokio::sync::{Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard};
use tokio_util::sync::CancellationToken;

use crate::esn::Generation;

/// Camera ownership for one robot. Preemptive: a new claim displaces whatever
/// handler held the feed and reports that it did so, and only the current owner
/// may release (`robot.go:99-131`).
#[derive(Debug, Default)]
pub struct CamOwner {
    state: Mutex<CamState>,
}

#[derive(Debug, Default)]
struct CamState {
    last_issued: Generation,
    /// The registry entry: `Some` while some handler owns the feed.
    current: Option<CamEntry>,
    streaming: bool,
}

/// The owning handler's generation and the token that stops it, which is Go's
/// `camStream{gen, cancel}` (`robot.go:39-45`).
#[derive(Debug)]
struct CamEntry {
    generation: Generation,
    cancel: CancellationToken,
}

impl CamOwner {
    /// A robot whose camera has never been claimed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes the caller the owner, displacing any previous one.
    ///
    /// Returns the new generation and the displaced owner's cancellation token,
    /// which the caller cancels once this lock is gone. Handing the token back
    /// rather than a bare "someone was displaced" flag is what keeps the read of
    /// the previous owner inside the same critical section as the claim, exactly
    /// as `claimCamStream` reads `prev` under `robotsMu` and cancels it after
    /// the unlock (`robot.go:105-116`). Reading the previous owner in a second
    /// call would let two claims race and cancel each other's replacement.
    ///
    /// A returned token also means the caller pays the settle, because the robot
    /// needs a moment to drop the camera feed that was just cancelled.
    pub fn claim(&self, cancel: CancellationToken) -> (Generation, Option<CancellationToken>) {
        let mut state = self.lock();
        let displaced = state.current.take().map(|entry| entry.cancel);
        let generation = state.last_issued.next();
        state.last_issued = generation;
        state.current = Some(CamEntry { generation, cancel });
        state.streaming = true;
        (generation, displaced)
    }

    /// Drops ownership if `generation` still holds it, and reports whether it
    /// did. A superseded handler gets `false` and changes nothing, so it never
    /// turns the camera off underneath the handler that displaced it.
    pub fn release(&self, generation: Generation) -> bool {
        let mut state = self.lock();
        if state.current.as_ref().map(|entry| entry.generation) != Some(generation) {
            return false;
        }
        state.current = None;
        state.streaming = false;
        true
    }

    /// Clears the streaming flag and cancels the owner, but keeps the entry.
    ///
    /// This mirrors `stopCamStream` (`robot.go:136-144`), which deliberately
    /// does not delete the registry entry: the departing handler's own release
    /// is what deletes it and issues the disable, under the operation lock.
    /// Clearing the flag alone would not end the feed, because a handler only
    /// samples it after a receive returns and a robot sending no frames never
    /// returns one, so the token is cancelled too. The cancel happens once the
    /// state lock is released, as Go cancels after its unlock.
    pub fn stop(&self) {
        let owner = {
            let mut state = self.lock();
            state.streaming = false;
            state.current.as_ref().map(|entry| entry.cancel.clone())
        };
        if let Some(cancel) = owner {
            cancel.cancel();
        }
    }

    /// Whether a feed is marked as running.
    pub fn is_streaming(&self) -> bool {
        self.lock().streaming
    }

    /// The generation that currently owns the feed, if any.
    pub fn current(&self) -> Option<Generation> {
        self.lock().current.as_ref().map(|entry| entry.generation)
    }

    fn lock(&self) -> MutexGuard<'_, CamState> {
        // Poisoning is ignored on purpose. Go's mutex has no such concept, so a
        // panic anywhere under this lock would otherwise turn every later
        // request for this robot into a permanent panic where the Go server
        // carries on. Every method here replaces the whole of the state it
        // touches, so a half-written value cannot survive a panic.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// One stimulation reading published by the owning receiver.
///
/// Go keeps only the value on the robot record (`robot.go:283-297`); the
/// velocity is carried alongside it here because it is what decides whether the
/// reading counts at all. Go tests that with
/// `strings.Contains(fmt.Sprint(stimInfo), "velocity")` (`server.go:656-659`),
/// and proto3 omits zero-valued scalars from the text form, so the rule is
/// exactly `velocity != 0.0`. That rule belongs to the caller that turns an
/// event into a sample; [`EventOwner::write_stim`] is the raw, unconditional
/// write.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StimSample {
    /// The stimulation level the dashboard graphs.
    pub value: f32,
    /// The velocity that admitted the reading.
    pub velocity: f32,
}

impl StimSample {
    /// The reading a stopped stream reports.
    pub const ZERO: Self = Self {
        value: 0.0,
        velocity: 0.0,
    };

    /// A reading with the given value and velocity.
    pub const fn new(value: f32, velocity: f32) -> Self {
        Self { value, velocity }
    }
}

/// Stim stream ownership for one robot. Exclusive: a claim while the stream is
/// owned is refused rather than displacing the incumbent, and a stop releases
/// ownership in the same critical section that clears the flag and zeroes the
/// reading (`robot.go:218-263`).
#[derive(Debug, Default)]
pub struct EventOwner {
    state: Mutex<EventState>,
}

#[derive(Debug, Default)]
struct EventState {
    last_issued: Generation,
    current: Option<Generation>,
    streaming: bool,
    stim: StimSample,
}

impl EventOwner {
    /// A robot whose stim stream has never been claimed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes ownership if nobody holds it, returning the new generation.
    ///
    /// `None` means the stream is already owned. Folding the "already running"
    /// test into the claim closes the check-then-act window where two
    /// simultaneous begins could both read false.
    pub fn claim(&self) -> Option<Generation> {
        let mut state = self.lock();
        if state.current.is_some() {
            return None;
        }
        let generation = state.last_issued.next();
        state.last_issued = generation;
        state.current = Some(generation);
        state.streaming = true;
        Some(generation)
    }

    /// Releases ownership, clears the flag and zeroes the reading, in one
    /// critical section.
    ///
    /// Releasing here rather than leaving it to the receiver is what lets a
    /// claim arriving immediately after a stop succeed. Zeroing in the same
    /// section means there is no moment where the reading is a stale non-zero
    /// value while the stream is already stopped (`robot.go:253-263`).
    pub fn stop(&self) {
        let mut state = self.lock();
        state.current = None;
        state.streaming = false;
        state.stim = StimSample::ZERO;
    }

    /// Drops ownership if `generation` still holds it, and reports whether it
    /// did.
    ///
    /// This is the receiver's own way out, taken on every exit path of the
    /// stim loop. A superseded receiver gets `false` and changes nothing, so it
    /// cannot clear the state of the receiver that replaced it, and a receiver
    /// unwinding from a [`EventOwner::stop`] gets `false` too, because the stop
    /// already took ownership away.
    ///
    /// Unlike [`EventOwner::stop`] this leaves the reading alone, which is what
    /// Go does: `releaseEventStream` deletes the entry and clears the flag
    /// (`robot.go:236-246`) while only `stopEventStream` zeroes the value
    /// (`robot.go:258`). The difference is not observable, because
    /// `get_stim_status` reads the value only while the flag is set, but it is
    /// reproduced rather than tidied up.
    pub fn release(&self, generation: Generation) -> bool {
        let mut state = self.lock();
        if state.current != Some(generation) {
            return false;
        }
        state.current = None;
        state.streaming = false;
        true
    }

    /// Publishes `sample` if `generation` still owns the stream, and reports
    /// whether it did. A receiver still unwinding from a cancelled receive
    /// cannot overwrite the value its replacement just published.
    pub fn write_stim(&self, generation: Generation, sample: StimSample) -> bool {
        let mut state = self.lock();
        if state.current != Some(generation) {
            return false;
        }
        state.stim = sample;
        true
    }

    /// The last published reading, or [`StimSample::ZERO`].
    pub fn stim(&self) -> StimSample {
        self.lock().stim
    }

    /// Whether a receiver is marked as running.
    pub fn is_streaming(&self) -> bool {
        self.lock().streaming
    }

    fn lock(&self) -> MutexGuard<'_, EventState> {
        // Poisoning is ignored, for the reason given on `CamOwner::lock`.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Everything the SDK app keeps per robot for the duration of a connection.
///
/// The two ownership state machines are synchronous, so a handler can read them
/// without a runtime. The camera operation lock is the one piece that has to be
/// asynchronous, because it is genuinely held across the settle and the enable
/// RPC exactly as Go holds a `sync.Mutex` across the same work
/// (`server.go:684-695`, `server.go:700-707`).
///
/// Lock order is fixed: the camera operation lock first, ownership state second.
/// Nothing here takes the ownership lock before the operation lock, which is the
/// rule Go writes out at `robot.go:64-66`.
#[derive(Debug)]
pub struct SdkSession {
    /// Camera ownership, preemptive.
    pub cam: CamOwner,
    /// Stim stream ownership, exclusive. The receive loop takes an `Arc` of it
    /// into its own task, so it is shared rather than owned inline.
    pub events: Arc<EventOwner>,
    cam_op: AsyncMutex<()>,
    last_touch: Mutex<Instant>,
}

impl Default for SdkSession {
    fn default() -> Self {
        Self::new()
    }
}

impl SdkSession {
    /// A session for a robot that has just connected.
    pub fn new() -> Self {
        Self {
            cam: CamOwner::new(),
            events: Arc::new(EventOwner::new()),
            cam_op: AsyncMutex::new(()),
            last_touch: Mutex::new(Instant::now()),
        }
    }

    /// Marks the robot as used now, which is what resets the idle timer.
    ///
    /// The sweeper that reads this arrives with the registry, and so does the
    /// injectable clock; until then the reading is a plain [`Instant`].
    pub fn touch(&self) {
        *self.lock_touch() = Instant::now();
    }

    /// When the robot was last used.
    pub fn last_touch(&self) -> Instant {
        *self.lock_touch()
    }

    /// Takes this robot's camera operation lock.
    ///
    /// Held across "claim ownership and turn the camera on" and across "give
    /// ownership back and turn the camera off", so a departing handler's disable
    /// cannot land between a replacement's claim and its enable and leave the
    /// camera off under a live feed (`robot.go:52-61`). One lock per robot, so
    /// an unresponsive robot cannot stall another robot's camera.
    pub(crate) async fn lock_cam_op(&self) -> AsyncMutexGuard<'_, ()> {
        self.cam_op.lock().await
    }

    fn lock_touch(&self) -> MutexGuard<'_, Instant> {
        // Poisoning is ignored, for the reason given on `CamOwner::lock`.
        self.last_touch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
