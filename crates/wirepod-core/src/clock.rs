//! The injectable monotonic clock the pinger measures elapsed time against.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// A monotonic clock, read as the time elapsed since the clock started.
///
/// The reading is a [`Duration`] rather than an [`Instant`] because an
/// `Instant` cannot be fabricated: it has no public constructor and no way to
/// be moved backwards, so a [`ManualClock`] could never hand one out at a
/// chosen point. Only differences between two readings from the same clock are
/// meaningful, and the zero point is arbitrary.
///
/// A `Duration` is enough for everything the SDK-app slice measures. Go's
/// pinger works in whole seconds since the last conn check
/// (`jdocspinger.go:136-149`) and nothing in the bot-status projection reads a
/// wall clock, so no calendar accessor is defined here.
pub trait Clock: Send + Sync {
    /// The time elapsed since this clock started.
    fn now(&self) -> Duration;

    /// Whole seconds from an earlier reading of this clock to now.
    ///
    /// This is Go's `TimeSinceLastCheck` counter, which a one-second ticker
    /// increments and a conn check resets to zero. A reading from the future,
    /// which a [`ManualClock`] can be set to produce, counts as zero rather
    /// than as a negative age, because Go's counter never goes below zero.
    fn secs_since(&self, earlier: Duration) -> i64 {
        i64::try_from(self.now().saturating_sub(earlier).as_secs()).unwrap_or(i64::MAX)
    }
}

/// A [`Clock`] reading the real monotonic clock, starting from its creation.
#[derive(Clone, Copy, Debug)]
pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    /// Starts a clock whose zero point is now.
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
}

/// A [`Clock`] a test drives by hand.
///
/// Reads and writes go through a [`Mutex`], so the clock is `Sync` and can be
/// shared through an [`Arc`](std::sync::Arc) between the code under test and
/// the test that advances it.
#[derive(Debug, Default)]
pub struct ManualClock {
    now: Mutex<Duration>,
}

impl ManualClock {
    /// Starts a clock reading zero.
    pub fn new() -> Self {
        Self::at(Duration::ZERO)
    }

    /// Starts a clock reading `now`.
    pub fn at(now: Duration) -> Self {
        Self {
            now: Mutex::new(now),
        }
    }

    /// Moves the clock to `now`, forwards or backwards.
    pub fn set(&self, now: Duration) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) = now;
    }

    /// Moves the clock forward by `delta`.
    pub fn advance(&self, delta: Duration) {
        let mut guard = self.now.lock().unwrap_or_else(PoisonError::into_inner);
        *guard = guard.saturating_add(delta);
    }

    /// Moves the clock forward by whole seconds.
    pub fn advance_secs(&self, secs: u64) {
        self.advance(Duration::from_secs(secs));
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
