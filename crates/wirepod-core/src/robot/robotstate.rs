//! The robot's own account of what it is doing, and what changed since last
//! time.
//!
//! The robot sends a `RobotState` event many times a second. Logging each one
//! would fill the 500-entry ring in well under a minute and evict everything
//! else, so nothing here logs a sample. [`StateTracker`] holds the last one and
//! answers only with what changed, which for a robot sitting still is nothing
//! at all.
//!
//! Even a change is not always worth a line: `LIFT_IN_POS` and `HEAD_IN_POS`
//! flicker throughout any movement. So ordinary changes are rate limited and
//! the ones that matter — delocalization, being picked up, falling, a cliff —
//! are not.

use std::time::{Duration, Instant};

/// One bit of `RobotState.status`, paired with the name a log line uses.
///
/// Taken from the `RobotStatus` enum in `messages.proto`. The values are a
/// bitfield rather than an enumeration, and `0x800` is unused by the robot.
pub const STATUS_FLAGS: [(u32, &str); 17] = [
    (0x1, "moving"),
    (0x2, "carrying_block"),
    (0x4, "picking_or_placing"),
    (0x8, "picked_up"),
    (0x10, "button_pressed"),
    (0x20, "falling"),
    (0x40, "animating"),
    (0x80, "pathing"),
    (0x100, "lift_in_pos"),
    (0x200, "head_in_pos"),
    (0x400, "calm_power_mode"),
    (0x1000, "on_charger"),
    (0x2000, "charging"),
    (0x4000, "cliff_detected"),
    (0x8000, "wheels_moving"),
    (0x10000, "being_held"),
    (0x20000, "motion_detected"),
];

/// The flags that always earn a line immediately, however recently the last one
/// was written.
///
/// Each one is either a safety event or a fact that invalidates what came
/// before: a fall, a cliff, a lift off the treads, and the pick-up that is
/// about to delocalize him.
const ALWAYS_REPORT: u32 = 0x8 | 0x20 | 0x4000 | 0x10000;

/// The gap an ordinary flag change has to clear before it is worth a line.
pub const QUIET_GAP: Duration = Duration::from_millis(500);

/// A decoded `RobotState` event.
///
/// The pose is only meaningful against `origin_id`: the robot has no world
/// frame, and two poses carrying different origins cannot be compared at all.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RobotStateSample {
    /// The `RobotStatus` bitfield.
    pub status: u32,
    /// Position in millimetres, in the frame `origin_id` names.
    pub x_mm: f32,
    pub y_mm: f32,
    /// Heading about +z, right-handed, so positive turns left.
    pub angle_rad: f32,
    /// Which coordinate frame the pose is in. Zero is none or unknown.
    pub origin_id: u32,
    /// The object the robot has localized against, or zero for none.
    ///
    /// Zero means he is running on dead reckoning alone, which the robot's own
    /// debug label calls "LocalizedTo: Odometry". Only the charger ever sets
    /// this; Vector does not localize to cubes.
    pub localized_to_object_id: i32,
}

impl RobotStateSample {
    /// The names of the flags that are set, in bit order.
    pub fn status_names(&self) -> Vec<&'static str> {
        STATUS_FLAGS
            .iter()
            .filter(|(bit, _)| self.status & bit != 0)
            .map(|(_, name)| *name)
            .collect()
    }
}

/// What changed between two samples.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StateChange {
    /// Flags that became set.
    pub gained: Vec<&'static str>,
    /// Flags that became clear.
    pub lost: Vec<&'static str>,
    /// The old and new origin, when the robot delocalized.
    pub origin: Option<(u32, u32)>,
    /// The old and new anchor object, when localization changed.
    pub localized_to: Option<(i32, i32)>,
}

impl StateChange {
    /// True when nothing in the sample moved.
    pub fn is_empty(&self) -> bool {
        self.gained.is_empty()
            && self.lost.is_empty()
            && self.origin.is_none()
            && self.localized_to.is_none()
    }
}

/// Holds the last sample and answers with what a log line should say.
///
/// One tracker belongs to one robot's event stream, which is what makes holding
/// the previous sample in a plain field correct: the stream is single-threaded
/// and ends when the robot goes away.
#[derive(Debug, Default)]
pub struct StateTracker {
    last: Option<RobotStateSample>,
    /// The sample the log last described. Changes are measured against this
    /// rather than against the previous sample, so a change the quiet gap held
    /// back is folded into the next line instead of being lost.
    reported: Option<RobotStateSample>,
    reported_at: Option<Instant>,
}

