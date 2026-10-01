//! The energy estimate: the drain and the refill, what LOW, FULL and a docking
//! reading teach, the first-sight guess, `energy.json` across a restart, and the
//! log text.
//!
//! Every time is an explicit Unix millisecond. The disk tests work in a
//! directory under the system temporary directory, named for the process, and
//! remove it afterwards.

use std::path::PathBuf;
use std::sync::Arc;

use wirepod_core::paths::{AssetDir, DataDir};
use wirepod_core::robot::energy::{
    BatteryObservation, DEFAULT_CHARGE_SECS, DEFAULT_RUNTIME_SECS, EnergyEvent, EnergyStore,
    LOW_LINE_VOLTS, battery_percent, energy_from_volts,
};
use wirepod_core::test_support::FakeConnFactory;
use wirepod_core::{AppState, ConnError, Paths, RobotConnFactory};

const SERIAL: &str = "00303f28";
/// 2026-09-20T00:00:00Z.
const T0: u64 = 1_789_862_400_000;
const NOMINAL: i32 = 2;
const LOW: i32 = 1;
const FULL: i32 = 3;
/// What his controller reports off the charger: the last on-charger reading.
const FROZEN: f32 = 4.05;

/// `T0` plus whole minutes.
fn at(minutes: u64) -> u64 {
    T0 + minutes * 60_000
}

fn on(now_ms: u64, level: i32, volts: f32) -> BatteryObservation {
    BatteryObservation {
        now_ms,
        home: true,
        level,
        volts,
    }
}

fn off(now_ms: u64, level: i32) -> BatteryObservation {
    off_reading(now_ms, level, FROZEN)
}

/// Off the charger with the reading his last on-charger poll left behind.
fn off_reading(now_ms: u64, level: i32, volts: f32) -> BatteryObservation {
    BatteryObservation {
        now_ms,
        home: false,
        level,
        volts,
    }
}

fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-6
}

/// A store that is never saved.
fn store() -> EnergyStore {
    EnergyStore::new("./energy-never-written.json")
}

/// First sight on the charger at `T0`, an hour's charge, and off the charger
/// at `at(60)` with a known 100%.
fn left_full(store: &EnergyStore) {
    store.observe(SERIAL, on(T0, NOMINAL, 4.1));
    assert!(store.observe(SERIAL, on(at(60), NOMINAL, 4.1)).is_empty());
    assert_eq!(
        store.observe(SERIAL, off(at(60), NOMINAL)),
        [EnergyEvent::LeftCharger {
            energy: 100.0,
            minutes_left: 20.0,
            runtime_minutes: 20.0,
        }]
    );
}

/// The curve's own comments and `webroot/js/battery.js`.
#[test]
fn the_curve_agrees_with_the_web_ui() {
    assert_eq!(battery_percent(4.2), 100);
    assert_eq!(battery_percent(4.1), 100);
    assert_eq!(battery_percent(3.85), 80);
    assert_eq!(battery_percent(3.5), 0);
    assert_eq!(battery_percent(3.4), 0);
    assert_eq!(battery_percent(0.0), 70);
    // The two logarithmic arms, rounded as Go rounds them.
    assert_eq!(battery_percent(4.0), 96);
    assert_eq!(battery_percent(3.7), 63);
}

#[test]
fn the_scale_runs_from_the_low_battery_line_to_a_full_battery() {
    assert_eq!(energy_from_volts(LOW_LINE_VOLTS), 0.0);
    assert_eq!(energy_from_volts(3.62), 0.0);
    assert_eq!(energy_from_volts(3.60), 0.0);
    assert_eq!(energy_from_volts(0.0), 0.0);
    // 4.1 as an f32 is a hair under the curve's 4.1, so it lands on the
    // logarithmic arm a millionth short of the top.
    assert!((energy_from_volts(4.1) - 100.0).abs() < 1e-3);
    assert_eq!(energy_from_volts(4.2), 100.0);
    let middle = energy_from_volts(3.85);
    assert!(middle > 60.0 && middle < 62.0, "{middle}");
}

