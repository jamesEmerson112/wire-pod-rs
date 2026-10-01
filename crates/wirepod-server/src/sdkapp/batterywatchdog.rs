//! Go's `pkg/wirepod/sdkapp/batterywatchdog.go`: poll each online bot's
//! battery and drive it onto the charger when the percentage stays at or below
//! the configured go-home percent.
//!
//! Each poll also feeds the energy estimate, which has no Go counterpart, and
//! the estimate reaching the go-home percent is a second trigger. His voltage
//! does not change off the charger on this firmware, so off the charger the
//! estimate is the trigger that can fire.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_util::sync::CancellationToken;
use wirepod_core::logger::COMP_SDK;
use wirepod_core::robot::energy::{BatteryObservation, battery_percent};
use wirepod_core::{
    AppState, BatteryLevel, BotStatusKind, Esn, GetRobotError, RobotEntry, Timings,
};
use wirepod_proto::anki::vector::external_interface as pb;
use wirepod_vector::motionlog::{control_granted, control_released};
use wirepod_vector::{logged, sdk_client, status_error};

use crate::energy;

const HYSTERESIS_N: i32 = 3;
const MAX_ATTEMPTS: i32 = 3;

#[derive(Default)]
struct BotState {
    consecutive_low: i32,
    attempts: i32,
    cooling_until: Duration,
    docking: bool,
    have_reading: bool,
    last_home: bool,
    sent_home: bool,
    low_flag_noted: bool,
}

struct CachedConn {
    entry: Arc<RobotEntry>,
    ip: String,
}

/// Why one poll got no reading.
enum PollError {
    /// The robot is not in the bot info, or the registry could not dial it.
    Robot(GetRobotError),
    /// The battery call failed, or the poll ran out of time.
    Battery(String),
}

/// Go's two package globals, owned by the one task that runs the loop.
#[derive(Default)]
struct Watchdog {
    states: Mutex<HashMap<Esn, BotState>>,
    conns: Mutex<HashMap<Esn, CachedConn>>,
}

pub(crate) fn threshold(state: &AppState) -> i32 {
    state.config().battery.gohome_percent.unwrap_or(0)
}

impl Watchdog {
    fn states(&self) -> MutexGuard<'_, HashMap<Esn, BotState>> {
        self.states.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn conns(&self) -> MutexGuard<'_, HashMap<Esn, CachedConn>> {
        self.conns.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Go redials only on IP change or RPC error because `vector.New` leaks its
    /// grpc conn; the registry hands back its own cached entry instead.
    async fn robot(
        &self,
        state: &AppState,
        esn: &Esn,
        serial: &str,
    ) -> Result<Arc<RobotEntry>, GetRobotError> {
        let ip = state
            .with_bot_info(|info| {
                info.robots
                    .iter()
                    .find(|robot| robot.esn == serial)
                    .map(|robot| robot.ip_address.clone())
            })
            .unwrap_or_default();
        if let Some(cached) = self.conns().get(esn)
            && cached.ip == ip
        {
            return Ok(Arc::clone(&cached.entry));
        }
        let entry = state.get_robot(esn).await?;
        self.conns().insert(
            esn.clone(),
            CachedConn {
                entry: Arc::clone(&entry),
                ip,
            },
        );
        Ok(entry)
    }

    fn drop_conn(&self, esn: &Esn) {
        self.conns().remove(esn);
    }

