//! The robot's own account of what it is doing, and the few changes in it that
//! are worth a log line.
//!
//! The robot sends a `RobotState` event many times a second, and an awake
//! Vector animates constantly, so his head and lift flags flicker with every
//! idle animation. The log ring holds 500 entries of every level and the voice
//! pipeline needs them too, so [`StateTracker`] writes a line in only two
//! cases. An urgent change is written at once. A change in the flags that
//! answer a motion call is written only in the few seconds after it, because
//! that is the question the call's own answer cannot settle: the motion RPCs
//! report success whether he moved or not. Everything else is stored as the
//! latest sample and never logged, so a parked or idling robot writes nothing
//! at all.

use std::time::{Duration, Instant};

use crate::robot::observe::MotionCall;

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

/// `picked_up`, `falling`, `cliff_detected` and `being_held`: the flags written
/// the moment they change, inside a motion window or not.
///
/// Each is either a safety event or the start of a delocalization, and none of
/// them flickers with an idle animation.
const URGENT: u32 = 0x8 | 0x20 | 0x4000 | 0x10000;

/// `moving`, `animating`, `pathing`, `lift_in_pos`, `head_in_pos` and
/// `wheels_moving`: the flags a motion window can watch.
const MOVEMENT: u32 = 0x1 | 0x40 | 0x80 | 0x100 | 0x200 | 0x8000;

/// `moving`, `animating`, `pathing` and `wheels_moving`: the movement flags
/// that are set only while something is happening. The two `_in_pos` flags are
/// set at rest.
const ACTIVE: u32 = 0x1 | 0x40 | 0x80 | 0x8000;

/// The shortest gap between two movement lines inside one motion window.
///
/// A change held back by the gap is folded into the next line, which is
/// measured against what the log last said, or written when the window ends
/// if no line comes first. A flicker that reverts inside the gap is never
/// written at all.
pub const QUIET_GAP: Duration = Duration::from_millis(500);

/// How long after its call a window must have been open for the call that
/// replaces it to earn it a verdict. One replaced sooner gets none, because he
/// may simply not have reacted yet.
pub const SUPERSEDED_MINIMUM: Duration = Duration::from_millis(500);

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
    /// The object the robot has localized against, or -1 for none.
    ///
    /// -1 means he is running on dead reckoning alone, which the robot's own
    /// debug label calls "LocalizedTo: Odometry"; zero is an object id like any
    /// other, and it is also what the derived `Default` leaves here. Only the
    /// charger ever sets this; Vector does not localize to cubes.
    pub localized_to_object_id: i32,
}

impl RobotStateSample {
    /// The names of the flags that are set, in bit order.
    pub fn status_names(&self) -> Vec<&'static str> {
        names_of(self.status)
    }

    /// The object he is localized to, or `None` on dead reckoning alone.
    pub fn localized_to(&self) -> Option<i32> {
        (self.localized_to_object_id >= 0).then_some(self.localized_to_object_id)
    }
}

/// What one sample changed that is worth a line.
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
    /// True when nothing in the sample is worth a line.
    pub fn is_empty(&self) -> bool {
        self.gained.is_empty()
            && self.lost.is_empty()
            && self.origin.is_none()
            && self.localized_to.is_none()
    }

    /// The lines this change reads as, given the sample that carried it.
    ///
    /// Three kinds rather than one, because they answer different questions: a
    /// new origin says everything measured before it is now meaningless, a
    /// localization change says whether his pose is being corrected or has been
    /// drifting, and the flag line says what he is doing.
    fn lines(&self, sample: &RobotStateSample) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some((was, now)) = self.origin {
            lines.push(format!("state delocalized: origin {was} -> {now}"));
        }
        if let Some((was, now)) = self.localized_to {
            lines.push(if now < 0 {
                format!("state on odometry alone: no longer localized to object {was}")
            } else {
                format!("state localized to object {now}")
            });
        }
        if !self.gained.is_empty() || !self.lost.is_empty() {
            let mut line = String::from("state");
            if !self.gained.is_empty() {
                line.push_str(&format!(" +[{}]", self.gained.join(" ")));
            }
            if !self.lost.is_empty() {
                line.push_str(&format!(" -[{}]", self.lost.join(" ")));
            }
            line.push_str(&format!(
                " pose=({:.1}, {:.1}) heading={:.3}rad origin={}",
                sample.x_mm, sample.y_mm, sample.angle_rad, sample.origin_id,
            ));
            lines.push(line);
        }
        lines
    }
}

