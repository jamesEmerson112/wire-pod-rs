//! Ownership of the two streams that watch the robot rather than drive him:
//! the connect-time `robot_state` stream and the nav map feed.
//!
//! Each is its own small type rather than one generic, for the same reason
//! [`CamOwner`](crate::robot::session::CamOwner) and
//! [`EventOwner`](crate::robot::session::EventOwner) are two: they want
//! different rules. The state stream is claimed once and lives as long as the
//! connection. The map feed lives only while someone is watching, so it carries
//! a lease.
//!
//! Both are generation-checked the way the stim stream is. A receiver that has
//! been stopped or superseded can neither write nor clear the state of the one
//! that replaced it.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::esn::Generation;
use crate::robot::navmap::NavMapFrame;
use crate::robot::robotstate::RobotStateSample;

/// A motion call the state stream should look for evidence of.
///
/// The motion logger stamps it as the call goes out. The state loop then
/// reports what the robot's movement flags did in the next few seconds, or
/// that they did nothing, which is the one question the transport status of a
/// motion call cannot answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MotionCall {
    /// The RPC, such as `DriveWheels`.
    pub rpc: &'static str,
    /// What was sent, already rendered for a log line.
    pub args: String,
    /// When it went out.
    pub at: Instant,
}

/// The claim a running stream holds.
#[derive(Debug)]
struct Claim {
    generation: Generation,
    cancel: CancellationToken,
}

impl Claim {
    fn holds(claim: Option<&Self>, generation: Generation) -> bool {
        claim.map(|claim| claim.generation) == Some(generation)
    }
}

#[derive(Debug, Default)]
struct StateInner {
    last_issued: Generation,
    current: Option<Claim>,
    latest: Option<RobotStateSample>,
    motion: Option<MotionCall>,
}

/// The connect-time `robot_state` stream: exclusive, for the life of the
/// connection.
#[derive(Debug, Default)]
pub struct StateSlot {
    inner: Mutex<StateInner>,
}

impl StateSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Claims the stream, or answers `None` when one is already running.
    ///
    /// A new stream starts with no sample, so a pose left behind by a stream
    /// that died is never read as his current one.
    pub fn claim(&self, cancel: CancellationToken) -> Option<Generation> {
        let mut inner = self.lock();
        if inner.current.is_some() {
            return None;
        }
        let generation = inner.last_issued.next();
        inner.last_issued = generation;
        inner.current = Some(Claim { generation, cancel });
        inner.latest = None;
        Some(generation)
    }

    /// The receiver's own way out. Changes nothing unless `generation` still
    /// holds the claim.
    pub fn release(&self, generation: Generation) -> bool {
        let mut inner = self.lock();
        if !Claim::holds(inner.current.as_ref(), generation) {
            return false;
        }
        inner.current = None;
        true
    }

    /// Ends whatever stream is running and forgets what it reported, handing
    /// back the token so the caller can cancel it.
    pub fn stop(&self) -> Option<CancellationToken> {
        let mut inner = self.lock();
        inner.latest = None;
        inner.motion = None;
        inner.current.take().map(|claim| claim.cancel)
    }

    pub fn is_running(&self) -> bool {
        self.lock().current.is_some()
    }

    /// Stores `sample` as the latest, if `generation` still holds the claim.
    pub fn write(&self, generation: Generation, sample: RobotStateSample) -> bool {
        let mut inner = self.lock();
        if !Claim::holds(inner.current.as_ref(), generation) {
            return false;
        }
        inner.latest = Some(sample);
        true
    }

    /// The last sample the robot sent, if a stream has reported one.
    pub fn latest(&self) -> Option<RobotStateSample> {
        self.lock().latest
    }

    /// Records a motion call going out. A later call replaces an earlier one,
    /// because the robot can only answer the most recent command.
    pub fn note_motion_call(&self, call: MotionCall) {
        self.lock().motion = Some(call);
    }

    /// The motion call being watched for, if any.
    pub fn motion_call(&self) -> Option<MotionCall> {
        self.lock().motion.clone()
    }

    /// Stops watching for `call`, unless a newer call has replaced it since it
    /// was read, in which case the newer one keeps its window.
    pub fn finish_motion_call(&self, call: &MotionCall) -> bool {
        let mut inner = self.lock();
        if inner.motion.as_ref() != Some(call) {
            return false;
        }
        inner.motion = None;
        true
    }

    fn lock(&self) -> MutexGuard<'_, StateInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A map as it arrived.
