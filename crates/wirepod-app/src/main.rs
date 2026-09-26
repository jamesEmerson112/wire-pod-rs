//! The `chipper` binary.
//!
//! Built with the `tray` feature on Windows, it is Go's `windows/cmd/main.go`:
//! a bare invocation, or the `-d` the `Run` key passes, starts the tray and the
//! server behind it. Otherwise it takes a subcommand. `serve` runs the server
//! from a console, and `sdk-trial` serves the SDK-app router beside the Go
//! server, as `RUNBOOK-SDK-TRIAL.md` describes.
//!
//! Without the tray, a bare invocation prints the usage and exits 2 rather than
//! starting anything, because both subcommands bind ports and dial a robot.

#![cfg_attr(all(windows, feature = "tray"), windows_subsystem = "windows")]

mod args;
mod sdk_trial;
mod serve;
#[cfg(windows)]
mod tray;

use std::process::ExitCode;

/// What a command line that did not parse exits with.
///
/// 2 rather than 1, which is the usual shell convention for usage as against a
/// run that started and failed. The trial's own failures exit 1.
const USAGE_EXIT: u8 = 2;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    // The tray's message loop owns this thread, so it starts before any runtime
    // and builds the server's on a thread of its own.
    #[cfg(all(windows, feature = "tray"))]
    if args::starts_tray(argv.first().map(String::as_str)) {
        tray::podapp::start_wire_pod(tray::win::funcs::Windows::new());
        return ExitCode::SUCCESS;
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("chipper: cannot start the runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(console(argv))
}

async fn console(argv: Vec<String>) -> ExitCode {
    let trial = match args::parse(argv) {
        Ok(args::Command::SdkTrial(trial)) => trial,
        Ok(args::Command::Serve(serve)) => {
            return match serve::run(serve).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("serve: {err}");
                    ExitCode::FAILURE
                }
            };
        }
        Err(err) => {
            eprintln!("chipper: {err}");
            eprintln!();
            eprint!("{}", args::USAGE);
            return ExitCode::from(USAGE_EXIT);
        }
    };

    match sdk_trial::run(trial).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("sdk-trial: {err}");
            ExitCode::FAILURE
        }
    }
}