/// What the tracker wants done after one sample.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StateReport {
    /// The log lines to write, oldest fact first.
    pub lines: Vec<String>,
    /// The motion call whose window has closed, for the caller to hand to
    /// [`StateSlot::finish_motion_call`](crate::robot::observe::StateSlot::finish_motion_call).
    pub finished: Option<MotionCall>,
}

/// The motion window being watched.
#[derive(Debug)]
struct Window {
    call: MotionCall,
    /// The flags that answer the call, from [`evidence`]. The flag fields
    /// below hold only these.
    evidence: u32,
    /// The flags just before the call could have had any effect.
    start: u32,
    /// The flags in the latest sample inside the window.
    latest: u32,
    /// The flags the log last described, which starts as `start`.
    reported: u32,
    /// When this window last wrote a movement line.
    reported_at: Option<Instant>,
    /// Whether any sample in the window differed from `start`.
    changed: bool,
}

impl Window {
    /// Adds whatever the quiet gap is still holding back to `gained` and
    /// `lost`, so a window never ends with the log behind the robot.
    fn flush(&self, gained: &mut u32, lost: &mut u32) {
        let held = self.latest ^ self.reported;
        *gained |= held & self.latest;
        *lost |= held & self.reported;
    }
}

/// Decides, sample by sample, which lines the state stream writes.
///
/// One tracker belongs to one stream, which is what makes holding the previous
/// sample in a plain field correct: the stream is single-threaded and ends
/// with the connection. The motion call and the time are passed in rather than
/// read, so every rule here can be tested without a runtime or a wait.
#[derive(Debug)]
pub struct StateTracker {
    window: Duration,
    last: Option<RobotStateSample>,
    watching: Option<Window>,
}

