//! Go's `cmd/vosk/main.go`: load the state, start the chipper listeners and
//! serve the HTTP surface.
//!
//! Go's `main` hands `StartFromProgramInit` the speech engine's three functions.
//! The engine is M3 work, so the chipper service starts here with no voice
//! processor and answers the three streaming RPCs as unimplemented.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, Layer};
use wirepod_core::logger::{LogLayer, LogRing, is_wire_pod_target};
use wirepod_core::paths::{AssetDir, DataDir, sdk_ini_dir};
use wirepod_core::wallclock::{SystemWallClock, WallClock};
use wirepod_core::{
    AppState, EnergyStore, Env, JdocsStore, Paths, SdkIniStore, SessionCertStore, WallLogClock,
    read_bot_info, read_config,
};
use wirepod_server::chipper::{Options, Server};
use wirepod_server::{CONN_CHECK_PORT, DEFAULT_WEB_PORT, startserver};
use wirepod_vector::{CONNECT_TIMEOUT, TonicConnFactory};

use crate::args::ServeArgs;
use crate::sdk_trial::{DEFAULT_FILTER, FILTER_ENV};

/// Go's `logger.Init` appends every line to this file when it is set.
const LOG_FILE_ENV: &str = "LOG_FILE";

/// Go's `vars.Init` reads the web port from this variable.
const WEBSERVER_PORT_ENV: &str = "WEBSERVER_PORT";

#[derive(Debug)]
pub enum ServeError {
    NoAppData,
    NoHome,
    Bind(String, std::io::Error),
    Serve(std::io::Error),
}

impl fmt::Display for ServeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAppData => write!(f, "APPDATA is not set; pass --data-dir <path>"),
            Self::NoHome => write!(
                f,
                "neither USERPROFILE nor HOME is set; pass --sdk-ini-dir <path>"
            ),
            Self::Bind(addr, err) => write!(f, "cannot bind {addr}: {err}"),
            Self::Serve(err) => write!(f, "the listener failed: {err}"),
        }
    }
}

impl std::error::Error for ServeError {}

/// What `serve` and the tray boot from. The tray cannot set variables, so it
/// passes what Go's `StartWirePod` and `onReady` set in the environment.
pub struct Boot {
    pub args: ServeArgs,
    pub env: Env,
    /// `WEBSERVER_PORT`.
    pub webserver_port: Option<String>,
    /// Go's `vars.Packaged`, which only the tray sets. `--packaged` selects the
    /// data layout alone.
    pub packaged: bool,
}

impl Boot {
    /// The console's boot, which reads the process environment.
    pub fn from_process(args: ServeArgs) -> Self {
        Self {
            args,
            env: Env::from_process(),
            webserver_port: std::env::var(WEBSERVER_PORT_ENV).ok(),
            packaged: false,
        }
    }
}

/// What [`init`] loaded.
pub struct Booted {
    pub state: Arc<AppState>,
    /// Go's `vars.WebPort`.
    pub web_port: String,
}

/// The part of Go's `vars.Init` that reads the state files.
async fn load_state(
    args: &ServeArgs,
    env: &Env,
    packaged: bool,
    logs: Arc<LogRing>,
    wall: Arc<dyn WallClock>,
) -> Result<Arc<AppState>, ServeError> {
    let data = match (&args.data_dir, args.packaged) {
        (Some(dir), _) => DataDir::rooted(dir),
        (None, true) => DataDir::packaged(&PathBuf::from(
            std::env::var_os("APPDATA").ok_or(ServeError::NoAppData)?,
        )),
        (None, false) => DataDir::source(),
    };
    let assets = AssetDir::new(args.asset_dir.clone().unwrap_or_else(|| PathBuf::from(".")));
    let sdk_ini = match &args.sdk_ini_dir {
        Some(dir) => SdkIniStore::new(format!("{}/", dir.display())),
        None => {
            let home = std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .ok_or(ServeError::NoHome)?;
            SdkIniStore::new(sdk_ini_dir(&PathBuf::from(home)))
        }
    };

    let gate = wirepod_core::config::config_gate(&data);
    let mut config = read_config(env, &gate).await.config;
    if let Some(port) = args.tls_port {
        // A trial override, held in memory only.
        config.server.port = port.to_string();
    }
    let bot_info = read_bot_info(&data).await.unwrap_or_default();
    let jdocs = JdocsStore::load(&data).await.store;
    let session_certs = SessionCertStore::load(&data, &bot_info).await.store;
    let energy = EnergyStore::load(&data).await;
    let custom_intents = wirepod_core::intents::load_custom_intents(&data);

    let state = AppState::builder(Arc::new(TonicConnFactory::insecure_tls()))
        .paths(Paths::new(data, assets))
        .config(config)
        .config_gate(gate)
        .bot_info(bot_info)
        .jdocs(jdocs)
        .session_certs(session_certs)
        .energy(energy)
        .sdk_ini(sdk_ini)
        .logs(logs)
        .wall(wall)
        // Go's connect-time event stream, which this server reads for the
        // robot's own state.
        .state_stream(true)
        // grpc-go's connection attempt waits up to this long for the server's
        // first frame, which tonic's dial does not wait for at all, so the
        // liveness call is where a robot that goes silent after the handshake
        // is caught.
        .liveness_deadline(Some(CONNECT_TIMEOUT))
        .packaged(packaged)
        .build();
    *state
        .custom_intents()
        .lock()
        .unwrap_or_else(|err| err.into_inner()) = custom_intents;
    Ok(state)
}

