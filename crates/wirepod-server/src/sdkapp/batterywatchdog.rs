//! Go's `pkg/wirepod/sdkapp/batterywatchdog.go`: poll each online bot's
//! battery and drive it onto the charger when the percentage stays at or below
//! the configured go-home percent.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_util::sync::CancellationToken;
use wirepod_core::logger::COMP_SDK;
use wirepod_core::{AppState, BotStatusKind, Esn, GetRobotError, RobotEntry, Timings};
use wirepod_proto::anki::vector::external_interface as pb;
use wirepod_vector::sdk_client;

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
}

struct CachedConn {
    entry: Arc<RobotEntry>,
    ip: String,
}

/// Go's two package globals, owned by the one task that runs the loop.
#[derive(Default)]
struct Watchdog {
    states: Mutex<HashMap<Esn, BotState>>,
    conns: Mutex<HashMap<Esn, CachedConn>>,
}

/// must match getBatteryPercentage in webroot/js/battery.js so the trigger
/// percent agrees with what the web UI shows
fn battery_percent(volts: f32) -> i32 {
    const MAX_VOLTAGE: f64 = 4.1;
    const MID_VOLTAGE: f64 = 3.85;
    const MIN_VOLTAGE: f64 = 3.5;
    let v = f64::from(volts);
    let mut percentage = if v >= MAX_VOLTAGE {
        100.0
    } else if v >= MID_VOLTAGE {
        let scaled = (v - MID_VOLTAGE) / (MAX_VOLTAGE - MID_VOLTAGE);
        80.0 + 20.0 * (1.0 + scaled * 9.0).log10()
    } else if v >= MIN_VOLTAGE {
        let scaled = (v - MIN_VOLTAGE) / (MID_VOLTAGE - MIN_VOLTAGE);
        80.0 * (1.0 + scaled * 9.0).log10()
    } else if v == 0.0 {
        // no voltage reported (bot booted off charger); the volts > 0 gate
        // in poll_bot keeps this from ever triggering a go-home
        70.0
    } else {
        0.0
    };
    // Go's two separate bounds checks, which clippy will not let stand apart.
    percentage = percentage.round().clamp(0.0, 100.0);
    percentage as i32
}

fn threshold(state: &AppState) -> i32 {
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

        let entry = match self.robot(state, &esn, serial).await {
            Ok(entry) => entry,
            Err(err) => {
                tracing::debug!(target: COMP_SDK, bot = %esn, "battery watchdog: {err}");
                return;
            }
        };
        let reading = match deadline(timings.battery_rpc, entry.conn.battery_state()).await {
            Ok(reading) => reading,
            Err(err) => {
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

        {
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
            if home || volts <= 0.0 {
                bot.consecutive_low = 0;
                bot.attempts = 0;
                bot.cooling_until = Duration::ZERO;
                bot.sent_home = false;
                return;
            }
            if cooling || threshold <= 0 {
                bot.consecutive_low = 0;
                return;
            }
            if percent <= threshold {
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
            bot.docking = true;
        }

        tracing::warn!(target: COMP_SDK, bot = %esn, "battery low ({percent}% <= {threshold}%, {volts:.2}V), sending robot to charger");
        let reached = drive_home(&entry, &esn, &timings).await;

        let mut states = self.states();
        let bot = states.entry(esn.clone()).or_default();
        bot.docking = false;
        bot.sent_home = reached;
        bot.cooling_until = state.clock().now() + timings.battery_cooldown;
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
    let opened = tokio::time::timeout_at(
        until,
        client.behavior_control(UnboundedReceiverStream::new(receiver)),
    )
    .await;
    let mut stream = match opened {
        Ok(Ok(response)) => response.into_inner(),
        Ok(Err(status)) => {
            tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: behavior control: {status}");
            return false;
        }
        Err(_) => {
            tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: behavior control: context deadline exceeded");
            return false;
        }
    };
    let control = pb::BehaviorControlRequest {
        request_type: Some(pb::behavior_control_request::RequestType::ControlRequest(
            pb::ControlRequest {
                priority: pb::control_request::Priority::OverrideBehaviors as i32,
            },
        )),
    };
    if sender.send(control).is_err() {
        tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: control request: the stream is closed");
        return false;
    }
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
                tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: control grant: {status}");
                return false;
            }
            Err(_) => {
                tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: control grant: context deadline exceeded");
                return false;
            }
        }
    }
    // blocks until the dock behavior finishes or the deadline expires
    let docked =
        tokio::time::timeout_at(until, client.drive_on_charger(pb::DriveOnChargerRequest {})).await;
    let _ = sender.send(pb::BehaviorControlRequest {
        request_type: Some(pb::behavior_control_request::RequestType::ControlRelease(
            pb::ControlRelease {},
        )),
    });
    match docked {
        Ok(Ok(_)) => {}
        Ok(Err(status)) => {
            tracing::warn!(target: COMP_SDK, bot = %esn, "battery watchdog: drive on charger: {status}");
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

    use wirepod_core::{BotInfo, RobotConnFactory};
    use wirepod_vector::test_support::{FakeRobotHandle, spawn_fake_robot};
    use wirepod_vector::{TonicConnFactory, plaintext_builder};

    const SERIAL: &str = "00303f28";
    const CEILING: Duration = Duration::from_secs(5);

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

    async fn fixture(addr: SocketAddr) -> Arc<AppState> {
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
        let state = AppState::builder(factory)
            .bot_info(bot_info)
            .timings(Timings {
                battery_poll: Duration::from_millis(1),
                battery_rpc: Duration::from_secs(2),
                battery_dock: Duration::from_secs(2),
                ..Timings::instant()
            })
            .build();
        state.update_config(|config| config.battery.gohome_percent = Some(50));
        // The watchdog only polls robots the jdocs pinger calls online.
        state.with_bot_info(|info| {
            state
                .pinger()
                .note_check(info, &target, state.clock().as_ref())
        });
        state
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

    #[tokio::test]
    async fn three_low_readings_send_the_robot_to_the_charger() {
        let (addr, handle) = spawn_fake_robot().await;
        // 3.6V is about 44%, under the 50% threshold, and off the charger.
        handle.set_battery(1, 3.6);
        let state = fixture(addr).await;
        let cancel = CancellationToken::new();

        start(Arc::clone(&state), cancel.clone());
        wait_for_rpc(&handle, "drive_on_charger").await;
        assert!(handle.methods().contains(&"BehaviorControl"));

        cancel.cancel();
        handle.shutdown().await;
    }
}
