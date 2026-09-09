//! `chipper sdk-trial`: the SDK-app router, served against the real robot.
//!
//! This is the trial harness the runbook drives, not the server. It wires the
//! three finished crates together in the order the real binary eventually will,
//! binds one plain listener on a port the production Go server does not own,
//! and stops on Ctrl-C. What it deliberately leaves out is everything P1 owns:
//! TLS, the tonic services, the second listener on port 80, mDNS, the jdocs
//! pinger, static files and `/cam-stream`.
//!
//! The point of it is that `wirepod-server`'s whole suite talks to a
//! `FakeConnFactory` over `tower`'s `oneshot`. Nothing before this has put a
//! real socket in front of the router or a real robot behind it, so three of
//! the questions `deviations.md` lists as not yet verifiable can only be
//! answered by running this beside the Go server and diffing the two.
//!
//! Nothing here ever prints a robot GUID. The startup line counts the robots in
//! the file and names the file; the log lines below it come from the crates,
//! which log serials and addresses and never the bearer token.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;
use wirepod_core::{AppState, BotInfo};
use wirepod_vector::TonicConnFactory;

use crate::args::TrialArgs;

/// The environment variable the log filter is read from.
pub const FILTER_ENV: &str = "RUST_LOG";

/// The filter used when [`FILTER_ENV`] is unset.
///
/// Debug on the three crates the trial exercises and info on everything else.
/// The three that matter are `wirepod_core` for the registry and the ownership
/// state machines, `wirepod_vector` for the dial and the RPCs, and
/// `wirepod_server` for the handlers. `sdkapp` is the target `stim::begin` logs
/// its stream errors to and it is covered by the info default.
pub const DEFAULT_FILTER: &str =
    "info,wirepod_core=debug,wirepod_vector=debug,wirepod_server=debug";

/// The environment variable the default bot-info path is built from.
pub const APPDATA_ENV: &str = "APPDATA";

/// The path under `%APPDATA%` that Go resolves `vars.BotInfoPath` to for a
/// packaged build (`vars.go:173`).
const BOT_INFO_SUFFIX: [&str; 3] = ["wire-pod", "jdocs", "botSdkInfo.json"];

/// Why the trial could not start, or did not finish cleanly.
#[derive(Debug)]
pub enum TrialError {
    /// `APPDATA` is unset and no `--bot-info` was given.
    NoAppData,
    /// The bot-info file is not there.
    BotInfoMissing(PathBuf),
    /// The bot-info file is there and could not be read.
    BotInfoUnreadable(PathBuf, std::io::Error),
    /// The bot-info file is there and is not the JSON this expects.
    BotInfoMalformed(PathBuf, serde_json::Error),
    /// The listener could not be bound, which on this machine almost always
    /// means the port is taken.
    Bind(String, std::io::Error),
    /// The accept loop failed.
    Serve(std::io::Error),
}

impl fmt::Display for TrialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAppData => write!(
                f,
                "{APPDATA_ENV} is not set, so the default bot-info path cannot be built; \
                 pass --bot-info <path>"
            ),
            Self::BotInfoMissing(path) => write!(
                f,
                "no bot-info file at {}; the Go server writes it once a robot has \
                 authenticated, so start the Go server and let the robot connect first",
                path.display()
            ),
            Self::BotInfoUnreadable(path, err) => {
                write!(f, "cannot read {}: {err}", path.display())
            }
            Self::BotInfoMalformed(path, err) => {
                write!(f, "cannot parse {}: {err}", path.display())
            }
            Self::Bind(addr, err) => write!(
                f,
                "cannot bind {addr}: {err}; pick another --port, and never 80, 443, \
                 8080 or 8084, which the Go server owns"
            ),
            Self::Serve(err) => write!(f, "the listener failed: {err}"),
        }
    }
}

impl std::error::Error for TrialError {}

/// The bot-info file to read.
///
/// An explicit `--bot-info` wins outright, including over a missing `APPDATA`,
/// so the trial can be pointed at a copy. Otherwise the path is Go's packaged
/// one: `<user config dir>/wire-pod/jdocs/botSdkInfo.json` (`vars.go:173`),
/// which on Windows is what `%APPDATA%` names.
///
/// The environment value is passed in rather than read here so that the rule is
/// testable without touching the process environment, which no test may do
/// while other tests run beside it in the same process.
pub fn bot_info_path(
    explicit: Option<PathBuf>,
    appdata: Option<OsString>,
) -> Result<PathBuf, TrialError> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    let mut path = PathBuf::from(appdata.ok_or(TrialError::NoAppData)?);
    path.extend(BOT_INFO_SUFFIX);
    Ok(path)
}

/// Reads and parses the bot-info file.
///
/// The missing case is separated from every other read failure because it is
/// the one a person hits, and because its remedy is a sentence about the Go
/// server rather than about permissions.
pub fn load_bot_info(path: &Path) -> Result<BotInfo, TrialError> {
    if !path.exists() {
        return Err(TrialError::BotInfoMissing(path.to_path_buf()));
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|err| TrialError::BotInfoUnreadable(path.to_path_buf(), err))?;
    serde_json::from_str(&raw).map_err(|err| TrialError::BotInfoMalformed(path.to_path_buf(), err))
}