    async fn poll_bot(&self, state: &AppState, serial: &str) {
        let esn = Esn::new(serial);
        let timings = *state.timings();
        let now = state.clock().now();
        let cooling = {
            let mut states = self.states();
            let bot = states.entry(esn.clone()).or_default();
            if bot.docking {
                return;
            }
            now < bot.cooling_until
        };

        // Go's one context covers its lazy dial and the call together, so one
        // bound covers the lookup and the reading. A dial through the registry
        // that never answers would otherwise hold this loop and the robot's
        // connect lock with it.
        let polled = tokio::time::timeout(timings.battery_rpc, async {
            let entry = self
                .robot(state, &esn, serial)
                .await
                .map_err(PollError::Robot)?;
            let reading = entry
                .conn
                .battery_state()
                .await
                .map_err(|err| PollError::Battery(err.to_string()))?;
            Ok((entry, reading))
        })
        .await
        .unwrap_or_else(|_| Err(PollError::Battery("context deadline exceeded".to_owned())));
        let (entry, reading) = match polled {
            Ok(polled) => polled,
            Err(PollError::Robot(err)) => {
                tracing::debug!(target: COMP_SDK, bot = %esn, "battery watchdog: {err}");
                return;
            }
            Err(PollError::Battery(err)) => {
                // transient errors don't touch the counters
                self.drop_conn(&esn);
                tracing::debug!(target: COMP_SDK, bot = %esn, "battery watchdog: battery state: {err}");
                return;
            }
        };

        let volts = reading.volts;
        let percent = battery_percent(volts);
        let threshold = threshold(state);
        let home = reading.is_charging || reading.is_on_charger_platform;

        let now_ms = energy::now_ms(state);
        let events = state.energy().observe(
            esn.as_str(),
            BatteryObservation {
                now_ms,
                home,
                level: reading.level.as_wire(),
                volts,
            },
        );
        for event in &events {
            tracing::info!(target: COMP_SDK, bot = %esn, "{event}");
        }
        if !events.is_empty()
            && let Err(err) = state.energy().save().await
        {
            tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: saving the energy estimate: {err}");
        }
        let estimate = state.energy().snapshot(esn.as_str(), now_ms);

        let energy_low = {
            let mut states = self.states();
            let bot = states.entry(esn.clone()).or_default();
            if bot.have_reading && home != bot.last_home {
                if home {
                    if bot.sent_home {
                        tracing::info!(target: COMP_SDK, bot = %esn, "robot came back home to charge because battery hit the go-home threshold ({percent}%, {volts:.2}V)");
                    } else {
                        tracing::info!(target: COMP_SDK, bot = %esn, "robot is back on the charger ({percent}%, {volts:.2}V)");
                    }
                } else {
                    tracing::info!(target: COMP_SDK, bot = %esn, "robot left the charger ({percent}%, {volts:.2}V)");
                }
            }
            bot.have_reading = true;
            bot.last_home = home;
            if home {
                bot.consecutive_low = 0;
                bot.attempts = 0;
                bot.cooling_until = Duration::ZERO;
                bot.sent_home = false;
                bot.low_flag_noted = false;
                return;
            }
            // Go also resets and returns on a voltage of 0 or less, which a
            // robot switched on off the charger reports until he first docks.
            // Here it only keeps the voltage trigger from firing, so the energy
            // trigger still can.
            if threshold <= 0 {
                bot.consecutive_low = 0;
                return;
            }
            // The estimate is not noisy, so it sends him home on the first
            // poll at or below the threshold. Unlike the voltage trigger it has
            // no cooldown and no give-up, because a low estimate means minutes
            // left, and it stands down once his LOW flag is up, because his
            // emergency behaviour then outranks SDK control.
            let mut energy_low =
                estimate.filter(|estimate| estimate.energy_percent <= f64::from(threshold));
            if energy_low.is_some() && reading.level == BatteryLevel::Low {
                if !bot.low_flag_noted {
                    bot.low_flag_noted = true;
                    tracing::info!(target: COMP_SDK, bot = %esn, "battery watchdog: his low-battery flag is up; leaving the drive home to his own emergency behaviour");
                }
                energy_low = None;
            }
            if energy_low.is_some() {
                bot.consecutive_low = 0;
            } else {
                if cooling {
                    bot.consecutive_low = 0;
                    return;
                }
                if volts > 0.0 && percent <= threshold {
                    bot.consecutive_low += 1;
                } else {
                    bot.consecutive_low = 0;
                }
                if bot.consecutive_low < HYSTERESIS_N {
                    return;
                }
                bot.consecutive_low = 0;
                if bot.attempts >= MAX_ATTEMPTS {
                    bot.attempts = 0;
                    bot.cooling_until = now + timings.battery_give_up_cooldown;
                    tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: charger not reached after repeated attempts, backing off");
                    return;
                }
                bot.attempts += 1;
            }
            bot.docking = true;
            energy_low
        };
        let by_energy = energy_low.is_some();

        match energy_low {
            Some(estimate) => {
                let (left, minutes) = (
                    estimate.energy_percent.round() as i64,
                    estimate.minutes_left.round() as i64,
                );
                tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: energy low (~{left}% <= {threshold}%, about {minutes} min left), sending robot to charger");
            }
            None => {
                tracing::warn!(target: COMP_SDK, bot = %esn, "battery low ({percent}% <= {threshold}%, {volts:.2}V), sending robot to charger");
            }
        }
        let reached = drive_home(&entry, &esn, &timings).await;

        let mut states = self.states();
        let bot = states.entry(esn.clone()).or_default();
        bot.docking = false;
        bot.sent_home = reached;
        if !by_energy {
            bot.cooling_until = state.clock().now() + timings.battery_cooldown;
        }
        if reached {
            bot.attempts = 0;
        }
    }
}

