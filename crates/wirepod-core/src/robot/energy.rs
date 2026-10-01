//! An estimate of how much charge each robot has left, kept per serial and
//! saved to `energy.json`. No Go counterpart.
//!
//! His power controller measures the battery only while he is on the charger,
//! so off it `battery_volts` repeats the last on-charger reading and says
//! nothing. What stays live off the charger is the charger flags and the LOW
//! level, which rises about four minutes before he shuts down. So the estimate
//! is time-based: 100% at a full charge and 0% at LOW, draining at a learned
//! runtime and refilling at a learned charge time, and re-anchored at every
//! transition and at the one live reading off a trip, the first poll after he
//! docks.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt;
use std::io;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use crate::gojson::go_marshal;
use crate::logger::COMP_SDK;
use crate::paths::DataDir;
use crate::persist::WriteGate;

/// The runtime before any trip has been learned from: one measured trip on
/// 2026-09-28, deliberately cautious.
pub const DEFAULT_RUNTIME_SECS: u64 = 20 * 60;
/// The charge time before any stay has been learned from: a cautious guess.
pub const DEFAULT_CHARGE_SECS: u64 = 60 * 60;
/// His power controller's low-battery line (`syscon/src/analog.cpp:342`).
pub const LOW_LINE_VOLTS: f32 = 3.62;
/// The least voltage at which a FULL on the charger is believed. Right after a
/// deep drain his firmware reports FULL on an empty battery; a real full charge
/// reads 4.00 to 4.06 V.
pub const FULL_FLOOR_VOLTS: f32 = 3.9;

const ENERGY_FILE_MODE: u32 = 0o644;
/// The voltages taken as a real reading rather than a missing one.
const LIVE_VOLTS: RangeInclusive<f32> = 3.3..=4.3;
/// Where a robot first seen with no usable voltage starts.
const SEED_ENERGY: f64 = 50.0;
const LEVEL_LOW: i32 = 1;
const LEVEL_FULL: i32 = 3;
/// The shortest trip or stay anything is learned from.
const MIN_LESSON_SECS: f64 = 5.0 * 60.0;
/// The smallest drop between leaving and docking that teaches a runtime.
const MIN_DOCK_DROP: f64 = 20.0;
/// LOW teaches a runtime only from a trip that started at least this full.
const MIN_LOW_START: f64 = 30.0;
/// FULL teaches a charge time only from a stay that started at most this full.
const MAX_FULL_START: f64 = 70.0;
const MIN_LEARNED_SECS: u64 = 5 * 60;
const MAX_LEARNED_SECS: u64 = 6 * 60 * 60;
/// How far LOW and FULL move what they teach toward what they observed.
const FIRM_LESSON: f64 = 0.5;
/// How far a docking reading moves the runtime: less, as it is noisier.
const DOCK_LESSON: f64 = 0.25;

/// must match getBatteryPercentage in webroot/js/battery.js so the trigger
/// percent agrees with what the web UI shows
pub fn battery_percent(volts: f32) -> i32 {
    // Go's two separate bounds checks, which clippy will not let stand apart.
    let percentage = curve(volts).round().clamp(0.0, 100.0);
    percentage as i32
}

/// [`battery_percent`] before it is rounded.
fn curve(volts: f32) -> f64 {
    const MAX_VOLTAGE: f64 = 4.1;
    const MID_VOLTAGE: f64 = 3.85;
    const MIN_VOLTAGE: f64 = 3.5;
    let v = f64::from(volts);
    if v >= MAX_VOLTAGE {
        100.0
    } else if v >= MID_VOLTAGE {
        let scaled = (v - MID_VOLTAGE) / (MAX_VOLTAGE - MID_VOLTAGE);
        80.0 + 20.0 * (1.0 + scaled * 9.0).log10()
    } else if v >= MIN_VOLTAGE {
        let scaled = (v - MIN_VOLTAGE) / (MID_VOLTAGE - MIN_VOLTAGE);
        80.0 * (1.0 + scaled * 9.0).log10()
    } else if v == 0.0 {
        // no voltage reported (bot booted off charger); the watchdog's
        // volts > 0 gate keeps this from ever triggering a go-home
        70.0
    } else {
        0.0
    }
}