#[test]
fn he_drains_off_the_charger_and_refills_on_it_between_zero_and_a_hundred() {
    let store = store();
    left_full(&store);

    let snapshot = store.snapshot(SERIAL, at(70)).expect("observed");
    assert!(close(snapshot.energy_percent, 50.0));
    assert!(close(snapshot.minutes_left, 10.0));
    assert!(!snapshot.on_charger && !snapshot.guess && snapshot.known);
    assert_eq!(snapshot.since_unix_secs, at(60) / 1000);
    assert_eq!(store.snapshot(SERIAL, at(90)).unwrap().energy_percent, 0.0);
    // A poll earlier than the anchor counts as no time at all.
    assert_eq!(
        store.snapshot(SERIAL, at(30)).unwrap().energy_percent,
        100.0
    );

    // Docked at 25% with no usable reading, so the estimate is the anchor.
    assert_eq!(
        store.observe(SERIAL, on(at(75), NOMINAL, 0.0)),
        [EnergyEvent::ReachedCharger {
            off_minutes: 15.0,
            estimate: 25.0,
            measured: None,
            volts: 0.0,
            runtime: None,
        }]
    );
    assert!(close(
        store.snapshot(SERIAL, at(105)).unwrap().energy_percent,
        75.0
    ));
    assert_eq!(
        store.snapshot(SERIAL, at(200)).unwrap().energy_percent,
        100.0
    );
    assert_eq!(store.snapshot("0000beef", at(200)), None);
}

#[test]
fn low_teaches_the_runtime_once_a_trip_and_not_from_a_trip_that_started_low() {
    let store = store();
    left_full(&store);

    assert_eq!(
        store.observe(SERIAL, off(at(90), LOW)),
        [EnergyEvent::Low {
            off_minutes: 30.0,
            runtime: Some((20.0, 25.0)),
            runtime_minutes: 25.0,
        }]
    );
    assert!(store.observe(SERIAL, off(at(91), LOW)).is_empty());
    let snapshot = store.snapshot(SERIAL, at(91)).unwrap();
    assert_eq!(snapshot.energy_percent, 0.0);
    assert!(snapshot.runtime_learned && close(snapshot.runtime_minutes, 25.0));

    // Fifteen minutes on the charger from nothing is 25%, under the guard.
    store.observe(SERIAL, on(at(95), NOMINAL, 0.0));
    store.observe(SERIAL, off(at(110), NOMINAL));
    let events = store.observe(SERIAL, off(at(120), LOW));
    assert_eq!(
        events,
        [EnergyEvent::Low {
            off_minutes: 10.0,
            runtime: None,
            runtime_minutes: 25.0,
        }]
    );
    assert_eq!(
        events[0].to_string(),
        "energy: low-battery flag after 10 min off the charger; runtime kept at 25 min"
    );
}

#[test]
fn full_teaches_the_charge_time_once_a_stay_and_not_from_a_stay_that_started_high() {
    let store = store();
    left_full(&store);
    store.observe(SERIAL, off(at(90), LOW));

    // Docked empty; fifty minutes later he is full.
    store.observe(SERIAL, on(at(100), NOMINAL, 0.0));
    assert_eq!(
        store.observe(SERIAL, on(at(150), FULL, 4.2)),
        [EnergyEvent::Full {
            on_minutes: 50.0,
            charge: Some((60.0, 55.0)),
            charge_minutes: 55.0,
        }]
    );
    assert!(store.observe(SERIAL, on(at(151), FULL, 4.2)).is_empty());
    let snapshot = store.snapshot(SERIAL, at(151)).unwrap();
    assert_eq!(snapshot.energy_percent, 100.0);
    assert!(snapshot.charge_learned && close(snapshot.charge_minutes, 55.0));

    // Five minutes off from full at the 25-minute runtime is 80% at the dock,
    // over the guard.
    store.observe(SERIAL, off(at(160), NOMINAL));
    store.observe(SERIAL, on(at(165), NOMINAL, 0.0));
    assert_eq!(
        store.observe(SERIAL, on(at(175), FULL, 4.2)),
        [EnergyEvent::Full {
            on_minutes: 10.0,
            charge: None,
            charge_minutes: 55.0,
        }]
    );
}

#[test]
fn full_in_the_docking_poll_teaches_nothing() {
    let store = store();
    left_full(&store);
    let events = store.observe(SERIAL, on(at(100), FULL, 4.05));
    let [
        EnergyEvent::ReachedCharger { .. },
        EnergyEvent::Full { charge, .. },
    ] = &events[..]
    else {
        panic!("{events:?}");
    };
    assert_eq!(*charge, None);
    let snapshot = store.snapshot(SERIAL, at(100)).unwrap();
    assert!(!snapshot.charge_learned && snapshot.energy_percent == 100.0);
}