/// Go's `context.WithTimeout` around one RPC, flattened into the error text the
/// log line carries.
async fn deadline<T, F>(limit: Duration, call: F) -> Result<T, String>
where
    F: Future<Output = Result<T, wirepod_core::ConnError>>,
{
    match tokio::time::timeout(limit, call).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(err.to_string()),
        Err(_) => Err("context deadline exceeded".to_owned()),
    }
}

async fn drive_home(entry: &RobotEntry, esn: &Esn, timings: &Timings) -> bool {
    let until = tokio::time::Instant::now() + timings.battery_dock;
    let Some(mut client) = sdk_client(entry.conn.as_ref()) else {
        tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: behavior control: connection has no SDK client");
        return false;
    };
    let (sender, receiver) = mpsc::unbounded_channel();
    let control = pb::BehaviorControlRequest {
        request_type: Some(pb::behavior_control_request::RequestType::ControlRequest(
            pb::ControlRequest {
                priority: pb::control_request::Priority::OverrideBehaviors as i32,
            },
        )),
    };
    // Queued before the call rather than after it, because tonic returns on the
    // response headers and a peer that waits for the first request message
    // would otherwise send them only with its next keep-alive.
    if sender.send(control).is_err() {
        tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: control request: the stream is closed");
        return false;
    }
    let opened = tokio::time::timeout_at(
        until,
        client.behavior_control(UnboundedReceiverStream::new(receiver)),
    )
    .await;
    let mut stream = match opened {
        Ok(Ok(response)) => response.into_inner(),
        Ok(Err(status)) => {
            tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: behavior control: {}", status_error(&status));
            return false;
        }
        Err(_) => {
            tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: behavior control: context deadline exceeded");
            return false;
        }
    };
    loop {
        match tokio::time::timeout_at(until, stream.message()).await {
            Ok(Ok(Some(response))) => {
                if matches!(
                    response.response_type,
                    Some(pb::behavior_control_response::ResponseType::ControlGrantedResponse(_))
                ) {
                    break;
                }
            }
            // Go reads `io.EOF` as an error here, so a closed stream is the
            // same branch as a failed one.
            Ok(Ok(None)) => {
                tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: control grant: the robot closed the stream");
                return false;
            }
            Ok(Err(status)) => {
                tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: control grant: {}", status_error(&status));
                return false;
            }
            Err(_) => {
                tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: control grant: context deadline exceeded");
                return false;
            }
        }
    }
    // The drive home runs with his cliff reaction off, which the grant line
    // says, and it is a motion call like any other, so its answer is logged.
    control_granted(
        COMP_SDK,
        esn.as_str(),
        pb::control_request::Priority::OverrideBehaviors as i32,
    );
    // blocks until the dock behavior finishes or the deadline expires
    let docked = tokio::time::timeout_at(
        until,
        logged(
            COMP_SDK,
            &entry.session,
            "DriveOnCharger",
            "",
            client.drive_on_charger(pb::DriveOnChargerRequest {}),
        ),
    )
    .await;
    control_released(COMP_SDK, esn.as_str());
    let _ = sender.send(pb::BehaviorControlRequest {
        request_type: Some(pb::behavior_control_request::RequestType::ControlRelease(
            pb::ControlRelease {},
        )),
    });
    match docked {
        Ok(Ok(_)) => {}
        Ok(Err(status)) => {
            tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: drive on charger: {}", status_error(&status));
            return false;
        }
        Err(_) => {
            tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: drive on charger: context deadline exceeded");
            return false;
        }
    }
    match deadline(timings.battery_rpc, entry.conn.battery_state()).await {
        Ok(verify) if verify.is_on_charger_platform => {
            tracing::info!(target: COMP_SDK, bot = %esn, "battery watchdog: robot reached the charger");
            true
        }
        _ => {
            tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: robot did not reach the charger");
            false
        }
    }
}