/// The one line printed before the log starts.
///
/// It carries the count rather than the list, which is the whole of what keeps
/// a GUID out of a terminal that may end up in a report. `robots` is the number
/// of entries in the file, not the number of reachable robots: nothing has been
/// dialled at this point.
pub fn startup_line(bind: &str, port: u16, robots: usize, path: &Path) -> String {
    format!(
        "sdk-trial: serving http://{bind}:{port} for {robots} robot(s) from {}",
        path.display()
    )
}

/// Runs the trial until Ctrl-C.
pub async fn run(args: TrialArgs) -> Result<(), TrialError> {
    install_logging();

    let path = bot_info_path(args.bot_info.clone(), std::env::var_os(APPDATA_ENV))?;
    let bot_info = load_bot_info(&path)?;
    let robots = bot_info.robots.len();

    // The one constructor the TLS dialer commit replaces. Everything else about
    // this wiring is what the real binary will do.
    let state = AppState::builder(Arc::new(TonicConnFactory::insecure_tls()))
        .bot_info(bot_info)
        .liveness_deadline(args.liveness_deadline_ms.map(Duration::from_millis))
        .build();
    let router = wirepod_server::build_router(state);

    let addr = format!("{}:{}", args.bind, args.port);
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|err| TrialError::Bind(addr.clone(), err))?;

    println!("{}", startup_line(&args.bind, args.port, robots, &path));
    println!("sdk-trial: Ctrl-C to stop");

    let cancel = CancellationToken::new();
    let stopper = cancel.clone();
    tokio::spawn(async move {
        // A failed signal registration is not a reason to refuse to serve: the
        // trial is still useful, it just has to be killed rather than
        // interrupted, and the operator is told so.
        match tokio::signal::ctrl_c().await {
            Ok(()) => tracing::info!("sdk-trial: Ctrl-C, shutting down"),
            Err(err) => {
                tracing::warn!("sdk-trial: cannot listen for Ctrl-C ({err}); kill the process");
                return;
            }
        }
        stopper.cancel();
    });

    wirepod_server::serve_plain(listener, router, cancel)
        .await
        .map_err(TrialError::Serve)
}

/// Installs the log sink.
///
/// Separate from [`run`] because it can only happen once per process, which is
/// what makes it the one thing in this module with no test.
fn install_logging() {
    let filter = match std::env::var(FILTER_ENV) {
        Ok(value) => EnvFilter::new(value),
        Err(_) => EnvFilter::new(DEFAULT_FILTER),
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_path_wins_even_with_no_appdata() {
        let path = bot_info_path(Some(PathBuf::from("C:/tmp/bot.json")), None)
            .expect("an explicit path needs no environment");
        assert_eq!(path, PathBuf::from("C:/tmp/bot.json"));
    }

    #[test]
    fn the_default_path_is_gos_packaged_one_under_appdata() {
        let path = bot_info_path(None, Some(OsString::from("C:/Users/x/AppData/Roaming")))
            .expect("APPDATA is set");
        let tail: Vec<_> = path
            .components()
            .rev()
            .take(3)
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect();
        assert_eq!(tail, vec!["botSdkInfo.json", "jdocs", "wire-pod"]);
        assert!(path.starts_with("C:/Users/x/AppData/Roaming"));
    }

    #[test]
    fn a_missing_appdata_says_which_flag_replaces_it() {
        let err = bot_info_path(None, None).expect_err("APPDATA is unset");
        let message = err.to_string();
        assert!(message.contains(APPDATA_ENV), "{message}");
        assert!(message.contains("--bot-info"), "{message}");
    }

    #[test]
    fn a_missing_file_names_the_path_and_the_remedy() {
        let path = PathBuf::from("C:/nowhere/botSdkInfo.json");
        let err = load_bot_info(&path).expect_err("the file is not there");
        assert!(matches!(err, TrialError::BotInfoMissing(_)));
        let message = err.to_string();
        assert!(message.contains("nowhere"), "{message}");
        assert!(message.contains("Go server"), "{message}");
    }

    #[test]
    fn the_startup_line_counts_the_robots_and_never_names_one() {
        let line = startup_line(
            "127.0.0.1",
            18080,
            1,
            Path::new("C:/Users/x/AppData/Roaming/wire-pod/jdocs/botSdkInfo.json"),
        );
        assert!(line.starts_with("sdk-trial: serving http://127.0.0.1:18080 for 1 robot(s) from "));
        assert!(line.ends_with("botSdkInfo.json"));
        // The only fields the file has that must never be printed.
        assert!(!line.contains("guid"), "{line}");
        assert!(!line.contains("esn"), "{line}");
    }

    #[test]
    fn the_default_filter_names_the_three_crates_the_trial_exercises() {
        for crate_name in ["wirepod_core", "wirepod_vector", "wirepod_server"] {
            assert!(
                DEFAULT_FILTER.contains(&format!("{crate_name}=debug")),
                "the default filter must turn {crate_name} up to debug"
            );
        }
        assert!(DEFAULT_FILTER.starts_with("info,"));
    }
}