#[test]
fn a_docking_reading_with_voltage_to_spare_lengthens_the_runtime_after_a_long_trip() {
    let store = store();
    left_full(&store);
    let measured = energy_from_volts(3.85);

    let events = store.observe(SERIAL, on(at(90), NOMINAL, 3.85));
    let [
        EnergyEvent::ReachedCharger {
            off_minutes,
            estimate,
            measured: Some(reading),
            runtime: Some((old, new)),
            ..
        },
    ] = events.as_slice()
    else {
        panic!("{events:?}");
    };
    assert_eq!((*off_minutes, *estimate, *reading), (30.0, 0.0, measured));
    // Thirty minutes spent 100 - measured points, a quarter of the way from
    // the old 20.
    let taught = 30.0 * 100.0 / (100.0 - measured);
    assert_eq!(*old, 20.0);
    assert!((new - (20.0 + (taught - 20.0) / 4.0)).abs() < 0.02, "{new}");
    let snapshot = store.snapshot(SERIAL, at(90)).unwrap();
    assert!(snapshot.on_charger && snapshot.runtime_learned);
    assert_eq!(snapshot.energy_percent, measured);
}

#[test]
fn a_docking_reading_teaches_nothing_after_a_short_trip_or_out_of_range() {
    let short = store();
    left_full(&short);
    let events = short.observe(SERIAL, on(at(64), NOMINAL, 3.85));
    assert!(matches!(
        events[..],
        [EnergyEvent::ReachedCharger {
            measured: Some(_),
            runtime: None,
            ..
        }]
    ));
    assert_eq!(
        short.snapshot(SERIAL, at(64)).unwrap().energy_percent,
        energy_from_volts(3.85)
    );

    let unusable = store();
    left_full(&unusable);
    let events = unusable.observe(SERIAL, on(at(90), NOMINAL, 4.5));
    assert!(matches!(
        events[..],
        [EnergyEvent::ReachedCharger {
            measured: None,
            runtime: None,
            ..
        }]
    ));
    let snapshot = unusable.snapshot(SERIAL, at(90)).unwrap();
    assert!(!snapshot.runtime_learned);
    assert_eq!(snapshot.energy_percent, 0.0);
}

/// LOW moves the runtime half of the way to what it saw, and a docking reading
/// a quarter of the way.
#[test]
fn a_docking_reading_moves_the_runtime_a_quarter_of_the_way_and_low_half_of_it() {
    let store = store();
    left_full(&store);

    // Forty minutes from full to LOW teaches 40 against the old 20.
    assert_eq!(
        store.observe(SERIAL, off(at(100), LOW)),
        [EnergyEvent::Low {
            off_minutes: 40.0,
            runtime: Some((20.0, 30.0)),
            runtime_minutes: 30.0,
        }]
    );

    // Docked empty with no usable reading, charged full, and off at 100%.
    store.observe(SERIAL, on(at(110), NOMINAL, 0.0));
    store.observe(SERIAL, on(at(170), FULL, 4.05));
    store.observe(SERIAL, off(at(170), NOMINAL));

    // Twenty minutes spent 100 - measured points.
    let measured = energy_from_volts(3.85);
    let taught = 20.0 * 100.0 / (100.0 - measured);
    let events = store.observe(SERIAL, on(at(190), NOMINAL, 3.85));
    let [
        EnergyEvent::ReachedCharger {
            runtime: Some((old, new)),
            ..
        },
    ] = events.as_slice()
    else {
        panic!("{events:?}");
    };
    assert_eq!(*old, 30.0);
    assert!((new - (30.0 + (taught - 30.0) / 4.0)).abs() < 0.02, "{new}");
}