impl StateTracker {
    /// A tracker that watches each motion call for `window` after it went out.
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            last: None,
            watching: None,
        }
    }

    /// Records `sample` and answers with the lines it earns.
    ///
    /// `call` is the session's current motion call, if any. The first sample of
    /// a stream earns nothing, because there is nothing to compare it with.
    ///
    /// A window watches only the flags that answer its call, from
    /// [`evidence`]. A change in them may still be his own motion rather than
    /// the call's effect; what the window can say for certain is the opposite
    /// case, that none of them changed at all.
    pub fn note(
        &mut self,
        sample: RobotStateSample,
        call: Option<MotionCall>,
        now: Instant,
    ) -> StateReport {
        let previous = self.last.replace(sample);
        let mut report = StateReport::default();
        let mut change = StateChange::default();
        let (mut gained, mut lost) = (0, 0);

        if let Some(previous) = previous {
            change.origin = (previous.origin_id != sample.origin_id)
                .then_some((previous.origin_id, sample.origin_id));
            // Every negative id means the same thing, so only a change of
            // anchor counts.
            change.localized_to = (previous.localized_to() != sample.localized_to()).then_some((
                previous.localized_to_object_id,
                sample.localized_to_object_id,
            ));
            let flipped = (previous.status ^ sample.status) & URGENT;
            gained |= flipped & sample.status;
            lost |= flipped & previous.status;
        }

        match call {
            Some(call) => {
                // A newer call replaces the window of an older one, because the
                // robot can only answer the most recent command. The older one
                // is judged at once if it was open long enough to be answered,
                // and as if it had closed if it had run its full length.
                if let Some(older) = self.watching.take_if(|window| window.call != call) {
                    older.flush(&mut gained, &mut lost);
                    let open = call.at.saturating_duration_since(older.call.at);
                    if !older.changed && open >= SUPERSEDED_MINIMUM {
                        let newer = (open < self.window).then_some(&call);
                        report.lines.push(verdict(&older, newer));
                    }
                }

                if now.saturating_duration_since(call.at) < self.window {
                    let window = self.watching.get_or_insert_with(|| {
                        let evidence = evidence(call.rpc);
                        let start = previous.unwrap_or(sample).status & evidence;
                        Window {
                            call,
                            evidence,
                            start,
                            latest: start,
                            reported: start,
                            reported_at: None,
                            changed: false,
                        }
                    });

                    let current = sample.status & window.evidence;
                    window.latest = current;
                    window.changed |= current != window.start;
                    let flipped = current ^ window.reported;
                    let quiet = window
                        .reported_at
                        .is_none_or(|at| now.saturating_duration_since(at) >= QUIET_GAP);
                    // A line is going out anyway, so it carries whatever the gap
                    // was holding back.
                    if flipped != 0 && (quiet || (gained | lost) != 0) {
                        gained |= flipped & current;
                        lost |= flipped & window.reported;
                        window.reported = current;
                        window.reported_at = Some(now);
                    }
                } else {
                    // The window has closed. A call first seen after its window
                    // was never observed, so it gets no verdict, only a finish.
                    if let Some(window) = self.watching.take() {
                        window.flush(&mut gained, &mut lost);
                        if !window.changed {
                            report.lines.push(verdict(&window, None));
                        }
                    }
                    report.finished = Some(call);
                }
            }
            None => self.watching = None,
        }

        // A flag a replaced window flushed and this sample's own window set
        // straight back is where the log already left it.
        let both = gained & lost;
        change.gained = names_of(gained & !both);
        change.lost = names_of(lost & !both);
        report.lines.extend(change.lines(&sample));
        report
    }
}

/// The movement flags that answer a call to `rpc`: the ones its window writes
/// lines for and judges it by.
///
/// An awake Vector's idle animations flip `animating`, `moving` and the two
/// `_in_pos` flags constantly, and judging every call by all six would hide a
/// direct-motor call he ignored while animating. The table cannot tell the
/// call's effect apart from his own motion on the same flags: an idle head
/// turn inside a `MoveHead` window reads as the call working.
fn evidence(rpc: &str) -> u32 {
    match rpc {
        // `pathing` and `wheels_moving`, but not `moving`, which his head and
        // lift set too.
        "DriveWheels" | "DriveStraight" | "TurnInPlace" | "GoToPose" | "DriveOffCharger"
        | "DriveOnCharger" => 0x80 | 0x8000,
        // `head_in_pos`
        "MoveHead" | "SetHeadAngle" => 0x200,
        // `lift_in_pos`
        "MoveLift" | "SetLiftHeight" => 0x100,
        // `animating`, `wheels_moving`, `head_in_pos` and `lift_in_pos`
        "PlayAnimation" | "LookAroundInPlace" => 0x40 | 0x8000 | 0x200 | 0x100,
        // `moving` and `wheels_moving`
        "StopAllMotors" => 0x1 | 0x8000,
        _ => MOVEMENT,
    }
}

/// The one line for a window in which none of its call's flags changed.
///
/// A robot that was already driving when the call arrived keeps
/// `wheels_moving` set throughout, so "no movement" would be false; the line
/// says what stayed set instead, and the reader can see that the call changed
/// no flag. `newer` is the call that replaced the window before it ran out.
fn verdict(window: &Window, newer: Option<&MotionCall>) -> String {
    let MotionCall { rpc, args, at } = &window.call;
    let active = window.start & ACTIVE;
    let mut line = if active == 0 {
        format!("no movement after {rpc}({args})")
    } else {
        format!("no change after {rpc}({args})")
    };
    if let Some(newer) = newer {
        let open = newer.at.saturating_duration_since(*at);
        line.push_str(&format!(
            " in the {:.1} s before {}",
            open.as_secs_f64(),
            newer.rpc
        ));
    }
    if active != 0 {
        line.push_str(&format!(": [{}] throughout", names_of(active).join(" ")));
    }
    line
}

