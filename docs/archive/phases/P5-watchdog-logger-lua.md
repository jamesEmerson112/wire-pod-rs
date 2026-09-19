# P5: Watchdog, logger polish, Lua

## Goal

Port the fork's battery watchdog, finish the logger, and bring the Lua scripting host over so that existing user scripts keep running. The phase is sized M in the master plan. The watchdog is the part that matters most, because it acts on the robot on its own and a mistake in it is visible as the robot docking when it should not, or failing to dock when it should.

## Scope

- The battery watchdog: a thirty-second poll, a volts-to-percent curve mirroring `webroot/js/battery.js`, three-reading low hysteresis, `OVERRIDE_BEHAVIORS` with a silent `DriveOnCharger`, a ten-minute cooldown with three attempts and a thirty-minute backoff, and attribution of the cause when the robot docks.
- The `gohome_percent` setting, which is a fork-only field with pointer semantics in Go: absent means the default of 25, and zero means disabled. In Rust it is an `Option<i32>` with `skip_serializing_if` so a configuration written by either server round-trips unchanged.
- Logger polish, so the ring, the JSON tags the web UI reads, ANSI stripping, the log-file sink, the debug gate, and the legacy shims for `/api/get_logs` are all complete.
- The Lua host and the `/api-lua/run_script` route. Go used gopher-lua, which is Lua 5.1, so the Rust host is mlua with the `lua51` and vendored features. Luau was rejected because it would break existing user scripts. Each execution gets a fresh `Lua` in `spawn_blocking` behind a semaphore, which is safer than gopher-lua's shared state ever was, and the shipped standard-library subset covers json, time, strings, base64, filepath, and http with loud stubs for anything unsupported.

## Exit criteria

- Unit tests over the battery curve and the hysteresis.
- A live or mocked go-home fires once and then cools down as configured.
- Existing user Lua scripts either run or fail loudly, never silently.

## Dependencies

P0 and P1 for the server, configuration, and logger foundations, and P4 for the outbound robot client, since the watchdog reads battery state and issues `DriveOnCharger` through it.

## Status

Not started.
