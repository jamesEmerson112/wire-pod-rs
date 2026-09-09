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

use std::sync::{Mutex, MutexGuard};

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
    current: Option<Generation>,
    streaming: bool,
}

impl CamOwner {
    /// A robot whose camera has never been claimed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes the caller the owner, displacing any previous one.
    ///
    /// Returns the new generation and whether a live owner was displaced. The
    /// caller pays the settle only when it displaced someone, because the robot
    /// needs a moment to drop the camera feed that was just cancelled.
    /// Cancelling the displaced owner is the caller's job and arrives with the
    /// camera guard in C6.
    pub fn claim(&self) -> (Generation, bool) {
        let mut state = self.lock();
        let displaced = state.current.is_some();
        let generation = state.last_issued.next();
        state.last_issued = generation;
        state.current = Some(generation);
        state.streaming = true;
        (generation, displaced)
    }

    /// Drops ownership if `generation` still holds it, and reports whether it
    /// did. A superseded handler gets `false` and changes nothing, so it never
    /// turns the camera off underneath the handler that displaced it.
    pub fn release(&self, generation: Generation) -> bool {
        let mut state = self.lock();
        if state.current != Some(generation) {
            return false;
        }
        state.current = None;
        state.streaming = false;
        true
    }

    /// Clears the streaming flag but keeps the owner entry.
    ///
    /// This mirrors `stopCamStream` (`robot.go:136-144`), which deliberately
    /// does not delete the registry entry: the departing handler's own release
    /// is what deletes it and issues the disable, under the operation lock.
    pub fn stop(&self) {
        self.lock().streaming = false;
    }

    /// Whether a feed is marked as running.
    pub fn is_streaming(&self) -> bool {
        self.lock().streaming
    }

    /// The generation that currently owns the feed, if any.
    pub fn current(&self) -> Option<Generation> {
        self.lock().current
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