/// The names of the set bits of `bits`, in bit order.
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

    const WINDOW: Duration = Duration::from_millis(3_000);

    const RESTING: u32 = 0x100 | 0x200;
    const DRIVING: u32 = 0x1 | 0x8000 | 0x100 | 0x200;

    /// Instants measured from one base, so the arithmetic is exact rather than
    /// clock-dependent.
    struct Clock(Instant);

    impl Clock {
        fn new() -> Self {
            Self(Instant::now())
        }

        fn at(&self, millis: u64) -> Instant {
            self.0 + Duration::from_millis(millis)
        }

        fn call(&self, rpc: &'static str, args: &str, millis: u64) -> MotionCall {
            MotionCall {
                rpc,
                args: args.to_owned(),
                at: self.at(millis),
            }
        }
    }

    fn sample(status: u32) -> RobotStateSample {
        RobotStateSample {
            status,
            x_mm: 120.43,
            y_mm: -33.06,
            angle_rad: 0.7812,
            origin_id: 3,
            localized_to_object_id: -1,
        }
    }

    #[test]
    fn the_bitfield_names_every_flag_the_robot_can_set() {
        assert_eq!(
            sample(DRIVING).status_names(),
            ["moving", "lift_in_pos", "head_in_pos", "wheels_moving"]
        );
        assert!(sample(0).status_names().is_empty());
    }

    #[test]
    fn urgent_changes_are_written_at_once_with_no_motion_call() {
        let clock = Clock::new();
        let mut tracker = StateTracker::new(WINDOW);
        assert_eq!(
            tracker.note(sample(RESTING), None, clock.at(0)),
            StateReport::default(),
            "the first sample has nothing to be compared with",
        );

        let lifted = tracker.note(sample(RESTING | 0x8 | 0x10000), None, clock.at(10));
        assert_eq!(
            lifted.lines,
            ["state +[picked_up being_held] pose=(120.4, -33.1) heading=0.781rad origin=3"]
        );

        // He throws his frame away with no flag changing, immediately after
        // the last line.
        let moved_frame = RobotStateSample {
            origin_id: 4,
            ..sample(RESTING | 0x8 | 0x10000)
        };
        assert_eq!(
            tracker.note(moved_frame, None, clock.at(11)).lines,
            ["state delocalized: origin 3 -> 4"]
        );

        // -1 is dead reckoning; zero is an object like any other.
        let on_charger = RobotStateSample {
            localized_to_object_id: 0,
            ..moved_frame
        };
        assert_eq!(
            tracker.note(on_charger, None, clock.at(12)).lines,
            ["state localized to object 0"]
        );
        let lost = tracker.note(moved_frame, None, clock.at(13));
        assert_eq!(
            lost.lines,
            ["state on odometry alone: no longer localized to object 0"]
        );
        let also_unknown = RobotStateSample {
            localized_to_object_id: -2,
            ..moved_frame
        };
        assert!(
            tracker
                .note(also_unknown, None, clock.at(13))
                .lines
                .is_empty(),
            "every negative id means dead reckoning"
        );

        let cliff = tracker.note(
            RobotStateSample {
                status: RESTING | 0x4000 | 0x20,
                ..moved_frame
            },
            None,
            clock.at(14),
        );
        assert_eq!(
            cliff.lines,
            [
                "state +[falling cliff_detected] -[picked_up being_held] pose=(120.4, -33.1) heading=0.781rad origin=4"
            ]
        );
        assert_eq!(cliff.finished, None);
    }

    #[test]
    fn idle_flicker_with_no_motion_call_writes_nothing() {
        let clock = Clock::new();
        let mut tracker = StateTracker::new(WINDOW);
        let idle = [
            RESTING,
            RESTING | 0x40 | 0x1,
            0x40 | 0x1 | 0x100,
            0x40 | 0x1,
            RESTING | 0x40,
            RESTING | 0x20000,
            RESTING | 0x1000 | 0x2000,
            RESTING,
        ];
        for (tick, status) in (0..).zip(idle.iter().cycle().take(200)) {
            let report = tracker.note(sample(*status), None, clock.at(tick * 30));
            assert_eq!(
                report,
                StateReport::default(),
                "tick {tick} wrote something"
            );
        }
    }

    #[test]
    fn a_motion_call_followed_by_movement_writes_the_movement() {
        let clock = Clock::new();
        let mut tracker = StateTracker::new(WINDOW);
        let call = clock.call("DriveWheels", "lw=50 rw=50", 100);
        tracker.note(sample(RESTING), None, clock.at(90));

        // The first sample after the call has not reacted yet.
        let report = tracker.note(sample(RESTING), Some(call.clone()), clock.at(110));
        assert_eq!(report, StateReport::default());

        let started = tracker.note(sample(DRIVING), Some(call.clone()), clock.at(160));
        assert_eq!(
            started.lines,
            ["state +[wheels_moving] pose=(120.4, -33.1) heading=0.781rad origin=3"]
        );

        // The wheels stop and start again inside the quiet gap: held back, and
        // gone again before the gap ends, so never written.
        let flicker = sample(DRIVING & !0x8000);
        assert!(
            tracker
                .note(flicker, Some(call.clone()), clock.at(200))
                .lines
                .is_empty()
        );
        assert!(
            tracker
                .note(sample(DRIVING), Some(call.clone()), clock.at(260))
                .lines
                .is_empty()
        );

        // A change still standing when the gap has passed is written, measured
        // against what the log last said.
        assert!(
            tracker
                .note(flicker, Some(call.clone()), clock.at(600))
                .lines
                .is_empty()
        );
        let held = tracker.note(flicker, Some(call.clone()), clock.at(700));
        assert_eq!(
            held.lines,
            ["state -[wheels_moving] pose=(120.4, -33.1) heading=0.781rad origin=3"]
        );

        // The window closes having seen movement: finished, with no verdict.
        let closed = tracker.note(flicker, Some(call.clone()), clock.at(3_100));
        assert_eq!(closed.lines, Vec::<String>::new());
        assert_eq!(closed.finished, Some(call));

        // Outside a window the same flags are silent again.
        assert!(
            tracker
                .note(sample(RESTING), None, clock.at(3_200))
                .lines
                .is_empty()
        );
    }

    #[test]
    fn a_motion_call_followed_by_nothing_writes_one_line_once_the_window_passes() {
        let clock = Clock::new();
        let mut tracker = StateTracker::new(WINDOW);
        let call = clock.call("DriveWheels", "lw=50 rw=50", 0);
        tracker.note(sample(RESTING), None, clock.at(0));
        for millis in (30..3_000).step_by(30) {
            let report = tracker.note(sample(RESTING), Some(call.clone()), clock.at(millis));
            assert_eq!(report, StateReport::default(), "{millis}ms wrote something");
        }

        let closed = tracker.note(sample(RESTING), Some(call.clone()), clock.at(3_010));
        assert_eq!(closed.lines, ["no movement after DriveWheels(lw=50 rw=50)"]);
        assert_eq!(closed.finished, Some(call.clone()));

        // Were the finish to be lost, the stale call still earns no second line.
        let again = tracker.note(sample(RESTING), Some(call.clone()), clock.at(3_040));
        assert!(again.lines.is_empty());
        assert_eq!(again.finished, Some(call));
    }

    #[test]
    fn a_robot_already_moving_is_not_reported_as_still() {
        let clock = Clock::new();
        let mut tracker = StateTracker::new(WINDOW);
        tracker.note(sample(DRIVING), None, clock.at(0));
        let turn = clock.call("DriveWheels", "lw=100 rw=190", 10);
        for millis in (30..3_000).step_by(30) {
            assert!(
                tracker
                    .note(sample(DRIVING), Some(turn.clone()), clock.at(millis))
                    .lines
                    .is_empty()
            );
        }
        let closed = tracker.note(sample(DRIVING), Some(turn), clock.at(3_020));
        assert_eq!(
            closed.lines,
            ["no change after DriveWheels(lw=100 rw=190): [wheels_moving] throughout"]
        );
    }

    #[test]
    fn a_newer_call_is_not_finished_by_an_older_window() {
        let clock = Clock::new();
        let mut tracker = StateTracker::new(Duration::from_millis(1_000));
        let older = clock.call("MoveHead", "speed=2", 0);
        let newer = clock.call("MoveLift", "speed=2", 500);
        tracker.note(sample(RESTING), None, clock.at(0));
        assert!(
            tracker
                .note(sample(RESTING), Some(older), clock.at(30))
                .lines
                .is_empty()
        );

        // The newer call replaces the older one before the older window ends,
        // which judges the older one there and then.
        let replaced = tracker.note(sample(RESTING), Some(newer.clone()), clock.at(510));
        assert_eq!(
            replaced.lines,
            ["no movement after MoveHead(speed=2) in the 0.5 s before MoveLift"]
        );
        assert_eq!(replaced.finished, None);

        // Past the older window's end, inside the newer one's: nothing more
        // for either, and nothing finished.
        let report = tracker.note(sample(RESTING), Some(newer.clone()), clock.at(1_100));
        assert_eq!(report, StateReport::default());

        let closed = tracker.note(sample(RESTING), Some(newer.clone()), clock.at(1_510));
        assert_eq!(closed.lines, ["no movement after MoveLift(speed=2)"]);
        assert_eq!(closed.finished, Some(newer));
    }

    #[test]
    fn an_urgent_line_carries_the_movement_the_gap_held_back() {
        let clock = Clock::new();
        let mut tracker = StateTracker::new(WINDOW);
        let call = clock.call("DriveWheels", "lw=50 rw=50", 0);
        tracker.note(sample(RESTING), None, clock.at(0));
        tracker.note(sample(DRIVING), Some(call.clone()), clock.at(30));

        // He stops and is lifted in the same sample, inside the gap.
        let lifted = tracker.note(sample(RESTING | 0x8), Some(call), clock.at(60));
        assert_eq!(
            lifted.lines,
            ["state +[picked_up] -[wheels_moving] pose=(120.4, -33.1) heading=0.781rad origin=3"]
        );
    }

    #[test]
    fn a_call_seen_only_after_its_window_is_finished_without_a_verdict() {
        let clock = Clock::new();
        let mut tracker = StateTracker::new(WINDOW);
        tracker.note(sample(RESTING), None, clock.at(0));
        let late = clock.call("DriveWheels", "lw=50 rw=50", 10);
        let report = tracker.note(sample(RESTING), Some(late.clone()), clock.at(5_000));
        assert!(report.lines.is_empty());
        assert_eq!(report.finished, Some(late));
    }

    #[test]
    fn his_own_animation_does_not_answer_a_drive() {
        let clock = Clock::new();
        let mut tracker = StateTracker::new(WINDOW);
        let call = clock.call("DriveWheels", "lw=140 rw=140", 0);
        tracker.note(sample(RESTING), None, clock.at(0));

        // Only `animating` and `head_in_pos` flip, as an idle animation does.
        let idle = [RESTING | 0x40, 0x40 | 0x100, RESTING];
        for (tick, status) in (1..100).zip(idle.iter().cycle()) {
            let report = tracker.note(sample(*status), Some(call.clone()), clock.at(tick * 30));
            assert_eq!(
                report,
                StateReport::default(),
                "tick {tick} wrote something"
            );
        }

        let closed = tracker.note(sample(RESTING), Some(call.clone()), clock.at(3_000));
        assert_eq!(
            closed.lines,
            ["no movement after DriveWheels(lw=140 rw=140)"]
        );
        assert_eq!(closed.finished, Some(call));
    }

    #[test]
    fn a_head_call_is_answered_by_the_head_alone() {
        let clock = Clock::new();
        let mut tracker = StateTracker::new(WINDOW);
        let call = clock.call("MoveHead", "speed=2", 0);
        tracker.note(sample(RESTING), None, clock.at(0));

        // His wheels turning in an idle animation are not the head's answer.
        let idle = tracker.note(
            sample(RESTING | 0x1 | 0x40 | 0x8000),
            Some(call.clone()),
            clock.at(30),
        );
        assert_eq!(idle, StateReport::default());

        let raised = tracker.note(
            sample(0x1 | 0x40 | 0x100),
            Some(call.clone()),
            clock.at(600),
        );
        assert_eq!(
            raised.lines,
            ["state -[head_in_pos] pose=(120.4, -33.1) heading=0.781rad origin=3"]
        );
        let settled = tracker.note(sample(RESTING), Some(call.clone()), clock.at(1_200));
        assert_eq!(
            settled.lines,
            ["state +[head_in_pos] pose=(120.4, -33.1) heading=0.781rad origin=3"]
        );

        // The window saw its answer, so it closes with no verdict.
        let closed = tracker.note(sample(RESTING), Some(call.clone()), clock.at(3_000));
        assert_eq!(closed.lines, Vec::<String>::new());
        assert_eq!(closed.finished, Some(call));
    }

    #[test]
    fn a_drive_replaced_by_a_stop_is_judged_when_the_stop_goes_out() {
        let clock = Clock::new();
        let mut tracker = StateTracker::new(WINDOW);
        let drive = clock.call("DriveWheels", "lw=140 rw=140", 0);
        tracker.note(sample(RESTING), None, clock.at(0));
        for millis in (30..1_000).step_by(30) {
            let report = tracker.note(sample(RESTING), Some(drive.clone()), clock.at(millis));
            assert_eq!(report, StateReport::default(), "{millis}ms wrote something");
        }

        // The dashboard sends its stop twice when the key comes up.
        let stop = clock.call("DriveWheels", "lw=0 rw=0", 1_000);
        let again = clock.call("DriveWheels", "lw=0 rw=0", 1_002);
        let judged = tracker.note(sample(RESTING), Some(stop), clock.at(1_001));
        assert_eq!(
            judged.lines,
            ["no movement after DriveWheels(lw=140 rw=140) in the 1.0 s before DriveWheels"]
        );
        assert_eq!(judged.finished, None);
        assert_eq!(
            tracker.note(sample(RESTING), Some(again.clone()), clock.at(1_010)),
            StateReport::default(),
            "the first stop was replaced too soon to be judged"
        );

        // The second stop's own window runs its course.
        let closed = tracker.note(sample(RESTING), Some(again.clone()), clock.at(4_002));
        assert_eq!(closed.lines, ["no movement after DriveWheels(lw=0 rw=0)"]);
        assert_eq!(closed.finished, Some(again));
    }

    #[test]
    fn a_drive_replaced_sooner_than_the_minimum_is_not_judged() {
        let clock = Clock::new();
        let mut tracker = StateTracker::new(WINDOW);
        let drive = clock.call("DriveWheels", "lw=140 rw=140", 0);
        tracker.note(sample(RESTING), None, clock.at(0));
        for millis in (30..200).step_by(30) {
            tracker.note(sample(RESTING), Some(drive.clone()), clock.at(millis));
        }

        let stop = clock.call("DriveWheels", "lw=0 rw=0", 200);
        let replaced = tracker.note(sample(RESTING), Some(stop), clock.at(210));
        assert_eq!(replaced, StateReport::default());
    }

    #[test]
    fn a_turn_while_driving_is_judged_on_what_stayed_set() {
        let clock = Clock::new();
        let mut tracker = StateTracker::new(WINDOW);
        let forward = clock.call("DriveWheels", "lw=140 rw=140", 0);
        tracker.note(sample(DRIVING), None, clock.at(0));
        for millis in (30..1_200).step_by(30) {
            let report = tracker.note(sample(DRIVING), Some(forward.clone()), clock.at(millis));
            assert_eq!(report, StateReport::default(), "{millis}ms wrote something");
        }

        let turn = clock.call("DriveWheels", "lw=100 rw=190", 1_200);
        let judged = tracker.note(sample(DRIVING), Some(turn), clock.at(1_210));
        assert_eq!(
            judged.lines,
            [
                "no change after DriveWheels(lw=140 rw=140) in the 1.2 s before DriveWheels: [wheels_moving] throughout"
            ]
        );
    }

    #[test]
    fn a_window_that_ran_out_before_it_was_replaced_is_judged_as_closed() {
        let clock = Clock::new();
        let mut tracker = StateTracker::new(WINDOW);
        let drive = clock.call("DriveWheels", "lw=140 rw=140", 0);
        tracker.note(sample(RESTING), None, clock.at(0));
        tracker.note(sample(RESTING), Some(drive), clock.at(30));

        // No sample arrived between the end of the window and the next call.
        let stop = clock.call("DriveWheels", "lw=0 rw=0", 3_500);
        let judged = tracker.note(sample(RESTING), Some(stop), clock.at(3_510));
        assert_eq!(
            judged.lines,
            ["no movement after DriveWheels(lw=140 rw=140)"]
        );
        assert_eq!(judged.finished, None);
    }

    #[test]
    fn a_change_the_gap_held_back_is_written_when_the_window_ends() {
        let clock = Clock::new();

        // He starts late and stops again inside the gap, and the window closes
        // before the gap ends.
        let mut tracker = StateTracker::new(WINDOW);
        let call = clock.call("DriveWheels", "lw=50 rw=50", 0);
        tracker.note(sample(RESTING), None, clock.at(0));
        let started = tracker.note(sample(DRIVING), Some(call.clone()), clock.at(2_700));
        assert_eq!(
            started.lines,
            ["state +[wheels_moving] pose=(120.4, -33.1) heading=0.781rad origin=3"]
        );
        assert!(
            tracker
                .note(sample(RESTING), Some(call.clone()), clock.at(2_900))
                .lines
                .is_empty()
        );
        let closed = tracker.note(sample(RESTING), Some(call.clone()), clock.at(3_010));
        assert_eq!(
            closed.lines,
            ["state -[wheels_moving] pose=(120.4, -33.1) heading=0.781rad origin=3"]
        );
        assert_eq!(closed.finished, Some(call));

        // The same, with a newer call ending the window instead.
        let held_back = || {
            let mut tracker = StateTracker::new(WINDOW);
            let drive = clock.call("DriveWheels", "lw=50 rw=50", 0);
            tracker.note(sample(RESTING), None, clock.at(0));
            tracker.note(sample(DRIVING), Some(drive.clone()), clock.at(100));
            let stopped = tracker.note(sample(RESTING), Some(drive), clock.at(300));
            assert!(stopped.lines.is_empty());
            tracker
        };
        let turn = clock.call("DriveWheels", "lw=100 rw=190", 400);
        let replaced = held_back().note(sample(RESTING), Some(turn.clone()), clock.at(410));
        assert_eq!(
            replaced.lines,
            ["state -[wheels_moving] pose=(120.4, -33.1) heading=0.781rad origin=3"]
        );

        // Moving again by the time the newer call is seen: the log already
        // says he is moving, so nothing is written until he stops.
        let mut tracker = held_back();
        let resumed = tracker.note(sample(DRIVING), Some(turn.clone()), clock.at(410));
        assert_eq!(resumed, StateReport::default());
        let stopped = tracker.note(sample(RESTING), Some(turn), clock.at(1_000));
        assert_eq!(
            stopped.lines,
            ["state -[wheels_moving] pose=(120.4, -33.1) heading=0.781rad origin=3"]
        );
    }
}