impl StateTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// The last sample seen, for a caller that wants the pose rather than the
    /// change.
    pub fn last(&self) -> Option<RobotStateSample> {
        self.last
    }

    /// Records `sample` and answers with the change worth reporting, if any.
    ///
    /// `now` is passed rather than read so the rate limit can be tested without
    /// waiting. The first sample of a stream always reports, because the flags
    /// it arrives with are news.
    pub fn note(&mut self, sample: RobotStateSample, now: Instant) -> Option<StateChange> {
        self.last = Some(sample);
        let Some(previous) = self.reported else {
            self.reported = Some(sample);
            self.reported_at = Some(now);
            return Some(StateChange {
                gained: sample.status_names(),
                lost: Vec::new(),
                origin: None,
                localized_to: None,
            });
        };

        let changed = previous.status ^ sample.status;
        let change = StateChange {
            gained: names_of(changed & sample.status),
            lost: names_of(changed & previous.status),
            origin: (previous.origin_id != sample.origin_id)
                .then_some((previous.origin_id, sample.origin_id)),
            localized_to: (previous.localized_to_object_id != sample.localized_to_object_id)
                .then_some((
                    previous.localized_to_object_id,
                    sample.localized_to_object_id,
                )),
        };
        if change.is_empty() {
            return None;
        }

        // A delocalization, a fall, a cliff or a lift is reported whenever it
        // happens. Everything else waits out the quiet gap, so that the head
        // and lift settling flags cannot crowd the ring.
        let urgent = change.origin.is_some() || changed & ALWAYS_REPORT != 0;
        if !urgent
            && let Some(reported_at) = self.reported_at
            && now.duration_since(reported_at) < QUIET_GAP
        {
            return None;
        }
        self.reported = Some(sample);
        self.reported_at = Some(now);
        Some(change)
    }
}

/// The names of the set bits of `bits`.
fn names_of(bits: u32) -> Vec<&'static str> {
    STATUS_FLAGS
        .iter()
        .filter(|(bit, _)| bits & bit != 0)
        .map(|(_, name)| *name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(millis: u64) -> Instant {
        // A fixed base so the arithmetic is exact rather than clock-dependent.
        Instant::now() + Duration::from_millis(millis)
    }

    fn sample(status: u32) -> RobotStateSample {
        RobotStateSample {
            status,
            ..RobotStateSample::default()
        }
    }

    #[test]
    fn the_bitfield_names_every_flag_the_robot_can_set() {
        // Driving: moving, wheels turning, head and lift settled.
        let driving = sample(0x1 | 0x8000 | 0x100 | 0x200);
        assert_eq!(
            driving.status_names(),
            ["moving", "lift_in_pos", "head_in_pos", "wheels_moving"]
        );
        assert!(sample(0).status_names().is_empty());
    }

    #[test]
    fn a_still_robot_produces_nothing_after_the_first_sample() {
        let mut tracker = StateTracker::new();
        let resting = sample(0x1000 | 0x2000);

        let first = tracker
            .note(resting, at(0))
            .expect("the first sample is always news");
        assert_eq!(first.gained, ["on_charger", "charging"]);

        // Every subsequent identical sample is silent, which is what keeps a
        // parked robot from evicting the ring.
        for millis in [10, 5_000, 60_000] {
            assert_eq!(tracker.note(resting, at(millis)), None);
        }
    }

    #[test]
    fn an_ordinary_flutter_waits_out_the_quiet_gap_but_a_cliff_does_not() {
        let mut tracker = StateTracker::new();
        tracker.note(sample(0x100), at(0)).expect("first sample");

        // head_in_pos flicking off well inside the gap is dropped.
        assert_eq!(tracker.note(sample(0x100 | 0x200), at(50)), None);

        // A cliff in the same window is reported regardless.
        let cliff = tracker
            .note(sample(0x100 | 0x200 | 0x4000), at(60))
            .expect("a cliff is always worth a line");
        assert_eq!(cliff.gained, ["head_in_pos", "cliff_detected"]);
        assert!(cliff.lost.is_empty());

        // And once the gap has passed, ordinary changes report again.
        let settled = tracker
            .note(sample(0x100), at(1_000))
            .expect("past the quiet gap");
        assert_eq!(settled.lost, ["head_in_pos", "cliff_detected"]);
    }

    #[test]
    fn delocalization_is_reported_the_instant_it_happens() {
        let mut tracker = StateTracker::new();
        let before = RobotStateSample {
            status: 0x1,
            origin_id: 3,
            localized_to_object_id: 7,
            ..RobotStateSample::default()
        };
        tracker.note(before, at(0)).expect("first sample");

        // A new origin with no flag change at all, immediately after the last
        // line: the robot has thrown away his coordinate frame and everything
        // measured in the old one is now meaningless.
        let after = RobotStateSample {
            origin_id: 4,
            localized_to_object_id: 0,
            ..before
        };
        let change = tracker
            .note(after, at(1))
            .expect("a new origin always reports");
        assert_eq!(change.origin, Some((3, 4)));
        assert_eq!(change.localized_to, Some((7, 0)));
        assert!(change.gained.is_empty() && change.lost.is_empty());
    }
}
