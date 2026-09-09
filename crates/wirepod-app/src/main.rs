//! The `chipper` binary.
//!
//! Go's entry point is `cmd/<engine>/main.go`, one per STT engine, each of
//! which reads the environment and `apiConfig.json` and calls
//! `initwirepod.StartFromProgramInit`. This binary is nowhere near that yet: P1
//! brings the configuration, the TLS listener and the tonic services, and P9 the
//! Windows tray shell.
//!
//! What it has today is one subcommand, `sdk-trial`, which serves the finished
//! SDK-app router against the real robot so that the parity diff in
//! `scripts/sdk-trial-diff.sh` has something to diff. `RUNBOOK-SDK-TRIAL.md` is
//! the procedure.
//!
//! A bare invocation prints the usage and exits 2 rather than starting
//! anything. The trial binds a port and dials a robot, so it is not something to
//! start by accident, and P1's real default is a different thing entirely.

mod args;
mod sdk_trial;

use std::process::ExitCode;

/// What a command line that did not parse exits with.
///
/// 2 rather than 1, which is the usual shell convention for usage as against a
/// run that started and failed. The trial's own failures exit 1.
const USAGE_EXIT: u8 = 2;

#[tokio::main]
async fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let trial = match args::parse(argv) {
        Ok(args::Command::SdkTrial(trial)) => trial,
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
