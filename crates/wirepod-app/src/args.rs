//! The command line, parsed by hand.
//!
//! `clap` is not in `Cargo.lock` and the lock may not gain a package, so this
//! is a loop over the arguments. The grammar it accepts is small enough that
//! the loop is shorter than the derive would be, and the error messages are
//! written rather than generated, which matters because the one a person is
//! most likely to see names an environment variable rather than a flag.
//!
//! Go has no equivalent to parse against. `cmd/<engine>/main.go` takes no
//! arguments at all and reads everything from the environment and from
//! `apiConfig.json`, so nothing here is a port of anything. It is the trial
//! harness's own surface, and it gives way to the real configuration in P1.

use std::fmt;
use std::path::PathBuf;

/// The one subcommand, and what a bare invocation has to name.
pub const SUBCOMMAND: &str = "sdk-trial";

/// The subcommand that runs the server.
pub const SERVE: &str = "serve";

/// The default address to bind, which is loopback so the trial is never
/// reachable from the LAN.
pub const DEFAULT_BIND: &str = "127.0.0.1";

/// The default port.
///
/// Not 8080: the production Go server owns that port, and the whole point of
/// the trial is to run beside it and diff the two. The plan names 18080 for
/// exactly this.
pub const DEFAULT_PORT: u16 = 18080;

/// What to print when the arguments do not parse.
pub const USAGE: &str = "\
usage: chipper sdk-trial [--bot-info <path>] [--bind <addr>] [--port <u16>]
                         [--liveness-deadline-ms <n>]

  --bot-info <path>            the botSdkInfo.json to read
                               (default: the one under %APPDATA%/wire-pod/jdocs)
  --bind <addr>                the address to listen on (default: 127.0.0.1)
  --port <u16>                 the port to listen on (default: 18080)
  --liveness-deadline-ms <n>   bound the connect-time liveness call
                               (default: none, which is what the Go server does)

usage: chipper serve [--packaged | --data-dir <path>] [--asset-dir <path>]
                     [--sdk-ini-dir <path>] [--bind <addr>] [--web-port <u16>]
                     [--http-port <u16>] [--tls-port <u16>]

  --packaged             keep state under %APPDATA%/wire-pod, as the installed
                         Go server does (default: the working directory)
  --data-dir <path>      keep state under this directory instead
  --asset-dir <path>     where webroot, intent-data and epod live (default: .)
  --sdk-ini-dir <path>   where sdk_config.ini goes (default: ~/.anki_vector)
  --bind <addr>          the address the two HTTP listeners bind (default: 0.0.0.0)
  --web-port <u16>       the web port (default: 8080)
  --http-port <u16>      the conn-check port (default: 80)
  --tls-port <u16>       override the configured gRPC port, in memory only
";

/// The parsed `sdk-trial` arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrialArgs {
    /// The bot-info file to read, or `None` to resolve it from `APPDATA`.
    pub bot_info: Option<PathBuf>,
    /// The address to bind.
    pub bind: String,
    /// The port to bind.
    pub port: u16,
    /// The connect-time liveness deadline in milliseconds, or `None` for Go's
    /// undeadlined `BatteryState` (`robot.go:365`).
    pub liveness_deadline_ms: Option<u64>,
}

impl Default for TrialArgs {
    fn default() -> Self {
        Self {
            bot_info: None,
            bind: DEFAULT_BIND.to_owned(),
            port: DEFAULT_PORT,
            liveness_deadline_ms: None,
        }
    }
}

/// What the command line asked for.
/// What `serve` runs with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServeArgs {
    pub packaged: bool,
    pub data_dir: Option<PathBuf>,
    pub asset_dir: Option<PathBuf>,
    pub sdk_ini_dir: Option<PathBuf>,
    pub bind: String,
    pub web_port: Option<u16>,
    pub http_port: Option<u16>,
    pub tls_port: Option<u16>,
}

