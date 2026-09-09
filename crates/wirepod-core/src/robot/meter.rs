//! The camera throughput meters behind the dashboard rate readout.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::esn::Esn;

/// Running totals for one robot camera feed.
///
/// The counters are atomic rather than mutex-guarded because they are written
/// from the frame loop: a 30 fps feed would otherwise take a lock thirty times
/// a second per robot and contend with every status poll (`robot.go:145-160`).
/// They are monotone and are never reset.
#[derive(Debug, Default)]
pub struct CamMeter {
    bytes: AtomicU64,
    frames: AtomicU64,
}

impl CamMeter {
    /// A meter reading zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Counts one received frame of `len` bytes.
    ///
    /// Both counters move before the decode, because a frame that failed to
    /// decode still crossed the wire and the measurement is of the link rather
    /// than of the picture (`server.go:770-776`).
    pub fn record(&self, len: u64) {
        self.bytes.fetch_add(len, Ordering::Relaxed);
        self.frames.fetch_add(1, Ordering::Relaxed);
    }

    /// The running totals.
    ///
    /// The pair is read non-atomically, exactly as Go reads it, which is
    /// acceptable because the dashboard differences two samples over time.
    pub fn read(&self) -> (u64, u64) {
        (
            self.bytes.load(Ordering::Relaxed),
            self.frames.load(Ordering::Relaxed),
        )
    }
}

/// Every robot camera meter, keyed by ESN and never pruned.
///
/// The meters live outside the per-robot entry so that totals survive an idle
/// eviction and a later reconnect, exactly as the Go `camMeters` map survives
/// `removeRobot`. Folding them into the evictable entry would make every
/// reconnect look like a server restart to the dashboard differencing logic.
/// Entries are never deleted, for the same reason the Go entries are not: a
/// frame loop that has already resolved its meter must keep counting into
/// something valid (`robot.go:163-166`).
#[derive(Debug, Default)]
pub struct CamMeters {
    meters: Mutex<HashMap<Esn, Arc<CamMeter>>>,
}

impl CamMeters {
    /// An empty set of meters.
    pub fn new() -> Self {
        Self::default()
    }

    /// The robot meter, created on first use.
    ///
    /// The frame loop resolves this once, before it starts receiving, and then
    /// counts without touching the map again.
    pub fn get(&self, esn: &Esn) -> Arc<CamMeter> {
        Arc::clone(self.lock().entry(esn.clone()).or_default())
    }

    /// The robot running totals, without creating a meter.
    ///
    /// An ESN nobody has streamed reads `(0, 0)`, because `net_probe` answers
    /// for robots whose camera has never been opened. Go reaches the value
    /// through `getCamMeter`, so a read of an unknown ESN inserts an entry
    /// (`robot.go:182-185`); this deliberately does not, and nothing observable
    /// depends on the insert.
    pub fn read(&self, esn: &Esn) -> (u64, u64) {
        let meter = self.lock().get(esn).map(Arc::clone);
        match meter {
            Some(meter) => meter.read(),
            None => (0, 0),
        }
    }

    /// How many robots have a meter. For tests.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether any robot has a meter.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<Esn, Arc<CamMeter>>> {
        // Poisoning is ignored on purpose, as it is for the ownership state.
        // Go's mutex has no such concept, and a panic elsewhere must not turn
        // every later meter read into a panic of its own. The map is only ever
        // inserted into, so a panic cannot leave it half-updated.
        self.meters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
