//! The energy estimate as JSON. It has no counterpart in the Go server and is
//! recorded in `docs/translation.md` as an addition.
//!
//! `GET /api-energy?serial=<esn>` answers the estimate the battery watchdog
//! keeps from its polls. It only reads the estimate: it dials nothing and
//! leaves the robot's idle clock alone.
//!
//! ```json
//! {
//!   "serial": "00303f28", "known": true, "gohome_percent": 25,
//!   "guess": false, "on_charger": false,
//!   "energy_percent": 50.0, "minutes_left": 10.0,
//!   "runtime_minutes": 20.0, "runtime_learned": false,
//!   "charge_minutes": 60.0, "charge_learned": false,
//!   "since": 1789866000
//! }
//! ```
//!
//! The four figures are rounded to one decimal and `since` is in Unix seconds.
//! `gohome_percent` is the watchdog's go-home threshold, 0 when it is disabled,
//! so the web UI colours the estimate low where the watchdog acts on it.
//! A robot never observed answers `{"serial":"...","known":false,
//! "gohome_percent":25}` and nothing else. `known` is also false, with the
//! rest of the body present, for an estimate read from `energy.json` that no
//! poll has confirmed since the server started.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::response::Response;
use serde::Serialize;
use wirepod_core::robot::energy::EnergySnapshot;
use wirepod_core::{AppState, Esn};

use crate::sdkapp::batterywatchdog::threshold;
use crate::{form, reply};

/// The estimate.
pub const PATH: &str = "/api-energy";

/// `GET /api-energy?serial=<esn>`.
pub async fn handle(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let (_, form) = form::read(req).await;
    let serial = Esn::new(form.get("serial"));
    let snapshot = state.energy().snapshot(serial.as_str(), now_ms(&state));
    let body = Body {
        serial: serial.as_str().to_owned(),
        known: snapshot.is_some_and(|snapshot| snapshot.known),
        // The watchdog takes anything at or below 0 as disabled.
        gohome_percent: threshold(&state).max(0),
        estimate: snapshot.map(Estimate::from),
    };
    reply::json(encode(&body))
}

/// Unix milliseconds on the state's wall clock, the time the estimate is kept
/// in.
pub(crate) fn now_ms(state: &AppState) -> u64 {
    let now = state.wall().now();
    u64::try_from(now.unix_secs)
        .unwrap_or(0)
        .saturating_mul(1000)
        .saturating_add(u64::from(now.nanos / 1_000_000))
}

/// The reply, `serial`, `known` and `gohome_percent` first so a robot never
/// observed answers those three alone.
#[derive(Debug, Serialize)]
struct Body {
    serial: String,
    known: bool,
    gohome_percent: i32,
    #[serde(flatten)]
    estimate: Option<Estimate>,
}

#[derive(Debug, Serialize)]
struct Estimate {
    guess: bool,
    on_charger: bool,
    energy_percent: f64,
    minutes_left: f64,
    runtime_minutes: f64,
    runtime_learned: bool,
    charge_minutes: f64,
    charge_learned: bool,
    since: u64,
}

impl From<EnergySnapshot> for Estimate {
    fn from(snapshot: EnergySnapshot) -> Self {
        Self {
            guess: snapshot.guess,
            on_charger: snapshot.on_charger,
            energy_percent: tenths(snapshot.energy_percent),
            minutes_left: tenths(snapshot.minutes_left),
            runtime_minutes: tenths(snapshot.runtime_minutes),
            runtime_learned: snapshot.runtime_learned,
            charge_minutes: tenths(snapshot.charge_minutes),
            charge_learned: snapshot.charge_learned,
            since: snapshot.since_unix_secs,
        }
    }
}

fn tenths(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// The reply as JSON. Nothing in it can fail to serialize, but the fallback is
/// still the contract's shape rather than a 500.
fn encode(body: &Body) -> String {
    serde_json::to_string(body).unwrap_or_else(|_| {
        serde_json::json!({
            "serial": body.serial,
            "known": false,
            "gohome_percent": body.gohome_percent,
        })
        .to_string()
    })
}