/// The web UI's curve rescaled so the low-battery line is 0 and a full battery
/// 100.
pub fn energy_from_volts(volts: f32) -> f64 {
    // Also keeps the curve's 70 for a missing reading out of the scale.
    if volts <= LOW_LINE_VOLTS {
        return 0.0;
    }
    let floor = curve(LOW_LINE_VOLTS);
    ((curve(volts) - floor) / (100.0 - floor) * 100.0).clamp(0.0, 100.0)
}

/// One battery poll, as the estimate reads it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BatteryObservation {
    /// Unix milliseconds.
    pub now_ms: u64,
    /// `is_charging || is_on_charger_platform`, the watchdog's own rule.
    pub home: bool,
    /// `BatteryState.battery_level`: 0 unknown, 1 low, 2 nominal, 3 full.
    pub level: i32,
    pub volts: f32,
}

/// What one observation did to a robot's model, for the log.
#[derive(Clone, Debug, PartialEq)]
pub enum EnergyEvent {
    /// The robot had no model, so one was started from his voltage.
    FirstSeen {
        on_charger: bool,
        energy: f64,
        volts: f32,
    },
    /// The first poll after a restart found him where `energy.json` left him.
    Resumed { on_charger: bool, energy: f64 },
    /// The first poll after a restart found him on the other side of the
    /// charger. `on_charger` is where he is now.
    ChangedWhileDown { on_charger: bool, energy: f64 },
    LeftCharger {
        energy: f64,
        minutes_left: f64,
        runtime_minutes: f64,
    },
    /// `measured` is `None` when the docking voltage was out of range, and
    /// `runtime` is the old and new runtime in minutes when the trip taught
    /// one.
    ReachedCharger {
        off_minutes: f64,
        estimate: f64,
        measured: Option<f64>,
        volts: f32,
        runtime: Option<(f64, f64)>,
    },
    /// `runtime` is the old and new runtime when this trip taught one, and
    /// `runtime_minutes` the runtime in force afterwards either way.
    Low {
        off_minutes: f64,
        runtime: Option<(f64, f64)>,
        runtime_minutes: f64,
    },
    /// `charge` is the old and new charge time when this stay taught one, and
    /// `charge_minutes` the charge time in force afterwards either way.
    Full {
        on_minutes: f64,
        charge: Option<(f64, f64)>,
        charge_minutes: f64,
    },
    /// A FULL on the charger below [`FULL_FLOOR_VOLTS`], told once a stay.
    FullIgnored { volts: f32 },
}

