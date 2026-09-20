//! Go's `cmd/vosk/main.go`: load the state, start the chipper listeners and
//! serve the HTTP surface.
//!
//! Go's `main` hands `StartFromProgramInit` the speech engine's three functions.
//! The engine is M3 work, so the chipper service starts here with no voice
//! processor and answers the three streaming RPCs as unimplemented.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;
use wirepod_core::logger::{LogLayer, LogRing};
use wirepod_core::paths::{AssetDir, DataDir, sdk_ini_dir};
use wirepod_core::wallclock::{SystemWallClock, WallClock};
use wirepod_core::{
    AppState, Env, JdocsStore, Paths, SdkIniStore, SessionCertStore, WallLogClock, read_bot_info,
    read_config,
};
use wirepod_server::chipper::{Options, Server};
use wirepod_server::{CONN_CHECK_PORT, DEFAULT_WEB_PORT, startserver};
use wirepod_vector::TonicConnFactory;

use crate::args::ServeArgs;
use crate::sdk_trial::{DEFAULT_FILTER, FILTER_ENV};

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

/// The part of Go's `vars.Init` that reads the state files.
async fn load_state(
    args: &ServeArgs,
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
    let mut config = read_config(&Env::from_process(), &gate).await.config;
    if let Some(port) = args.tls_port {
        // A trial override, held in memory only.
        config.server.port = port.to_string();
    }
    let bot_info = read_bot_info(&data).await.unwrap_or_default();
    let jdocs = JdocsStore::load(&data).await.store;
    let session_certs = SessionCertStore::load(&data, &bot_info).await.store;

    Ok(
        AppState::builder(Arc::new(TonicConnFactory::insecure_tls()))
            .paths(Paths::new(data, assets))
            .config(config)
            .config_gate(gate)
            .bot_info(bot_info)
            .jdocs(jdocs)
            .session_certs(session_certs)
            .sdk_ini(sdk_ini)
            .logs(logs)
            .wall(wall)
            .build(),
    )
}

pub async fn run(args: ServeArgs) -> Result<(), ServeError> {
    // Go's `logger.Init`: the ring the web UI's log page reads, fed by every
    // `tracing` event alongside the console.
    let wall: Arc<dyn WallClock> = Arc::new(SystemWallClock::new());
    let logs = Arc::new(LogRing::new(Arc::new(WallLogClock::new(Arc::clone(&wall)))));
    install_logging(Arc::clone(&logs));
    let state = load_state(&args, logs, wall).await?;
    wirepod_server::jdocspinger::init_jdocs_pinger(&state);

    let cancel = CancellationToken::new();
    let stopper = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("serve: Ctrl-C, shutting down");
        }
        stopper.cancel();
    });

    // Go's `BeginServer` starts both of these beside the HTTP surface.
    // Beside the Go server its own watchdog is already sending Vector home.
    if !args.web_only {
        wirepod_server::sdkapp::batterywatchdog::start(Arc::clone(&state), cancel.clone());
    }
    {
        let (state, cancel) = (Arc::clone(&state), cancel.clone());
        tokio::spawn(async move { state.registry().run_conn_timer(cancel).await });
    }

    // Go serves one mux on the web port and on port 80.
    let router = wirepod_server::build_router(Arc::clone(&state));
    let mut plain = Vec::new();
    for port in [
        args.web_port.unwrap_or(DEFAULT_WEB_PORT),
        args.http_port.unwrap_or(CONN_CHECK_PORT),
    ] {
        let addr = format!("{}:{port}", args.bind);
        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|err| ServeError::Bind(addr.clone(), err))?;
        println!("serve: http on {addr}");
        plain.push(tokio::spawn(wirepod_server::serve_plain(
            listener,
            router.clone(),
            cancel.clone(),
        )));
    }

    // TODO(M3): wp.New(stt.Init, stt.STT, stt.Name) builds the voice processor
    // these options carry.
    if args.web_only {
        println!("serve: web only, the chipper listeners and mDNS stay off");
    } else {
        startserver::start_from_program_init(Arc::clone(&state), Server::new(Options::new())).await;
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
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(LogLayer::new(logs))
        .init();
}