/// Right after a deep drain his firmware reports FULL on an empty battery.
#[test]
fn a_full_below_the_floor_is_ignored_and_told_once_a_stay() {
    let store = store();
    left_full(&store);
    store.observe(SERIAL, off(at(90), LOW));

    let events = store.observe(SERIAL, on(at(100), FULL, 3.60));
    assert_eq!(
        events,
        [
            EnergyEvent::ReachedCharger {
                off_minutes: 10.0,
                estimate: 0.0,
                measured: Some(0.0),
                volts: 3.60,
                runtime: None,
            },
            EnergyEvent::FullIgnored { volts: 3.60 },
        ]
    );
    assert_eq!(
        events[1].to_string(),
        "energy: his battery reports full at 3.60V, too low to be a real charge; ignoring it"
    );
    assert!(store.observe(SERIAL, on(at(103), FULL, 3.61)).is_empty());

    // Six minutes of a sixty-minute charge from empty, not 100%.
    let snapshot = store.snapshot(SERIAL, at(106)).unwrap();
    assert!(close(snapshot.energy_percent, 10.0));
    assert!(!snapshot.charge_learned && close(snapshot.charge_minutes, 60.0));

    // A FULL at 4.0V later in the same stay is a real one.
    assert_eq!(
        store.observe(SERIAL, on(at(150), FULL, 4.0)),
        [EnergyEvent::Full {
            on_minutes: 50.0,
            charge: Some((60.0, 55.0)),
            charge_minutes: 55.0,
        }]
    );
    assert_eq!(
        store.snapshot(SERIAL, at(150)).unwrap().energy_percent,
        100.0
    );
}

/// The 16:22 departure: revived on the charger at 3.60V, a false FULL, and
/// off nine minutes later at 3.62V with his low-battery flag a minute after.
#[test]
fn leaving_below_the_floor_after_an_ignored_full_starts_the_trip_near_empty() {
    let store = store();
    left_full(&store);
    store.observe(SERIAL, off(at(90), LOW));
    store.observe(SERIAL, on(at(100), FULL, 3.60));
    assert!(store.observe(SERIAL, on(at(109), FULL, 3.62)).is_empty());
    assert!(close(
        store.snapshot(SERIAL, at(109)).unwrap().energy_percent,
        15.0
    ));

    assert_eq!(
        store.observe(SERIAL, off_reading(at(109), FULL, 3.62)),
        [EnergyEvent::LeftCharger {
            energy: 0.0,
            minutes_left: 0.0,
            runtime_minutes: 25.0,
        }]
    );
    assert_eq!(
        store.observe(SERIAL, off_reading(at(110), LOW, 3.62)),
        [EnergyEvent::Low {
            off_minutes: 1.0,
            runtime: None,
            runtime_minutes: 25.0,
        }]
    );

    // The next stay tells its own ignored FULL.
    let events = store.observe(SERIAL, on(at(120), FULL, 3.65));
    assert_eq!(
        events.last(),
        Some(&EnergyEvent::FullIgnored { volts: 3.65 })
    );
}

/// A real full charge rests at 4.00 to 4.06V, and leaving there keeps 100%.
#[test]
fn leaving_above_the_floor_after_a_real_full_starts_at_a_hundred() {
    let store = store();
    left_full(&store);
    store.observe(SERIAL, off(at(90), LOW));
    store.observe(SERIAL, on(at(100), NOMINAL, 0.0));
    store.observe(SERIAL, on(at(150), FULL, 4.06));

    assert_eq!(
        store.observe(SERIAL, off_reading(at(160), NOMINAL, 4.06)),
        [EnergyEvent::LeftCharger {
            energy: 100.0,
            minutes_left: 25.0,
            runtime_minutes: 25.0,
        }]
    );
}

#[test]
fn a_first_sight_starts_from_his_voltage_as_a_guess_and_teaches_nothing() {
    let store = store();
    assert_eq!(
        store.observe(SERIAL, off(T0, NOMINAL)),
        [EnergyEvent::FirstSeen {
            on_charger: false,
            energy: energy_from_volts(FROZEN),
            volts: FROZEN,
        }]
    );
    let snapshot = store.snapshot(SERIAL, T0).unwrap();
    assert!(snapshot.guess && snapshot.known && !snapshot.on_charger);
    assert_eq!(snapshot.energy_percent, energy_from_volts(FROZEN));
    assert_eq!(snapshot.since_unix_secs, T0 / 1000);
    assert!(close(
        snapshot.runtime_minutes,
        DEFAULT_RUNTIME_SECS as f64 / 60.0
    ));
    assert!(close(
        snapshot.charge_minutes,
        DEFAULT_CHARGE_SECS as f64 / 60.0
    ));

    // His trip began before the server saw him, so LOW teaches nothing.
    assert_eq!(
        store.observe(SERIAL, off(at(10), LOW)),
        [EnergyEvent::Low {
            off_minutes: 10.0,
            runtime: None,
            runtime_minutes: 20.0,
        }]
    );
    assert!(!store.snapshot(SERIAL, at(10)).unwrap().guess);

    let other = store.observe("0000beef", on(T0, NOMINAL, 0.0));
    assert_eq!(
        other,
        [EnergyEvent::FirstSeen {
            on_charger: true,
            energy: 50.0,
            volts: 0.0,
        }]
    );
}