impl fmt::Display for EnergyEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FirstSeen {
                on_charger,
                energy,
                volts,
            } => write!(
                f,
                "energy: first sight of him {} the charger; starting from a guess of ~{}% from his reported {volts:.2}V",
                on_off(*on_charger),
                whole(*energy),
            ),
            Self::Resumed { on_charger, energy } => write!(
                f,
                "energy: resumed from energy.json at ~{}%, {} the charger",
                whole(*energy),
                on_off(*on_charger),
            ),
            Self::ChangedWhileDown { on_charger, energy } => write!(
                f,
                "energy: he {} the charger while the server was down; assuming it happened now, at ~{}%",
                if *on_charger { "reached" } else { "left" },
                whole(*energy),
            ),
            Self::LeftCharger {
                energy,
                minutes_left,
                runtime_minutes,
            } => write!(
                f,
                "energy: left the charger at ~{}%, about {} min before his low-battery flag (runtime {} min)",
                whole(*energy),
                whole(*minutes_left),
                whole(*runtime_minutes),
            ),
            Self::ReachedCharger {
                off_minutes,
                estimate,
                measured,
                volts,
                runtime,
            } => {
                write!(
                    f,
                    "energy: back on the charger after {} min off; estimate ~{}%",
                    whole(*off_minutes),
                    whole(*estimate),
                )?;
                match measured {
                    Some(measured) => write!(
                        f,
                        ", his docking reading {volts:.2}V gives ~{}%",
                        whole(*measured)
                    )?,
                    None => write!(f, ", no usable docking reading ({volts:.2}V)")?,
                }
                if let Some((old, new)) = runtime {
                    write!(f, "; runtime {} -> {} min", whole(*old), whole(*new))?;
                }
                Ok(())
            }
            Self::Low {
                off_minutes,
                runtime,
                runtime_minutes,
            } => {
                write!(
                    f,
                    "energy: low-battery flag after {} min off the charger",
                    whole(*off_minutes)
                )?;
                match runtime {
                    Some((old, new)) => {
                        write!(f, "; runtime {} -> {} min", whole(*old), whole(*new))
                    }
                    None => write!(f, "; runtime kept at {} min", whole(*runtime_minutes)),
                }
            }
            Self::Full {
                on_minutes,
                charge,
                charge_minutes,
            } => {
                write!(
                    f,
                    "energy: charged full after {} min on the charger",
                    whole(*on_minutes)
                )?;
                match charge {
                    Some((old, new)) => {
                        write!(f, "; charge time {} -> {} min", whole(*old), whole(*new))
                    }
                    None => write!(f, "; charge time kept at {} min", whole(*charge_minutes)),
                }
            }
            Self::FullIgnored { volts } => write!(
                f,
                "energy: his battery reports full at {volts:.2}V, too low to be a real charge; ignoring it"
            ),
        }
    }
}

fn on_off(on_charger: bool) -> &'static str {
    if on_charger { "on" } else { "off" }
}

/// Rounded half away from zero, which `{:.0}` does not do.
fn whole(value: f64) -> i64 {
    value.round() as i64
}

fn minutes(secs: u64) -> f64 {
    secs as f64 / 60.0
}

/// A robot's estimate at one instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnergySnapshot {
    /// False while the model is what `energy.json` held and no poll has
    /// confirmed it since the restart.
    pub known: bool,
    /// The anchor is a guess: a first sight, or a charger change while the
    /// server was down.
    pub guess: bool,
    pub on_charger: bool,
    pub energy_percent: f64,
    /// `energy_percent * runtime / 100`, in minutes.
    pub minutes_left: f64,
    pub runtime_minutes: f64,
    pub runtime_learned: bool,
    pub charge_minutes: f64,
    pub charge_learned: bool,
    /// The anchor time.
    pub since_unix_secs: u64,
}

/// One robot's model.
#[derive(Clone, Debug)]
struct Model {
    runtime_s: u64,
    runtime_learned: bool,
    charge_s: u64,
    charge_learned: bool,
    on_charger: bool,
    anchor_ms: u64,
    anchor_energy: f64,
    guess: bool,
    /// LOW has been dealt with on this trip.
    low_seen: bool,
    /// FULL has been dealt with on this stay.
    full_seen: bool,
    /// A FULL below the floor has been told on this stay. Not saved, so a
    /// restart mid-stay may tell it once more.
    full_ignored: bool,
    /// Loaded from `energy.json` and not observed since.
    restored: bool,
}

impl Model {
    fn first(obs: &BatteryObservation) -> Self {
        let energy = if LIVE_VOLTS.contains(&obs.volts) {
            energy_from_volts(obs.volts)
        } else {
            SEED_ENERGY
        };
        Self {
            runtime_s: DEFAULT_RUNTIME_SECS,
            runtime_learned: false,
            charge_s: DEFAULT_CHARGE_SECS,
            charge_learned: false,
            on_charger: obs.home,
            anchor_ms: obs.now_ms,
            anchor_energy: energy,
            guess: true,
            low_seen: false,
            full_seen: false,
            full_ignored: false,
            restored: false,
        }
    }