/// Go's `WebPort` rule from `vars.Init`. `--web-port` overrides it.
pub fn web_port(flag: Option<u16>, webserver_port: Option<&str>) -> String {
    if let Some(port) = flag {
        return port.to_string();
    }
    match webserver_port {
        // Go's `strconv.Atoi`, which takes a sign and a number too large for a
        // port; the bind refuses those.
        Some(value) if !value.is_empty() => {
            if value.parse::<i64>().is_ok() {
                value.to_owned()
            } else {
                tracing::info!("WEBSERVER_PORT contains letters, using default of 8080");
                DEFAULT_WEB_PORT.to_string()
            }
        }
        _ => DEFAULT_WEB_PORT.to_string(),
    }
}

/// Go's `logger.Init` and `vars.Init`.
pub async fn init(boot: &Boot) -> Result<Booted, ServeError> {
    // Go's `logger.Init`: the ring the web UI's log page reads, fed by every
    // `tracing` event alongside the console.
    let wall: Arc<dyn WallClock> = Arc::new(SystemWallClock::new());
    let clock = Arc::new(WallLogClock::new(Arc::clone(&wall)));
    let logs = Arc::new(match std::env::var_os(LOG_FILE_ENV) {
        Some(path) if !path.is_empty() => LogRing::with_log_file(clock, Path::new(&path)),
        _ => LogRing::new(clock),
    });
    install_logging(Arc::clone(&logs));
    let state = load_state(&boot.args, &boot.env, boot.packaged, logs, wall).await?;
    let web_port = web_port(boot.args.web_port, boot.webserver_port.as_deref());
    wirepod_server::jdocspinger::init_jdocs_pinger(&state);
    Ok(Booted { state, web_port })
}

pub async fn run(args: ServeArgs) -> Result<(), ServeError> {
    let boot = Boot::from_process(args);
    let booted = init(&boot).await?;

    let cancel = CancellationToken::new();
    let stopper = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("serve: Ctrl-C, shutting down");
        }
        stopper.cancel();
    });

    start(&boot.args, booted, cancel).await
}

/// Go serves one mux on the web port, from `StartWebServer`, and on port 80,
/// from `BeginServer`. Only the web port is fatal.
async fn bind_listeners(
    bind: &str,
    web_port: &str,
    http_port: u16,
    packaged: bool,
) -> Result<Vec<TcpListener>, ServeError> {
    let mut listeners = Vec::new();

    let addr = format!("{bind}:{web_port}");
    match TcpListener::bind(&addr).await {
        Ok(listener) => {
            println!("serve: http on {addr}");
            listeners.push(listener);
        }
        Err(err) => {
            tracing::info!("Error binding to {web_port}: {err}");
            if packaged {
                let msg = format!(
                    "FATAL: Wire-pod was unable to bind to port {web_port}. Another process is likely using it. Exiting."
                );
                let _ = tokio::task::spawn_blocking(move || wirepod_core::msg::err_msg(&msg)).await;
            }
            return Err(ServeError::Bind(addr, err));
        }
    }

    let addr = format!("{bind}:{http_port}");
    match TcpListener::bind(&addr).await {
        Ok(listener) => {
            println!("serve: http on {addr}");
            listeners.push(listener);
        }
        Err(_) => {
            // Go shows the box on the goroutine that bound port 80, so the rest
            // of the server starts beside it.
            tokio::task::spawn_blocking(move || {
                if packaged {
                    wirepod_core::msg::warn_msg(
                        "A process is using port 80. Wire-pod will keep running, but connCheck functionality will not work, so your bot may not always stay connected to your wire-pod instance.",
                    );
                }
                tracing::info!(
                    "A process is already using port 80 - connCheck functionality will not work"
                );
            });
        }
    }
    Ok(listeners)
}