impl Default for ServeArgs {
    fn default() -> Self {
        Self {
            packaged: false,
            data_dir: None,
            asset_dir: None,
            sdk_ini_dir: None,
            // Go listens on every interface.
            bind: "0.0.0.0".to_owned(),
            web_port: None,
            http_port: None,
            tls_port: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Serve(ServeArgs),
    /// Serve the SDK-app router against the real robot.
    SdkTrial(TrialArgs),
}

/// Why the command line did not parse.
///
/// Each variant carries what its message needs in order to name the offending
/// argument, because a parse failure with no argument in it is the one thing
/// worse than no message at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// No subcommand at all, which is the bare `chipper`.
    NoSubcommand,
    /// A first argument that is not [`SUBCOMMAND`].
    UnknownSubcommand(String),
    /// A flag the subcommand does not take.
    UnknownFlag(String),
    /// A flag whose value is missing because it was last on the line.
    MissingValue(&'static str),
    /// A numeric flag whose value is not a number, or does not fit.
    BadNumber {
        /// The flag, with its dashes.
        flag: &'static str,
        /// What was written after it.
        value: String,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSubcommand => write!(f, "no subcommand; the only one is {SUBCOMMAND}"),
            Self::UnknownSubcommand(name) => {
                write!(f, "unknown subcommand {name}; the only one is {SUBCOMMAND}")
            }
            Self::UnknownFlag(flag) => write!(f, "unknown flag {flag}"),
            Self::MissingValue(flag) => write!(f, "{flag} needs a value"),
            Self::BadNumber { flag, value } => write!(f, "{flag} needs a number, not {value}"),
        }
    }
}

/// Parses the arguments after the program name.
///
/// Flags may appear in any order, and a repeated flag takes its last value.
/// That is the ordinary shell expectation and costs nothing to allow. There are
/// no short forms and no `=` form, because nothing types this command line
/// often enough to want them.
pub fn parse<I>(args: I) -> Result<Command, ParseError>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    match args.next() {
        None => return Err(ParseError::NoSubcommand),
        Some(name) if name == SUBCOMMAND => {}
        Some(name) if name == SERVE => return parse_serve(args),
        Some(name) => return Err(ParseError::UnknownSubcommand(name)),
    }

    let mut trial = TrialArgs::default();
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--bot-info" => {
                let value = args.next().ok_or(ParseError::MissingValue("--bot-info"))?;
                trial.bot_info = Some(PathBuf::from(value));
            }
            "--bind" => {
                trial.bind = args.next().ok_or(ParseError::MissingValue("--bind"))?;
            }
            "--port" => {
                let value = args.next().ok_or(ParseError::MissingValue("--port"))?;
                trial.port = value.parse().map_err(|_| ParseError::BadNumber {
                    flag: "--port",
                    value,
                })?;
            }
            "--liveness-deadline-ms" => {
                let value = args
                    .next()
                    .ok_or(ParseError::MissingValue("--liveness-deadline-ms"))?;
                let millis = value.parse().map_err(|_| ParseError::BadNumber {
                    flag: "--liveness-deadline-ms",
                    value,
                })?;
                trial.liveness_deadline_ms = Some(millis);
            }
            _ => return Err(ParseError::UnknownFlag(flag)),
        }
    }
    Ok(Command::SdkTrial(trial))
}

