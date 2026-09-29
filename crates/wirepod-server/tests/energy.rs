//! `GET /api-energy`, through the router, over an estimate seeded directly.
//!
//! The wall clock is fixed, so the figures the route rounds are exact. The one
//! test that saves works in a directory under the system temporary directory,
//! named for the process, and removes it afterwards.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use http::Method;
use wirepod_core::paths::DataDir;
use wirepod_core::robot::energy::{BatteryObservation, EnergyStore};
use wirepod_core::test_support::{FakeConnFactory, ManualWallClock};
use wirepod_core::wallclock::{WallClock, WallTime};
use wirepod_core::{AppState, RobotConnFactory};
use wirepod_server::build_router;
use wirepod_server::test_support::{Reply, one_robot, request, send_to, unreachable_error};

const SERIAL: &str = "00303f28";
/// 2026-09-20T00:00:00Z, in seconds and in milliseconds.
const T0_SECS: i64 = 1_789_862_400;
const T0_MS: u64 = 1_789_862_400_000;
const NOMINAL: i32 = 2;
const FULL: i32 = 3;

/// A router over `energy`, reading the wall clock at `now`, whose robot is
/// never dialled.
fn router(energy: EnergyStore, now: WallTime) -> (Router, Arc<AppState>, Arc<FakeConnFactory>) {
    let factory = Arc::new(FakeConnFactory::failing(unreachable_error()));
    let dialler: Arc<dyn RobotConnFactory> = Arc::clone(&factory) as Arc<dyn RobotConnFactory>;
    let state = AppState::builder(dialler)
        .bot_info(one_robot())
        .wall(Arc::new(ManualWallClock::new(now)) as Arc<dyn WallClock>)
        .energy(energy)
        .build();
    (build_router(Arc::clone(&state)), state, factory)
}

/// Full on the charger at `T0`, and off it at the same instant.
fn left_full(store: &EnergyStore) {
    for (home, level) in [(true, FULL), (false, NOMINAL)] {
        store.observe(
            SERIAL,
            BatteryObservation {
                now_ms: T0_MS,
                home,
                level,
                volts: 4.1,
            },
        );
    }
}

async fn get(router: &Router, uri: &str) -> Reply {
    let reply = send_to(router, request(Method::GET, uri, None)).await;
    assert_eq!(reply.status, http::StatusCode::OK, "{uri}");
    assert_eq!(reply.content_type(), Some("application/json"), "{uri}");
    reply
}

/// A directory under the system temporary directory, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "wirepod-energy-route-{name}-{}",
            std::process::id()
        ));
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
async fn the_route_answers_the_estimate_rounded_and_dials_nothing() {
    let store = EnergyStore::new("./energy-never-written.json");
    left_full(&store);
    // 10 min 7 s off at 20 min a charge: 49.42% and 9.88 min.
    let (router, _state, factory) = router(store, WallTime::new(T0_SECS + 607, 0));

    // The serial is matched as the watchdog stores it, trimmed and lowercased.
    let reply = get(&router, "/api-energy?serial=00303F28").await;
    assert_eq!(
        reply.body,
        concat!(
            r#"{"serial":"00303f28","known":true,"gohome_percent":0,"#,
            r#""guess":false,"on_charger":false,"#,
            r#""energy_percent":49.4,"minutes_left":9.9,"#,
            r#""runtime_minutes":20.0,"runtime_learned":false,"#,
            r#""charge_minutes":60.0,"charge_learned":false,"since":1789862400}"#
        )
    );
    assert_eq!(factory.connect_count(), 0);
}

#[tokio::test]
async fn a_robot_never_observed_answers_known_false_and_nothing_else() {
    let store = EnergyStore::new("./energy-never-written.json");
    left_full(&store);
    let (router, _state, factory) = router(store, WallTime::new(T0_SECS, 0));

    for (uri, body) in [
        (
            "/api-energy?serial=DEADBEEF",
            r#"{"serial":"deadbeef","known":false,"gohome_percent":0}"#,
        ),
        (
            "/api-energy",
            r#"{"serial":"","known":false,"gohome_percent":0}"#,
        ),
    ] {
        assert_eq!(get(&router, uri).await.body, body, "{uri}");
    }
    assert_eq!(factory.connect_count(), 0);
}

/// The watchdog's threshold rides along in both shapes, so the web UI can
/// colour the estimate low where the watchdog acts on it, and a negative one
/// reads as disabled, as the watchdog takes it.
#[tokio::test]
async fn both_shapes_carry_the_go_home_threshold() {
    let store = EnergyStore::new("./energy-never-written.json");
    left_full(&store);
    let (router, state, _factory) = router(store, WallTime::new(T0_SECS + 607, 0));

    for (gohome, answered) in [(Some(25), 25), (Some(-5), 0), (None, 0)] {
        state.update_config(|config| config.battery.gohome_percent = gohome);
        let known = get(&router, "/api-energy?serial=00303f28").await;
        let prefix = format!(
            r#"{{"serial":"00303f28","known":true,"gohome_percent":{answered},"guess":false,"#
        );
        assert!(
            known.body.starts_with(&prefix),
            "{gohome:?}: {}",
            known.body
        );
        assert_eq!(
            get(&router, "/api-energy?serial=deadbeef").await.body,
            format!(r#"{{"serial":"deadbeef","known":false,"gohome_percent":{answered}}}"#),
            "{gohome:?}"
        );
    }
}

/// An estimate read back from `energy.json` answers in full, with `known`
/// false until a poll confirms it.
#[tokio::test]
async fn an_estimate_no_poll_has_confirmed_since_a_restart_is_not_known() {
    let dir = TempDir::new("restored");
    let data = DataDir::rooted(&dir.0);
    let saved = EnergyStore::new(data.energy_path());
    left_full(&saved);
    saved.save().await.expect("save energy.json");

    let (router, state, _factory) = router(
        EnergyStore::load(&data).await,
        WallTime::new(T0_SECS + 600, 0),
    );
    let reply = get(&router, "/api-energy?serial=00303f28").await;
    assert!(
        reply
            .body
            .starts_with(r#"{"serial":"00303f28","known":false,"gohome_percent":0,"guess":false,"on_charger":false,"energy_percent":50.0,"#),
        "{}",
        reply.body
    );

    state.energy().observe(
        SERIAL,
        BatteryObservation {
            now_ms: T0_MS + 600_000,
            home: false,
            level: NOMINAL,
            volts: 4.1,
        },
    );
    let reply = get(&router, "/api-energy?serial=00303f28").await;
    assert!(
        reply
            .body
            .starts_with(r#"{"serial":"00303f28","known":true,"#),
        "{}",
        reply.body
    );
}