    fn elapsed_s(&self, now_ms: u64) -> f64 {
        now_ms.saturating_sub(self.anchor_ms) as f64 / 1000.0
    }

    fn energy_at(&self, now_ms: u64) -> f64 {
        let elapsed_s = self.elapsed_s(now_ms);
        if self.on_charger {
            (self.anchor_energy + elapsed_s / self.charge_s as f64 * 100.0).min(100.0)
        } else {
            (self.anchor_energy - elapsed_s / self.runtime_s as f64 * 100.0).max(0.0)
        }
    }

    fn minutes_left(&self, energy: f64) -> f64 {
        energy * minutes(self.runtime_s) / 100.0
    }

    fn anchor(&mut self, now_ms: u64, energy: f64) {
        self.anchor_ms = now_ms;
        self.anchor_energy = energy;
    }

    /// Starts a trip or a stay at `energy`.
    fn start(&mut self, on_charger: bool, now_ms: u64, energy: f64) {
        self.on_charger = on_charger;
        self.anchor(now_ms, energy);
        if on_charger {
            self.full_seen = false;
            self.full_ignored = false;
        } else {
            self.low_seen = false;
        }
    }

    /// Whether a trip or stay this long, from this anchor, can teach anything.
    /// A guessed anchor cannot: after a first sight off the charger his voltage
    /// is the frozen on-charger one and the trip began before the server saw
    /// it.
    fn can_learn(&self, elapsed_s: f64) -> bool {
        !self.guess && elapsed_s >= MIN_LESSON_SECS
    }

    fn learn_runtime(&mut self, observed_s: f64, weight: f64) -> (f64, f64) {
        let old = self.runtime_s;
        self.runtime_s = blend(old, observed_s, weight);
        self.runtime_learned = true;
        (minutes(old), minutes(self.runtime_s))
    }

    fn learn_charge(&mut self, observed_s: f64) -> (f64, f64) {
        let old = self.charge_s;
        self.charge_s = blend(old, observed_s, FIRM_LESSON);
        self.charge_learned = true;
        (minutes(old), minutes(self.charge_s))
    }

    /// The first poll after a restart, or a change of charger state.
    fn transition(&mut self, obs: &BatteryObservation, events: &mut Vec<EnergyEvent>) {
        let now = obs.now_ms;
        if std::mem::take(&mut self.restored) {
            let energy = self.energy_at(now);
            if obs.home == self.on_charger {
                events.push(EnergyEvent::Resumed {
                    on_charger: self.on_charger,
                    energy,
                });
            } else {
                self.start(obs.home, now, energy);
                // When it happened is assumed, so nothing is learned from it.
                self.guess = true;
                events.push(EnergyEvent::ChangedWhileDown {
                    on_charger: obs.home,
                    energy,
                });
            }
            return;
        }
        match (self.on_charger, obs.home) {
            (true, false) => {
                let mut energy = self.energy_at(now);
                // The leaving poll's voltage is the last one from the stay, and a
                // low one caps a stay that an ignored FULL or the clock overrated.
                if LIVE_VOLTS.contains(&obs.volts) && obs.volts < FULL_FLOOR_VOLTS {
                    energy = energy.min(energy_from_volts(obs.volts));
                }
                self.start(false, now, energy);
                self.guess = false;
                events.push(EnergyEvent::LeftCharger {
                    energy,
                    minutes_left: self.minutes_left(energy),
                    runtime_minutes: minutes(self.runtime_s),
                });
            }
            (false, true) => {
                let elapsed_s = self.elapsed_s(now);
                let estimate = self.energy_at(now);
                let (measured, runtime) = if LIVE_VOLTS.contains(&obs.volts) {
                    let measured = energy_from_volts(obs.volts);
                    let drop = self.anchor_energy - measured;
                    let runtime = (self.can_learn(elapsed_s) && drop >= MIN_DOCK_DROP)
                        .then(|| self.learn_runtime(elapsed_s * 100.0 / drop, DOCK_LESSON));
                    (Some(measured), runtime)
                } else {
                    (None, None)
                };
                self.start(true, now, measured.unwrap_or(estimate));
                self.guess = false;
                events.push(EnergyEvent::ReachedCharger {
                    off_minutes: elapsed_s / 60.0,
                    estimate,
                    measured,
                    volts: obs.volts,
                    runtime,
                });
            }
            _ => {}
        }
    }