/// The listeners and Go's `StartFromProgramInit`, until `cancel` fires.
pub async fn start(
    args: &ServeArgs,
    booted: Booted,
    cancel: CancellationToken,
) -> Result<(), ServeError> {
    let Booted { state, web_port } = booted;

    // Go's `BeginServer` starts both of these beside the HTTP surface.
    // Beside the Go server its own watchdog is already sending Vector home.
    if !args.web_only {
        wirepod_server::sdkapp::batterywatchdog::start(Arc::clone(&state), cancel.clone());
    }
    {
        let (state, cancel) = (Arc::clone(&state), cancel.clone());
        tokio::spawn(async move { state.registry().run_conn_timer(cancel).await });
    }

    let router = wirepod_server::build_router(Arc::clone(&state));
    let mut plain = Vec::new();
    for listener in bind_listeners(
        &args.bind,
        &web_port,
        args.http_port.unwrap_or(CONN_CHECK_PORT),
        state.packaged(),
    )
    .await?
    {
        plain.push(tokio::spawn(wirepod_server::serve_plain(
            listener,
            router.clone(),
            cancel.clone(),
        )));
    }

    if args.web_only {
        println!("serve: web only, the chipper listeners and mDNS stay off");
    } else {
        // Go's conn check browses for a robot whose address it does not know.
        wirepod_server::jdocspinger::set_mdns_enabled(true);
        startserver::start_from_program_init(Arc::clone(&state), voice_processor(&state)).await;
    }

    for task in plain {
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => return Err(ServeError::Serve(err)),
            Err(err) => return Err(ServeError::Serve(std::io::Error::other(err))),
        }
    }
    startserver::stop_server().await;
    Ok(())
}

fn install_logging(logs: Arc<LogRing>) {
    let filter = match std::env::var(FILTER_ENV) {
        Ok(value) => EnvFilter::new(value),
        Err(_) => EnvFilter::new(DEFAULT_FILTER),
    };
    subscriber(logs, filter).init();
}

/// Go's ring keeps every level and `DEBUG_LOGGING` gates only the stdout copy,
/// so the filter goes on the console layer alone and the ring takes every level
/// of wire-pod's own targets that Go has. Go has no trace, and the port's trace
/// lines, such as the dashboard's three-second protocol probe, are there to stay
/// out of the web UI's log.
fn subscriber(
    logs: Arc<LogRing>,
    console: EnvFilter,
) -> impl tracing::Subscriber + Send + Sync + 'static {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(console))
        .with(LogLayer::new(logs).with_filter(filter_fn(|meta| {
            is_wire_pod_target(meta.target()) && *meta.level() <= tracing::Level::DEBUG
        })))
}

