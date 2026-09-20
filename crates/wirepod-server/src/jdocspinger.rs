//! Go's `pkg/wirepod/sdkapp/jdocspinger.go`: the conn-check side effects and the jdocs pull.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent};
use wirepod_core::logger::COMP_SDK;
use wirepod_core::persist::WriteGate;
use wirepod_core::{AppState, Esn, JdocKind, host_of, marshal_bot_info};

/// The mode `jdocspinger.go:250` writes the bot-info file with, where every
/// other writer of it uses `0644`.
const PINGER_BOT_INFO_FILE_MODE: u32 = 0o777;

/// Go's service type and domain (`jdocspinger.go:238`), as `mdns-sd` spells the
/// pair.
const ANKI_VECTOR_SERVICE: &str = "_ankivector._tcp.local.";

/// How long Go's browse runs before its context expires (`jdocspinger.go:234`).
const MDNS_BROWSE: Duration = Duration::from_secs(5);

/// The wait before the pull when the robot is in escape-pod mode
/// (`jdocspinger.go:255-257`).
const EP_CONFIG_SETTLE: Duration = Duration::from_secs(1);

/// Whether [`run_mdns`] may put traffic on the LAN.
///
/// Go has no such switch. It is here because nothing starts the server yet and
/// because a browse reaches the real robot and the production Go server on this
/// network, so the browse stays off until a boot path calls
/// [`set_mdns_enabled`].
static MDNS_ENABLED: AtomicBool = AtomicBool::new(false);