    /// LOW off the charger and FULL on it, each once per trip or stay.
    fn level(&mut self, obs: &BatteryObservation, events: &mut Vec<EnergyEvent>) {
        let now = obs.now_ms;
        if !self.on_charger && obs.level == LEVEL_LOW && !self.low_seen {
            let elapsed_s = self.elapsed_s(now);
            let start = self.anchor_energy;
            let runtime = (self.can_learn(elapsed_s) && start >= MIN_LOW_START)
                .then(|| self.learn_runtime(elapsed_s * 100.0 / start, FIRM_LESSON));
            self.anchor(now, 0.0);
            self.low_seen = true;
            self.guess = false;
            events.push(EnergyEvent::Low {
                off_minutes: elapsed_s / 60.0,
                runtime,
                runtime_minutes: minutes(self.runtime_s),
            });
        }
        if self.on_charger && obs.level == LEVEL_FULL && !self.full_seen {
            if obs.volts < FULL_FLOOR_VOLTS {
                // Left unseen, so a real FULL later in the stay still counts.
                if !std::mem::replace(&mut self.full_ignored, true) {
                    events.push(EnergyEvent::FullIgnored { volts: obs.volts });
                }
                return;
            }
            let elapsed_s = self.elapsed_s(now);
            let start = self.anchor_energy;
            let charge = (self.can_learn(elapsed_s) && start <= MAX_FULL_START)
                .then(|| self.learn_charge(elapsed_s * 100.0 / (100.0 - start)));
            self.anchor(now, 100.0);
            self.full_seen = true;
            self.guess = false;
            events.push(EnergyEvent::Full {
                on_minutes: elapsed_s / 60.0,
                charge,
                charge_minutes: minutes(self.charge_s),
            });
        }
    }

    fn snapshot(&self, now_ms: u64) -> EnergySnapshot {
        let energy = self.energy_at(now_ms);
        EnergySnapshot {
            known: !self.restored,
            guess: self.guess,
            on_charger: self.on_charger,
            energy_percent: energy,
            minutes_left: self.minutes_left(energy),
            runtime_minutes: minutes(self.runtime_s),
            runtime_learned: self.runtime_learned,
            charge_minutes: minutes(self.charge_s),
            charge_learned: self.charge_learned,
            since_unix_secs: self.anchor_ms / 1000,
        }
    }

    fn saved(&self) -> Saved {
        Saved {
            runtime_secs: self.runtime_s,
            runtime_learned: self.runtime_learned,
            charge_secs: self.charge_s,
            charge_learned: self.charge_learned,
            on_charger: self.on_charger,
            anchor_ms: self.anchor_ms,
            anchor_energy_tenths: (self.anchor_energy * 10.0).round() as u64,
            guess: self.guess,
            low_seen: self.low_seen,
            full_seen: self.full_seen,
        }
    }

    fn restore(saved: Saved) -> Self {
        Self {
            runtime_s: saved.runtime_secs.clamp(MIN_LEARNED_SECS, MAX_LEARNED_SECS),
            runtime_learned: saved.runtime_learned,
            charge_s: saved.charge_secs.clamp(MIN_LEARNED_SECS, MAX_LEARNED_SECS),
            charge_learned: saved.charge_learned,
            on_charger: saved.on_charger,
            anchor_ms: saved.anchor_ms,
            anchor_energy: (saved.anchor_energy_tenths as f64 / 10.0).clamp(0.0, 100.0),
            guess: saved.guess,
            low_seen: saved.low_seen,
            full_seen: saved.full_seen,
            full_ignored: false,
            restored: true,
        }
    }
}

