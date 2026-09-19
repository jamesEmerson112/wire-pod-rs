# P10: Cutover + soak

## Goal

Prove the Rust server can hold the robot for a full day, then take over from the Go server on the live installation without losing state and without a one-way door. The phase is sized S in the master plan, which is only true because everything that makes it safe was built into the earlier phases: the serde flatten and default fields on every persisted structure, the atomic write-and-rename persistence, and the round-trip tests.

## Scope

- A twenty-four-hour soak with an hourly script exercising voice, the language model, the camera, and the watchdog.
- Watching four things across the soak: behavior-control leaks, resident memory, mDNS, and jdoc monotonicity.
- A timestamped backup of the data directory before the first cutover.
- The cutover itself: stop the Go `chipper.exe`, start the Rust server against the same `%APPDATA%\wire-pod`, and let the robot follow `escapepod.local` over mDNS.
- Keeping the Go server available as a rollback for at least a month afterwards.

## Exit criteria

- The soak completes with no behavior-control leak, no memory growth trend, mDNS still answering, and jdoc versions only ever increasing.
- The cutover succeeds and the robot reconnects.
- Rollback is the same procedure in reverse, and it works. Both directions are schema-safe, which the serde flatten and round-trip tests are there to guarantee: a document written by the Rust server must survive being read and rewritten by the Go server, and the reverse.

## Dependencies

P0 through P9. This is the last phase and it depends on all of them.

## Status

Not started.