/// Go's `wp.New(stt.Init, stt.STT, stt.Name)`, which builds the voice processor
/// and hands it to the chipper service as all three request processors.
///
/// The engine links against libvosk, so it is compiled in only under the
/// `stt-vosk` feature. Without it the service is built with no processor at all
/// and answers the three streaming RPCs as unimplemented, which is what the
/// Go server does when its engine fails to start.
#[cfg(feature = "stt-vosk")]
fn voice_processor(state: &Arc<AppState>) -> Server {
    use wirepod_server::vtt::{IntentGraphProcessor, IntentProcessor, KgProcessor};
    use wirepod_stt::vosk::{Vosk, VoskConfig};

    let config = state.config();
    let data = state.paths().data();
    let assets = state.paths().assets();
    let intent_list = wirepod_core::intents::load_intents(assets, &config.stt.language)
        .inspect_err(|err| tracing::info!(comp = "", "{err}"))
        .unwrap_or_default();
    let engine = Arc::new(Vosk::new(VoskConfig {
        past_initial_setup: config.past_initial_setup,
        stt_language: config.stt.language.clone(),
        intent_graph: config.knowledge.intentgraph,
        vosk_model_path: data.vosk_model_dir(),
        stttest_path: assets.stttest_path(),
        intent_list,
        custom_intents: wirepod_core::intents::load_custom_intents(data).unwrap_or_default(),
    }));

    match wirepod_ttr::preqs::server::Server::new(Arc::clone(state), engine) {
        Ok(processor) => {
            // Go hands the same `*preqs.Server` to all three option functions.
            let processor = Arc::new(processor);
            // Each binding is what coerces the concrete type to its trait
            // object; `Arc::clone` on its own cannot.
            let intent: Arc<dyn IntentProcessor> = processor.clone();
            let kg: Arc<dyn KgProcessor> = processor.clone();
            let intent_graph: Arc<dyn IntentGraphProcessor> = processor;
            Server::new(
                Options::new()
                    .with_intent_processor(intent)
                    .with_knowledge_graph_processor(kg)
                    .with_intent_graph_processor(intent_graph),
            )
        }
        Err(err) => {
            tracing::info!(comp = "", "{err}");
            Server::new(Options::new())
        }
    }
}

/// The same entry point with no engine compiled in.
#[cfg(not(feature = "stt-vosk"))]
fn voice_processor(_state: &Arc<AppState>) -> Server {
    println!("serve: built without a speech engine, so voice commands are off");
    Server::new(Options::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wirepod_core::logger::{LogLevel, ManualLogClock};

    #[test]
    fn debug_lines_on_wire_pod_targets_reach_the_ring_under_the_default_filter() {
        let ring = Arc::new(LogRing::new(Arc::new(ManualLogClock::new(
            1_000,
            "2026.01.02 03:04:05",
        ))));
        let subscriber = subscriber(Arc::clone(&ring), EnvFilter::new(DEFAULT_FILTER));
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(target: "sdkapp", "motion line");
            tracing::debug!(target: "stt", "transcription line");
            tracing::debug!(target: "h2::codec", "a library's frame");
            tracing::trace!(target: "wirepod_vector::conn", "the dashboard's protocol probe");
        });

        let messages: Vec<String> = ring
            .get_entries(LogLevel::Debug, 0)
            .into_iter()
            .map(|entry| entry.msg)
            .collect();
        assert_eq!(messages, ["motion line", "transcription line"]);
    }

    #[test]
    fn webserver_port_sets_the_web_port_unless_it_has_letters_and_the_flag_wins() {
        assert_eq!(web_port(None, Some("8081")), "8081");
        assert_eq!(web_port(None, Some("80a")), "8080");
        assert_eq!(web_port(None, Some("")), "8080");
        assert_eq!(web_port(None, None), "8080");
        assert_eq!(web_port(Some(18080), Some("8081")), "18080");
        assert_eq!(web_port(Some(18080), Some("80a")), "18080");
    }

    #[tokio::test]
    async fn the_server_bounds_the_connect_time_liveness_call() {
        let root = std::env::temp_dir().join(format!("wirepod-serve-{}", std::process::id()));
        let args = ServeArgs {
            data_dir: Some(root.join("data")),
            sdk_ini_dir: Some(root.join("sdk")),
            ..ServeArgs::default()
        };
        let logs = Arc::new(LogRing::new(Arc::new(ManualLogClock::new(
            1_000,
            "2026.01.02 03:04:05",
        ))));
        let state = load_state(
            &args,
            &Env::default(),
            false,
            logs,
            Arc::new(SystemWallClock::new()),
        )
        .await;
        let _ = std::fs::remove_dir_all(&root);

        let state = state.expect("the state loads from an empty data directory");
        assert_eq!(state.registry().liveness_deadline(), Some(CONNECT_TIMEOUT));
    }

    #[tokio::test]
    async fn a_taken_conn_check_port_is_not_fatal_and_a_taken_web_port_is() {
        let held = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let taken = held.local_addr().expect("local addr").port();

        let listeners = bind_listeners("127.0.0.1", "0", taken, false)
            .await
            .expect("the web port binds and port 80 is only logged");
        assert_eq!(listeners.len(), 1);

        let err = bind_listeners("127.0.0.1", &taken.to_string(), 0, false)
            .await
            .expect_err("the web port is taken");
        assert!(matches!(err, ServeError::Bind(..)), "{err}");
    }
}