/// `new = old + (observed - old) * weight`, held to 5 min ..= 6 h.
fn blend(old: u64, observed_s: f64, weight: f64) -> u64 {
    let old = old as f64;
    (old + (observed_s - old) * weight)
        .clamp(MIN_LEARNED_SECS as f64, MAX_LEARNED_SECS as f64)
        .round() as u64
}

/// One robot's model as `energy.json` holds it, in integers so the file needs
/// no float formatting.
#[derive(Serialize, Deserialize)]
struct Saved {
    runtime_secs: u64,
    runtime_learned: bool,
    charge_secs: u64,
    charge_learned: bool,
    on_charger: bool,
    anchor_ms: u64,
    anchor_energy_tenths: u64,
    guess: bool,
    low_seen: bool,
    full_seen: bool,
}

/// Every robot's model, keyed by serial.
pub struct EnergyStore {
    robots: Mutex<BTreeMap<String, Model>>,
    gate: WriteGate,
}

impl EnergyStore {
    /// An empty store writing to `path`. Reads nothing.
    pub fn new(path: impl Into<String>) -> Self {
        Self::with_robots(path, BTreeMap::new())
    }

    fn with_robots(path: impl Into<String>, robots: BTreeMap<String, Model>) -> Self {
        Self {
            robots: Mutex::new(robots),
            gate: WriteGate::new(path, ENERGY_FILE_MODE),
        }
    }

    /// The store saved at `data.energy_path()`, or an empty one if the file is
    /// absent or unreadable.
    pub async fn load(data: &DataDir) -> Self {
        let path = data.energy_path();
        let target = PathBuf::from(&path);
        let read = tokio::task::spawn_blocking(move || std::fs::read(target))
            .await
            .unwrap_or_else(|err| Err(io::Error::other(err)));
        let robots = match read {
            Ok(bytes) => match serde_json::from_slice::<BTreeMap<String, Saved>>(&bytes) {
                Ok(saved) => saved
                    .into_iter()
                    .map(|(serial, saved)| (serial, Model::restore(saved)))
                    .collect(),
                Err(err) => {
                    tracing::warn!(target: COMP_SDK, "energy: {path} is unreadable, starting empty: {err}");
                    BTreeMap::new()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(err) => {
                tracing::warn!(target: COMP_SDK, "energy: {path} is unreadable, starting empty: {err}");
                BTreeMap::new()
            }
        };
        Self::with_robots(path, robots)
    }

    /// Folds one reading into the robot's model and answers what happened, for
    /// the log.
    pub fn observe(&self, serial: &str, obs: BatteryObservation) -> Vec<EnergyEvent> {
        let mut events = Vec::new();
        let mut robots = self.robots();
        let model = match robots.entry(serial.to_owned()) {
            Entry::Occupied(entry) => {
                let model = entry.into_mut();
                model.transition(&obs, &mut events);
                model
            }
            Entry::Vacant(entry) => {
                let model = entry.insert(Model::first(&obs));
                events.push(EnergyEvent::FirstSeen {
                    on_charger: model.on_charger,
                    energy: model.anchor_energy,
                    volts: obs.volts,
                });
                model
            }
        };
        model.level(&obs, &mut events);
        events
    }

    /// The estimate at `now_ms`, or `None` for a robot never observed.
    pub fn snapshot(&self, serial: &str, now_ms: u64) -> Option<EnergySnapshot> {
        self.robots()
            .get(serial)
            .map(|model| model.snapshot(now_ms))
    }

    /// Writes every robot's model through the store's gate.
    pub async fn save(&self) -> io::Result<()> {
        self.gate
            .write(|| {
                let saved: BTreeMap<String, Saved> = self
                    .robots()
                    .iter()
                    .map(|(serial, model)| (serial.clone(), model.saved()))
                    .collect();
                go_marshal(&saved).expect("an energy model holds only integers and flags")
            })
            .await
    }

    fn robots(&self) -> MutexGuard<'_, BTreeMap<String, Model>> {
        self.robots.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