#[derive(Clone, Debug, PartialEq)]
pub struct ReceivedMap {
    pub frame: NavMapFrame,
    /// Wall-clock Unix milliseconds at arrival.
    pub received_ms: i64,
}

#[derive(Debug, Default)]
struct MapInner {
    last_issued: Generation,
    current: Option<Claim>,
    latest: Option<Arc<ReceivedMap>>,
    /// Whether the feed holding the current or last claim has stored a map.
    /// The last map outlives its feed, so a map being present says nothing
    /// about whether the feed running now is receiving anything.
    delivered: bool,
    error: Option<String>,
    watched_at: Duration,
}

/// The nav map feed: runs only while someone is watching.
///
/// Every snapshot request renews the lease with [`MapSlot::watch`], and the
/// feed task ends itself once [`MapSlot::lapsed`] says nobody has asked for a
/// while, so a closed tab cleans up without having to say goodbye.
///
/// The last map outlives the feed that brought it. The robot broadcasts his map
/// only when it has changed since his last broadcast, and a new subscription
/// does not count as a change, so a fresh feed to a robot sitting still
/// receives nothing at all. Keeping the last map is what lets a page that comes
/// back show it, with its age, rather than wait for him to move. Only
/// [`MapSlot::stop`], which a disconnect calls, forgets it.
#[derive(Debug, Default)]
pub struct MapSlot {
    inner: Mutex<MapInner>,
}