/// Go's `MDNSAlreadyRun` (`jdocspinger.go:223`).
static MDNS_ALREADY_RUN: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Turns the mDNS browse on or off.
pub fn set_mdns_enabled(enabled: bool) {
    MDNS_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Go's `pingJdocs` (`jdocspinger.go:79-127`).
pub async fn ping_jdocs(state: &Arc<AppState>, target: &str) {
    let target = host_of(target).trim().to_ascii_lowercase();
    // Go's loop has no break, so the last matching robot is the one whose
    // serial survives.
    let serial = state.with_bot_info(|info| {
        info.robots
            .iter()
            .filter(|robot| robot.ip_address.trim().to_ascii_lowercase() == target)
            .map(|robot| robot.esn.clone())
            .next_back()
    });
    let Some(serial) = serial else {
        tracing::error!(target: COMP_SDK, "jdocs pinger: serial not in bot json");
        return;
    };
    let esn = Esn::new(&serial);

    let mut robot = match state.get_robot(&esn).await {
        Ok(robot) => robot,
        Err(err) => {
            tracing::error!(target: COMP_SDK, bot = %serial, "error pinging jdocs: {err}");
            return;
        }
    };
    if robot.conn.battery_state().await.is_err() {
        // Go dials a second, independent connection here; the registry hands
        // back the one it already holds.
        robot = match state.get_robot(&esn).await {
            Ok(robot) => robot,
            Err(err) => {
                tracing::error!(target: COMP_SDK, bot = %serial, "error pinging jdocs: {err}");
                return;
            }
        };
        if robot.conn.battery_state().await.is_err() {
            tracing::error!(target: COMP_SDK, bot = %serial, "ping failed, likely unauthenticated");
            return;
        }
    }

    let named = match robot.conn.pull_jdocs(&[JdocKind::RobotSettings]).await {
        Ok(named) => named,
        Err(err) => {
            tracing::error!(target: COMP_SDK, bot = %serial, "pull jdocs: {err}");
            return;
        }
    };
    tracing::info!(target: COMP_SDK, bot = %serial, "pulled jdocs");
    // Go indexes `NamedJdocs[0]` without checking the length.
    let Some(first) = named.first() else {
        tracing::error!(target: COMP_SDK, bot = %serial, "pull jdocs: the robot returned no document");
        return;
    };
    let outcome = state
        .jdocs()
        .add_jdoc(
            &format!("vic:{serial}"),
            "vic.RobotSettings",
            first.doc.clone(),
        )
        .await;
    if let Err(err) = outcome.written {
        tracing::warn!(target: COMP_SDK, bot = %serial, "write jdocs: {err}");
    }
}

/// Go's `InitJdocsPinger` (`jdocspinger.go:129-150`).
///
/// Go's one-second ticker only ages a counter that decides `Stopped`;
/// `PingerState` derives the same age from the clock, so there is no task to
/// start here.
pub fn init_jdocs_pinger(state: &AppState) {
    if std::env::var("JDOCS_PINGER_ENABLED").as_deref() == Ok("false") {
        tracing::debug!(target: COMP_SDK, "jdocs pinger disabled (JDOCS_PINGER_ENABLED=false)");
        state.pinger().set_enabled(false);
        return;
    }
    tracing::debug!(comp = "", "Starting jdocs pinger ticker");
}

/// Go's `ShouldPingJdocs` (`jdocspinger.go:152-191`).
pub fn should_ping_jdocs(state: &AppState, target: &str) -> bool {
    state.with_bot_info(|info| {
        state
            .pinger()
            .note_check(info, target, state.clock().as_ref())
    })
}

/// Go's `RunMDNS` (`jdocspinger.go:227-269`).
pub async fn run_mdns(state: Arc<AppState>, bot_ip: String) {
    {
        let already = MDNS_ALREADY_RUN
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if already.contains(&bot_ip) {
            return;
        }
    }
    if !MDNS_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    tracing::debug!(comp = "", "Running mDNS...");

    let daemon = match ServiceDaemon::new() {
        Ok(daemon) => daemon,
        Err(err) => {
            tracing::error!(target: COMP_SDK, "mdns: {err}");
            return;
        }
    };
    let entries = match daemon.browse(ANKI_VECTOR_SERVICE) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::error!(target: COMP_SDK, "mdns: {err}");
            return;
        }
    };

    let browse = async {
        while let Ok(event) = entries.recv_async().await {
            let ServiceEvent::ServiceResolved(info) = event else {
                continue;
            };
            let Some(address) = info.get_addresses_v4().into_iter().next() else {
                continue;
            };
            // Go takes `AddrIPv4[0]`; the addresses arrive here as a set, so
            // a robot answering on two interfaces can be recorded under either.
            let address = address.to_string();
            let hostname = info.get_hostname().to_owned();
            let robot_id = hostname.split('.').next().unwrap_or(&hostname).to_owned();

            let Some(esn) = state
                .session_certs()
                .snapshot()
                .into_iter()
                .find(|rinf| rinf.id == robot_id)
                .map(|rinf| rinf.esn)
            else {
                MDNS_ALREADY_RUN
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(bot_ip.clone());
                continue;
            };

            state
                .session_certs()
                .add_to_r_info(&esn, &robot_id, &address);
            let mut bot_info = state.bot_info_snapshot();
            for index in 0..bot_info.robots.len() {
                if bot_info.robots[index].esn == esn {
                    bot_info.robots[index].ip_address = address.clone();
                    state.set_bot_info(bot_info.clone());
                    let bytes = marshal_bot_info(&bot_info);
                    tracing::debug!(comp = "", "Updating robot {robot_id}");
                    let gate = WriteGate::new(
                        state.paths().data().bot_info_path(),
                        PINGER_BOT_INFO_FILE_MODE,
                    );
                    if let Err(err) = gate.write(move || bytes).await {
                        tracing::warn!(target: COMP_SDK, bot = %esn, "write bot info: {err}");
                    }
                }
            }

            let pinging = Arc::clone(&state);
            let pinged = address.clone();
            tokio::spawn(async move {
                if pinging.config().server.epconfig {
                    tokio::time::sleep(EP_CONFIG_SETTLE).await;
                }
                ping_jdocs(&pinging, &pinged).await;
            });
        }
    };

    let _ = tokio::time::timeout(MDNS_BROWSE, browse).await;
    let _ = daemon.shutdown();
    tracing::debug!(comp = "", "Done running mDNS");
}