/// A directory under the system temporary directory, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("wirepod-energy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create the temporary directory");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn a_restart_resumes_from_energy_json_or_notices_a_change_while_down() {
    let dir = TempDir::new("restart");
    let data = DataDir::rooted(&dir.0);
    assert!(
        EnergyStore::load(&data)
            .await
            .snapshot(SERIAL, T0)
            .is_none()
    );

    let before = EnergyStore::new(data.energy_path());
    left_full(&before);
    before.save().await.expect("save");
    let bytes = std::fs::read(dir.0.join("energy.json")).expect("read energy.json");
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        format!(
            concat!(
                r#"{{"00303f28":{{"runtime_secs":1200,"runtime_learned":false,"#,
                r#""charge_secs":3600,"charge_learned":false,"on_charger":false,"#,
                r#""anchor_ms":{},"anchor_energy_tenths":1000,"guess":false,"#,
                r#""low_seen":false,"full_seen":false}}}}"#
            ),
            at(60)
        )
    );

    let after = EnergyStore::load(&data).await;
    let snapshot = after.snapshot(SERIAL, at(70)).unwrap();
    assert!(!snapshot.known && !snapshot.on_charger && !snapshot.guess);
    assert!(close(snapshot.energy_percent, 50.0));
    let events = after.observe(SERIAL, off(at(70), NOMINAL));
    assert_eq!(
        events,
        [EnergyEvent::Resumed {
            on_charger: false,
            energy: 50.0,
        }]
    );
    assert_eq!(
        events[0].to_string(),
        "energy: resumed from energy.json at ~50%, off the charger"
    );
    assert!(after.snapshot(SERIAL, at(70)).unwrap().known);

    let changed = EnergyStore::load(&data).await;
    let events = changed.observe(SERIAL, on(at(70), NOMINAL, 3.9));
    assert_eq!(
        events,
        [EnergyEvent::ChangedWhileDown {
            on_charger: true,
            energy: 50.0,
        }]
    );
    assert_eq!(
        events[0].to_string(),
        "energy: he reached the charger while the server was down; assuming it happened now, at ~50%"
    );
    let snapshot = changed.snapshot(SERIAL, at(85)).unwrap();
    assert!(snapshot.on_charger && snapshot.guess);
    assert!(close(snapshot.energy_percent, 75.0));
}

/// A mid-stay model as the code before the FULL floor wrote it loads, starts
/// with no FULL told on its stay, and saves back unchanged.
#[tokio::test]
async fn an_energy_json_in_the_current_format_still_loads() {
    let dir = TempDir::new("current-format");
    let file = dir.0.join("energy.json");
    let saved = format!(
        concat!(
            r#"{{"00303f28":{{"runtime_secs":1140,"runtime_learned":true,"#,
            r#""charge_secs":3840,"charge_learned":true,"on_charger":true,"#,
            r#""anchor_ms":{},"anchor_energy_tenths":250,"guess":false,"#,
            r#""low_seen":false,"full_seen":false}}}}"#
        ),
        at(100)
    );
    std::fs::write(&file, &saved).unwrap();
    let store = EnergyStore::load(&DataDir::rooted(&dir.0)).await;

    // Sixteen minutes of a 64-minute charge from 25%.
    let snapshot = store.snapshot(SERIAL, at(116)).expect("loaded");
    assert!(!snapshot.known && snapshot.on_charger && !snapshot.guess);
    assert_eq!(snapshot.energy_percent, 50.0);
    assert_eq!(
        (snapshot.runtime_minutes, snapshot.charge_minutes),
        (19.0, 64.0)
    );
    assert!(snapshot.runtime_learned && snapshot.charge_learned);
    assert_eq!(
        store.observe(SERIAL, on(at(116), FULL, 3.62)),
        [
            EnergyEvent::Resumed {
                on_charger: true,
                energy: 50.0,
            },
            EnergyEvent::FullIgnored { volts: 3.62 },
        ]
    );

    store.save().await.expect("save");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), saved);
}