impl MapSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Renews the lease at `now`, a reading of the registry's clock.
    pub fn watch(&self, now: Duration) {
        self.lock().watched_at = now;
    }

    /// True when the lease was last renewed more than `lease` before `now`.
    pub fn lapsed(&self, now: Duration, lease: Duration) -> bool {
        now.saturating_sub(self.lock().watched_at) > lease
    }

    /// Claims the feed, or answers `None` when one is already running. A new
    /// claim clears the previous feed's error.
    pub fn claim(&self, cancel: CancellationToken) -> Option<Generation> {
        let mut inner = self.lock();
        if inner.current.is_some() {
            return None;
        }
        let generation = inner.last_issued.next();
        inner.last_issued = generation;
        inner.current = Some(Claim { generation, cancel });
        inner.error = None;
        inner.delivered = false;
        Some(generation)
    }

    /// The feed's own way out, keeping the last map. Changes nothing unless
    /// `generation` still holds the claim.
    pub fn release(&self, generation: Generation) -> bool {
        let mut inner = self.lock();
        if !Claim::holds(inner.current.as_ref(), generation) {
            return false;
        }
        inner.current = None;
        true
    }

    /// Ends whatever feed is running and forgets the map, handing back the
    /// token so the caller can cancel it.
    pub fn stop(&self) -> Option<CancellationToken> {
        let mut inner = self.lock();
        inner.latest = None;
        inner.error = None;
        inner.current.take().map(|claim| claim.cancel)
    }

    pub fn is_running(&self) -> bool {
        self.lock().current.is_some()
    }

    /// Stores `map` as the latest and clears any error, if `generation` still
    /// holds the claim.
    pub fn write(&self, generation: Generation, map: ReceivedMap) -> bool {
        let mut inner = self.lock();
        if !Claim::holds(inner.current.as_ref(), generation) {
            return false;
        }
        inner.latest = Some(Arc::new(map));
        inner.delivered = true;
        inner.error = None;
        true
    }

    /// Records why the feed failed, if `generation` still holds the claim.
    pub fn fail(&self, generation: Generation, error: String) -> bool {
        let mut inner = self.lock();
        if !Claim::holds(inner.current.as_ref(), generation) {
            return false;
        }
        inner.error = Some(error);
        true
    }

    /// True once the feed holding the current or last claim has stored a map
    /// of its own, rather than showing one an earlier feed left.
    pub fn delivered(&self) -> bool {
        self.lock().delivered
    }

    /// The last map the robot sent.
    pub fn latest(&self) -> Option<Arc<ReceivedMap>> {
        self.lock().latest.clone()
    }

    /// Why the current or last feed failed, if it did.
    pub fn error(&self) -> Option<String> {
        self.lock().error.clone()
    }

    fn lock(&self) -> MutexGuard<'_, MapInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::robot::navmap::NavMapInfo;

    fn frame(origin_id: u32) -> NavMapFrame {
        NavMapFrame {
            origin_id,
            info: NavMapInfo {
                root_depth: 4,
                root_size_mm: 128.0,
                root_center_x: 0.0,
                root_center_y: 0.0,
            },
            quads: Vec::new(),
        }
    }

    #[test]
    fn a_state_stream_is_exclusive_and_a_stale_receiver_cannot_write() {
        let slot = StateSlot::new();
        let first = slot
            .claim(CancellationToken::new())
            .expect("the first claim succeeds");
        assert_eq!(slot.claim(CancellationToken::new()), None);

        assert!(slot.write(first, RobotStateSample::default()));
        assert!(slot.stop().is_some());
        assert_eq!(slot.latest(), None, "a stop forgets the last sample");

        // The stopped receiver has not noticed yet and tries to report.
        assert!(!slot.write(first, RobotStateSample::default()));
        assert!(!slot.release(first));

        let second = slot
            .claim(CancellationToken::new())
            .expect("a stopped slot can be claimed again");
        assert_ne!(first, second);
        assert!(
            !slot.release(first),
            "the old generation cannot free the new claim"
        );
        assert!(slot.release(second));
    }

    #[test]
    fn a_new_state_stream_does_not_inherit_a_dead_one_s_pose() {
        let slot = StateSlot::new();
        let dead = slot
            .claim(CancellationToken::new())
            .expect("the first claim succeeds");
        assert!(slot.write(dead, RobotStateSample::default()));
        // The robot ended the stream; the loop gave its claim back.
        assert!(slot.release(dead));
        slot.claim(CancellationToken::new())
            .expect("a released slot can be claimed again");
        assert_eq!(slot.latest(), None);
    }

    #[test]
    fn a_map_kept_from_an_earlier_feed_is_not_a_delivery() {
        let slot = MapSlot::new();
        let first = slot
            .claim(CancellationToken::new())
            .expect("the first claim succeeds");
        assert!(!slot.delivered());
        assert!(slot.write(
            first,
            ReceivedMap {
                frame: frame(3),
                received_ms: 1,
            },
        ));
        assert!(slot.delivered());
        assert!(slot.release(first));

        // A new feed shows the old map but has delivered nothing of its own.
        slot.claim(CancellationToken::new())
            .expect("a released slot can be claimed again");
        assert!(slot.latest().is_some());
        assert!(!slot.delivered());
    }

    #[test]
    fn a_newer_motion_call_keeps_its_window() {
        let slot = StateSlot::new();
        let older = MotionCall {
            rpc: "DriveWheels",
            args: "lw=50 rw=50".to_owned(),
            at: Instant::now(),
        };
        slot.note_motion_call(older.clone());
        let newer = MotionCall {
            rpc: "MoveHead",
            args: "speed=2".to_owned(),
            at: Instant::now(),
        };
        slot.note_motion_call(newer.clone());

        // The loop finishes the window it read before the newer call landed.
        assert!(!slot.finish_motion_call(&older));
        assert_eq!(slot.motion_call(), Some(newer.clone()));
        assert!(slot.finish_motion_call(&newer));
        assert_eq!(slot.motion_call(), None);
    }

    #[test]
    fn the_map_lease_lapses_and_the_last_map_outlives_the_feed() {
        let slot = MapSlot::new();
        let lease = Duration::from_secs(15);
        slot.watch(Duration::from_secs(100));
        assert!(!slot.lapsed(Duration::from_secs(115), lease));
        assert!(slot.lapsed(Duration::from_secs(116), lease));

        let feed = slot
            .claim(CancellationToken::new())
            .expect("the first claim succeeds");
        assert!(slot.fail(feed, "rpc error".to_owned()));
        assert!(slot.write(
            feed,
            ReceivedMap {
                frame: frame(3),
                received_ms: 1,
            },
        ));
        assert_eq!(slot.error(), None, "a map clears the error");

        // The lease lapses and the feed lets go, but the map stays for a page
        // that comes back.
        assert!(slot.release(feed));
        assert!(!slot.is_running());
        assert_eq!(
            slot.latest().map(|map| map.frame.origin_id),
            Some(3),
            "a lapsed feed keeps its last map",
        );

        // A disconnect forgets it.
        assert!(slot.stop().is_none());
        assert_eq!(slot.latest(), None);
    }
}