/// Go's `BatteryWatchdog`, which its caller starts as a goroutine.
pub fn start(state: Arc<AppState>, cancel: CancellationToken) {
    tokio::spawn(run(state, cancel));
}

async fn run(state: Arc<AppState>, cancel: CancellationToken) {
    tracing::info!(target: COMP_SDK, "battery go-home watchdog started");
    let watchdog = Watchdog::default();
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(state.timings().battery_poll) => {}
        }
        let statuses =
            state.with_bot_info(|info| state.pinger().snapshot(info, state.clock().as_ref()));
        for status in statuses {
            if status.status != BotStatusKind::Online {
                continue;
            }
            // Go wraps this call in a `recover()`; nothing on this path panics.
            watchdog.poll_bot(&state, &status.esn).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::path::PathBuf;

    use wirepod_core::robot::energy::{DEFAULT_RUNTIME_SECS, EnergyStore, energy_from_volts};
    use wirepod_core::test_support::{FakeConnFactory, FakeRobotConn, ManualWallClock};
    use wirepod_core::wallclock::{WallClock, WallTime};
    use wirepod_core::{BotInfo, Clock, ManualClock, RobotConn, RobotConnFactory};
    use wirepod_vector::test_support::{FakeRobotHandle, spawn_fake_robot};

    use crate::test_support::one_robot;
    use wirepod_vector::{TonicConnFactory, plaintext_builder};

    const SERIAL: &str = "00303f28";
    const CEILING: Duration = Duration::from_secs(5);
    /// 2026-09-20T00:00:00Z.
    const T0: WallTime = WallTime::new(1_789_862_400, 0);
    /// What his controller reports off the charger: the last on-charger reading.
    const FROZEN: f32 = 4.05;
    const LOW: i32 = 1;
    const NOMINAL: i32 = 2;
    const FULL: i32 = 3;
    const COOLDOWN: Duration = Duration::from_secs(600);
    const GIVE_UP_COOLDOWN: Duration = Duration::from_secs(1800);
    const POLL: Duration = Duration::from_secs(30);

    /// A directory under the system temporary directory, removed when dropped.
    /// Every test that polls saves `energy.json` into one of these.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("wirepod-watchdog-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create the temporary directory");
            Self(path)
        }

        fn energy_json(&self) -> PathBuf {
            self.0.join("energy.json")
        }

        fn energy(&self) -> EnergyStore {
            EnergyStore::new(self.energy_json().to_string_lossy().into_owned())
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Go's cooldowns, on a monotonic clock the test moves by hand.
    async fn fixture(
        addr: SocketAddr,
        dir: &TempDir,
        gohome_percent: i32,
    ) -> (Arc<AppState>, Arc<ManualWallClock>, Arc<ManualClock>) {
        let target = addr.to_string();
        let bot_info: BotInfo = serde_json::from_str(&format!(
            concat!(
                r#"{{"global_guid":"global-guid-placeholder","robots":[{{"esn":"{esn}","#,
                r#""ip_address":"{ip}","guid":"robot-guid-placeholder","activated":true}}]}}"#
            ),
            esn = SERIAL,
            ip = target
        ))
        .expect("parse the loopback fixture");
        let factory: Arc<dyn RobotConnFactory> =
            Arc::new(TonicConnFactory::with_endpoint_builder(plaintext_builder()));
        let wall = Arc::new(ManualWallClock::new(T0));
        let clock = Arc::new(ManualClock::new());
        let state = AppState::builder(factory)
            .bot_info(bot_info)
            .timings(Timings {
                battery_poll: Duration::from_millis(1),
                battery_rpc: Duration::from_secs(2),
                battery_dock: Duration::from_secs(2),
                battery_cooldown: COOLDOWN,
                battery_give_up_cooldown: GIVE_UP_COOLDOWN,
                ..Timings::instant()
            })
            .clock(Arc::clone(&clock) as Arc<dyn Clock>)
            .wall(Arc::clone(&wall) as Arc<dyn WallClock>)
            .energy(dir.energy())
            .build();
        state.update_config(|config| config.battery.gohome_percent = Some(gohome_percent));
        // The watchdog only polls robots the jdocs pinger calls online.
        state.with_bot_info(|info| {
            state
                .pinger()
                .note_check(info, &target, state.clock().as_ref())
        });
        (state, wall, clock)
    }

    async fn wait_for_rpc(handle: &FakeRobotHandle, method: &str) {
        tokio::time::timeout(CEILING, async {
            while !handle.methods().contains(&method) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for the robot to answer {method}"));
    }

    fn sent_home(handle: &FakeRobotHandle) -> bool {
        handle.methods().contains(&"drive_on_charger")
    }

    fn count(handle: &FakeRobotHandle, method: &str) -> usize {
        handle
            .methods()
            .iter()
            .filter(|called| **called == method)
            .count()
    }

    /// Off the charger with his voltage frozen, so the estimate is the only
    /// trigger, and drained until it has sent him home once. Every drive fails,
    /// because his charger flags stay down.
    async fn sent_home_on_the_estimate(
        dir: &TempDir,
    ) -> (
        Arc<AppState>,
        Arc<ManualWallClock>,
        FakeRobotHandle,
        Watchdog,
    ) {
        let (addr, handle) = spawn_fake_robot().await;
        handle.set_battery(NOMINAL, FROZEN);
        let (state, wall, _clock) = fixture(addr, dir, 25).await;
        let watchdog = Watchdog::default();
        watchdog.poll_bot(&state, SERIAL).await;
        drain_to_25_percent(&watchdog, &state, &wall, &handle).await;
        assert_eq!(count(&handle, "drive_on_charger"), 1);
        (state, wall, handle, watchdog)
    }

    /// Starts the estimate at 100% as he leaves the charger. While the wall
    /// clock stands still it stays there, so only the voltage can fire.
    fn hold_the_estimate_full(state: &AppState) {
        let now_ms = energy::now_ms(state);
        for (home, level) in [(true, FULL), (false, NOMINAL)] {
            state.energy().observe(
                SERIAL,
                BatteryObservation {
                    now_ms,
                    home,
                    level,
                    volts: 4.1,
                },
            );
        }
    }

    /// Fills the voltage trigger's three low readings and reports whether the
    /// third sent him home.
    async fn three_polls(watchdog: &Watchdog, state: &AppState, handle: &FakeRobotHandle) -> bool {
        let before = count(handle, "drive_on_charger");
        for _ in 0..3 {
            watchdog.poll_bot(state, SERIAL).await;
        }
        count(handle, "drive_on_charger") > before
    }

    /// Runs the off-charger estimate down to 25%: one poll half a minute above
    /// it, which leaves him alone, and one half a minute below it, which sends
    /// him home.
    async fn drain_to_25_percent(
        watchdog: &Watchdog,
        state: &AppState,
        wall: &ManualWallClock,
        handle: &FakeRobotHandle,
    ) {
        let start = state
            .energy()
            .snapshot(SERIAL, energy::now_ms(state))
            .expect("the watchdog observed him")
            .energy_percent;
        assert!(start > 30.0, "{start}");
        let per_second = 100.0 / DEFAULT_RUNTIME_SECS as f64;
        let to_threshold = ((start - 25.0) / per_second).round() as u64;

        wall.advance(Duration::from_secs(to_threshold - 30));
        watchdog.poll_bot(state, SERIAL).await;
        assert!(!sent_home(handle), "sent home above the threshold");

        wall.advance(Duration::from_secs(60));
        watchdog.poll_bot(state, SERIAL).await;
        assert!(sent_home(handle), "not sent home below the threshold");
        assert!(handle.methods().contains(&"BehaviorControl"));
    }

    /// Go's watchdog dials lazily inside its five-second call, so a robot that
    /// never answers costs one poll at most that long.
    #[tokio::test]
    async fn a_dial_that_never_answers_costs_one_poll_its_bound_and_frees_the_robot() {
        let dir = TempDir::new("dial");
        let robot: Arc<dyn RobotConn> = Arc::new(FakeRobotConn::new());
        let factory = Arc::new(FakeConnFactory::connecting_to(robot));
        // Holds the first dial for good, and only the first.
        let _gate = factory.arm_connect_gate();
        let dialler: Arc<dyn RobotConnFactory> = Arc::clone(&factory) as Arc<dyn RobotConnFactory>;
        let state = AppState::builder(dialler)
            .bot_info(one_robot())
            .timings(Timings {
                battery_rpc: Duration::from_millis(100),
                ..Timings::instant()
            })
            .energy(dir.energy())
            .build();
        let watchdog = Watchdog::default();

        tokio::time::timeout(CEILING, watchdog.poll_bot(&state, SERIAL))
            .await
            .expect("a dial that never answered held the poll past its bound");

        // The abandoned dial gave the robot's connect lock back, so the next
        // caller dials again rather than queueing behind it.
        tokio::time::timeout(CEILING, state.get_robot(&Esn::new(SERIAL)))
            .await
            .expect("the abandoned dial still held the robot's connect lock")
            .expect("the second dial connects");
        assert_eq!(factory.connect_count(), 2);
    }

    #[tokio::test]
    async fn three_low_readings_send_the_robot_to_the_charger() {
        let dir = TempDir::new("three-low");
        let (addr, handle) = spawn_fake_robot().await;
        // 3.6V is about 44%, under the 50% threshold, and off the charger.
        handle.set_battery(1, 3.6);
        let (state, _wall, _clock) = fixture(addr, &dir, 50).await;
        let cancel = CancellationToken::new();

        start(Arc::clone(&state), cancel.clone());
        wait_for_rpc(&handle, "drive_on_charger").await;
        assert!(handle.methods().contains(&"BehaviorControl"));

        cancel.cancel();
        handle.shutdown().await;
    }

    /// With the estimate held at 100%, only the voltage can fire, and it still
    /// waits for Go's three low readings in a row.
    #[tokio::test]
    async fn the_voltage_trigger_still_waits_for_three_low_readings() {
        let dir = TempDir::new("voltage");
        let (addr, handle) = spawn_fake_robot().await;
        handle.set_battery(NOMINAL, 3.6);
        let (state, _wall, _clock) = fixture(addr, &dir, 50).await;
        hold_the_estimate_full(&state);
        let watchdog = Watchdog::default();

        for _ in 0..2 {
            watchdog.poll_bot(&state, SERIAL).await;
            assert!(!sent_home(&handle));
        }
        watchdog.poll_bot(&state, SERIAL).await;
        assert!(sent_home(&handle));

        handle.shutdown().await;
    }

    /// Off the charger his voltage stays at the last on-charger reading, so the
    /// voltage trigger never fires and the estimate is what sends him home.
    #[tokio::test]
    async fn the_estimate_sends_him_home_when_his_frozen_voltage_cannot() {
        let dir = TempDir::new("frozen");
        let (addr, handle) = spawn_fake_robot().await;
        // In this order, because `set_battery` clears the charger flags.
        handle.set_battery(NOMINAL, FROZEN);
        handle.set_charger(true, true);
        let (state, wall, _clock) = fixture(addr, &dir, 25).await;
        let watchdog = Watchdog::default();

        // First sight on the charger guesses from his voltage.
        watchdog.poll_bot(&state, SERIAL).await;
        let guess = state
            .energy()
            .snapshot(SERIAL, energy::now_ms(&state))
            .expect("the watchdog observed him")
            .energy_percent;
        assert_eq!(guess, energy_from_volts(FROZEN));
        assert!(dir.energy_json().exists(), "the first sight was not saved");

        handle.set_charger(false, false);
        watchdog.poll_bot(&state, SERIAL).await;
        assert!(!sent_home(&handle));

        drain_to_25_percent(&watchdog, &state, &wall, &handle).await;
        handle.shutdown().await;
    }

    /// A robot switched on off the charger reports 0 V until he first docks.
    /// Go resets and stops there; the estimate still sends him home.
    #[tokio::test]
    async fn a_robot_reporting_no_voltage_off_the_charger_goes_home_on_the_estimate() {
        let dir = TempDir::new("no-voltage");
        let (addr, handle) = spawn_fake_robot().await;
        // With no voltage his level reads FULL until he docks.
        handle.set_battery(FULL, 0.0);
        let (state, wall, _clock) = fixture(addr, &dir, 25).await;
        let watchdog = Watchdog::default();

        watchdog.poll_bot(&state, SERIAL).await;
        assert!(!sent_home(&handle));

        drain_to_25_percent(&watchdog, &state, &wall, &handle).await;
        handle.shutdown().await;
    }

    /// The monotonic clock stands still, so a cooldown after the first drive
    /// would hold the second off.
    #[tokio::test]
    async fn a_failed_drive_home_on_the_estimate_is_tried_again_on_the_next_poll() {
        let dir = TempDir::new("retry");
        let (state, wall, handle, watchdog) = sent_home_on_the_estimate(&dir).await;

        wall.advance(POLL);
        watchdog.poll_bot(&state, SERIAL).await;
        assert_eq!(count(&handle, "drive_on_charger"), 2);

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn the_estimate_never_backs_off_however_many_drives_home_fail() {
        let dir = TempDir::new("no-give-up");
        let (state, wall, handle, watchdog) = sent_home_on_the_estimate(&dir).await;

        let polls = 2 * MAX_ATTEMPTS as usize;
        for _ in 0..polls {
            wall.advance(POLL);
            watchdog.poll_bot(&state, SERIAL).await;
        }
        assert_eq!(count(&handle, "drive_on_charger"), 1 + polls);

        handle.shutdown().await;
    }

    /// His emergency behaviour outranks SDK control once the flag is up, so the
    /// watchdog no longer takes control to drive him.
    #[tokio::test]
    async fn no_drive_home_starts_while_his_low_battery_flag_is_up() {
        let dir = TempDir::new("low-flag");
        let (state, wall, handle, watchdog) = sent_home_on_the_estimate(&dir).await;
        wall.advance(POLL);
        watchdog.poll_bot(&state, SERIAL).await;
        assert_eq!(count(&handle, "drive_on_charger"), 2);

        handle.set_battery(LOW, FROZEN);
        for _ in 0..3 {
            wall.advance(POLL);
            watchdog.poll_bot(&state, SERIAL).await;
        }
        assert_eq!(count(&handle, "drive_on_charger"), 2);
        assert_eq!(count(&handle, "BehaviorControl"), 2);

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn the_voltage_trigger_keeps_gos_cooldown_and_give_up() {
        let dir = TempDir::new("voltage-cooldown");
        let (addr, handle) = spawn_fake_robot().await;
        handle.set_battery(NOMINAL, 3.6);
        let (state, _wall, clock) = fixture(addr, &dir, 50).await;
        hold_the_estimate_full(&state);
        let watchdog = Watchdog::default();

        for attempt in 1..=MAX_ATTEMPTS {
            assert!(
                three_polls(&watchdog, &state, &handle).await,
                "attempt {attempt} was not made"
            );
            assert!(
                !three_polls(&watchdog, &state, &handle).await,
                "attempt {attempt} was repeated inside the cooldown"
            );
            clock.advance(COOLDOWN);
        }
        assert!(
            !three_polls(&watchdog, &state, &handle).await,
            "a fourth attempt instead of the give-up"
        );
        clock.advance(GIVE_UP_COOLDOWN - Duration::from_secs(1));
        assert!(
            !three_polls(&watchdog, &state, &handle).await,
            "an attempt inside the give-up"
        );
        clock.advance(Duration::from_secs(1));
        assert!(
            three_polls(&watchdog, &state, &handle).await,
            "no attempt after the give-up"
        );

        handle.shutdown().await;
    }
}