fn parse_serve(mut args: impl Iterator<Item = String>) -> Result<Command, ParseError> {
    fn path(
        args: &mut impl Iterator<Item = String>,
        flag: &'static str,
    ) -> Result<Option<PathBuf>, ParseError> {
        Ok(Some(PathBuf::from(
            args.next().ok_or(ParseError::MissingValue(flag))?,
        )))
    }
    fn port(
        args: &mut impl Iterator<Item = String>,
        flag: &'static str,
    ) -> Result<Option<u16>, ParseError> {
        let value = args.next().ok_or(ParseError::MissingValue(flag))?;
        match value.parse() {
            Ok(port) => Ok(Some(port)),
            Err(_) => Err(ParseError::BadNumber { flag, value }),
        }
    }

    let mut serve = ServeArgs::default();
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--packaged" => serve.packaged = true,
            "--data-dir" => serve.data_dir = path(&mut args, "--data-dir")?,
            "--asset-dir" => serve.asset_dir = path(&mut args, "--asset-dir")?,
            "--sdk-ini-dir" => serve.sdk_ini_dir = path(&mut args, "--sdk-ini-dir")?,
            "--bind" => serve.bind = args.next().ok_or(ParseError::MissingValue("--bind"))?,
            "--web-port" => serve.web_port = port(&mut args, "--web-port")?,
            "--http-port" => serve.http_port = port(&mut args, "--http-port")?,
            "--tls-port" => serve.tls_port = port(&mut args, "--tls-port")?,
            _ => return Err(ParseError::UnknownFlag(flag)),
        }
    }
    Ok(Command::Serve(serve))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four flags, in one place, so a test that loops over them and the
    /// usage text cannot drift apart from the parser.
    const FLAGS: [&str; 4] = ["--bot-info", "--bind", "--port", "--liveness-deadline-ms"];

    /// Builds the argument vector the way `std::env::args().skip(1)` hands it
    /// over.
    fn argv(line: &[&str]) -> Vec<String> {
        line.iter().map(|arg| (*arg).to_owned()).collect()
    }

    fn trial(line: &[&str]) -> TrialArgs {
        match parse(argv(line)).expect("the line parses") {
            Command::SdkTrial(trial) => trial,
            Command::Serve(_) => panic!("the line is a serve line"),
        }
    }

    #[test]
    fn the_bare_subcommand_takes_every_default() {
        let parsed = trial(&["sdk-trial"]);
        assert_eq!(parsed, TrialArgs::default());
        assert_eq!(parsed.bind, "127.0.0.1");
        assert_eq!(parsed.port, 18080);
        assert_eq!(parsed.bot_info, None, "the path is resolved from APPDATA");
        assert_eq!(
            parsed.liveness_deadline_ms, None,
            "no deadline is what the Go server does"
        );
    }

    #[test]
    fn every_flag_is_read() {
        let parsed = trial(&[
            "sdk-trial",
            "--bot-info",
            "C:/tmp/botSdkInfo.json",
            "--bind",
            "0.0.0.0",
            "--port",
            "1880",
            "--liveness-deadline-ms",
            "2500",
        ]);
        assert_eq!(
            parsed.bot_info,
            Some(PathBuf::from("C:/tmp/botSdkInfo.json"))
        );
        assert_eq!(parsed.bind, "0.0.0.0");
        assert_eq!(parsed.port, 1880);
        assert_eq!(parsed.liveness_deadline_ms, Some(2500));
    }

    #[test]
    fn flags_may_come_in_any_order_and_a_repeat_takes_the_last_value() {
        let parsed = trial(&["sdk-trial", "--port", "1", "--bind", "::1", "--port", "2"]);
        assert_eq!(parsed.port, 2);
        assert_eq!(parsed.bind, "::1");
    }

    #[test]
    fn a_path_with_spaces_survives_as_one_argument() {
        // The default lives under `%APPDATA%`, and a user name with a space in
        // it produces exactly this.
        let parsed = trial(&["sdk-trial", "--bot-info", "C:/Program Files/x/bot.json"]);
        assert_eq!(
            parsed.bot_info,
            Some(PathBuf::from("C:/Program Files/x/bot.json"))
        );
    }

    #[test]
    fn a_bare_invocation_is_an_error_rather_than_a_default_run() {
        assert_eq!(parse(argv(&[])), Err(ParseError::NoSubcommand));
    }

    #[test]
    fn an_unknown_subcommand_names_itself_and_the_one_that_exists() {
        let err = parse(argv(&["launch"])).expect_err("launch is not a subcommand");
        assert_eq!(err, ParseError::UnknownSubcommand("launch".to_owned()));
        let message = err.to_string();
        assert!(message.contains("launch"), "{message}");
        assert!(message.contains(SUBCOMMAND), "{message}");
    }

    #[test]
    fn serve_defaults_to_the_working_directory_and_takes_its_overrides() {
        assert_eq!(
            parse(argv(&["serve"])),
            Ok(Command::Serve(ServeArgs::default()))
        );
        let Ok(Command::Serve(serve)) = parse(argv(&[
            "serve",
            "--data-dir",
            "C:/tmp/pod",
            "--tls-port",
            "1443",
            "--web-port",
            "18080",
        ])) else {
            panic!("the serve line parses");
        };
        assert_eq!(serve.data_dir, Some(PathBuf::from("C:/tmp/pod")));
        assert_eq!(serve.tls_port, Some(1443));
        assert_eq!(serve.web_port, Some(18080));
        assert!(!serve.packaged);
        assert_eq!(
            parse(argv(&["serve", "--tls"])),
            Err(ParseError::UnknownFlag("--tls".to_owned()))
        );
    }

    #[test]
    fn an_unknown_flag_is_refused_rather_than_ignored() {
        assert_eq!(
            parse(argv(&["sdk-trial", "--tls"])),
            Err(ParseError::UnknownFlag("--tls".to_owned()))
        );
    }

    #[test]
    fn a_flag_with_no_value_names_the_flag() {
        for flag in FLAGS {
            let err = parse(argv(&["sdk-trial", flag])).expect_err("the value is missing");
            assert!(
                err.to_string().contains(flag),
                "the message for {flag} must name it: {err}"
            );
        }
    }

    #[test]
    fn a_non_numeric_or_out_of_range_number_is_refused() {
        assert_eq!(
            parse(argv(&["sdk-trial", "--port", "eighty"])),
            Err(ParseError::BadNumber {
                flag: "--port",
                value: "eighty".to_owned(),
            })
        );
        // 65536 parses as a number and does not fit a `u16`, which is the case
        // a hand-written parser is most likely to let through.
        assert_eq!(
            parse(argv(&["sdk-trial", "--port", "65536"])),
            Err(ParseError::BadNumber {
                flag: "--port",
                value: "65536".to_owned(),
            })
        );
        assert!(parse(argv(&["sdk-trial", "--port", "65535"])).is_ok());
        assert_eq!(
            parse(argv(&["sdk-trial", "--liveness-deadline-ms", "-1"])),
            Err(ParseError::BadNumber {
                flag: "--liveness-deadline-ms",
                value: "-1".to_owned(),
            })
        );
    }

    #[test]
    fn the_usage_names_every_flag_the_parser_takes() {
        for flag in FLAGS {
            assert!(USAGE.contains(flag), "the usage must name {flag}");
        }
        assert!(USAGE.contains(SUBCOMMAND));
        assert!(USAGE.contains(DEFAULT_BIND));
        assert!(USAGE.contains(&DEFAULT_PORT.to_string()));
    }
}