#[tokio::test]
async fn an_unreadable_energy_json_starts_empty() {
    let dir = TempDir::new("unreadable");
    std::fs::write(dir.0.join("energy.json"), b"{not json").unwrap();
    let store = EnergyStore::load(&DataDir::rooted(&dir.0)).await;
    assert!(store.snapshot(SERIAL, T0).is_none());
}

#[tokio::test]
async fn the_state_saves_energy_json_at_the_pod_root_by_default() {
    let dir = TempDir::new("state");
    let factory: Arc<dyn RobotConnFactory> =
        Arc::new(FakeConnFactory::failing(ConnError::deadline_exceeded()));
    let state = AppState::builder(factory)
        .paths(Paths::new(DataDir::rooted(&dir.0), AssetDir::new(".")))
        .build();
    state.energy().observe(SERIAL, on(T0, NOMINAL, 4.1));
    state.energy().save().await.expect("save");
    assert!(dir.0.join("energy.json").is_file());
}

#[test]
fn every_event_reads_as_the_log_line() {
    let lines = [
        (
            EnergyEvent::FirstSeen {
                on_charger: true,
                energy: 96.6,
                volts: 4.05,
            },
            "energy: first sight of him on the charger; starting from a guess of ~97% from his reported 4.05V",
        ),
        (
            EnergyEvent::FirstSeen {
                on_charger: false,
                energy: 50.0,
                volts: 0.0,
            },
            "energy: first sight of him off the charger; starting from a guess of ~50% from his reported 0.00V",
        ),
        (
            EnergyEvent::Resumed {
                on_charger: true,
                energy: 81.7,
            },
            "energy: resumed from energy.json at ~82%, on the charger",
        ),
        (
            EnergyEvent::ChangedWhileDown {
                on_charger: false,
                energy: 42.2,
            },
            "energy: he left the charger while the server was down; assuming it happened now, at ~42%",
        ),
        (
            EnergyEvent::LeftCharger {
                energy: 100.0,
                minutes_left: 20.0,
                runtime_minutes: 20.0,
            },
            "energy: left the charger at ~100%, about 20 min before his low-battery flag (runtime 20 min)",
        ),
        (
            EnergyEvent::ReachedCharger {
                off_minutes: 30.2,
                estimate: 0.0,
                measured: Some(60.9),
                volts: 3.85,
                runtime: Some((20.0, 48.4)),
            },
            "energy: back on the charger after 30 min off; estimate ~0%, his docking reading 3.85V gives ~61%; runtime 20 -> 48 min",
        ),
        (
            EnergyEvent::ReachedCharger {
                off_minutes: 4.0,
                estimate: 80.0,
                measured: Some(60.9),
                volts: 3.85,
                runtime: None,
            },
            "energy: back on the charger after 4 min off; estimate ~80%, his docking reading 3.85V gives ~61%",
        ),
        (
            EnergyEvent::ReachedCharger {
                off_minutes: 15.0,
                estimate: 25.0,
                measured: None,
                volts: 0.0,
                runtime: None,
            },
            "energy: back on the charger after 15 min off; estimate ~25%, no usable docking reading (0.00V)",
        ),
        (
            EnergyEvent::Low {
                off_minutes: 30.0,
                runtime: Some((20.0, 25.0)),
                runtime_minutes: 25.0,
            },
            "energy: low-battery flag after 30 min off the charger; runtime 20 -> 25 min",
        ),
        (
            EnergyEvent::Full {
                on_minutes: 50.0,
                charge: Some((60.0, 55.0)),
                charge_minutes: 55.0,
            },
            "energy: charged full after 50 min on the charger; charge time 60 -> 55 min",
        ),
        (
            EnergyEvent::Full {
                on_minutes: 10.0,
                charge: None,
                charge_minutes: 55.0,
            },
            "energy: charged full after 10 min on the charger; charge time kept at 55 min",
        ),
        (
            EnergyEvent::FullIgnored { volts: 3.62 },
            "energy: his battery reports full at 3.62V, too low to be a real charge; ignoring it",
        ),
    ];
    for (event, line) in lines {
        assert_eq!(event.to_string(), line);
    }
}
