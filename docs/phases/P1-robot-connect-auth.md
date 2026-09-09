# P1: Robot connects & authenticates

## Goal

Get the robot to connect to the Rust server and authenticate against it, using the same on-disk state the Go server wrote. This is the first phase whose output the robot can talk to, and it is the phase that proves the riskiest parity claim in the project: that the Rust server can read, write, and extend the existing `%APPDATA%\wire-pod` state without the robot noticing a change of server. The phase is sized L in the master plan.

## Scope

- Configuration read and write, byte round-tripping against the live `apiConfig.json`.
- The logger and its in-memory ring, since the web UI reads its JSON tags.
- The token service, all three RPCs, with GUID and hash generation matching Go byte for byte.
- The jdocs service: `WriteDoc` and `ReadDocs` including the peer-IP association state machine, session-certificate writes, and the `botSdkInfo`, jdocs, and `sdk_config.ini` files.
- `StreamingConnectionCheck`.
- mDNS registration and browsing for `escapepod` on `_app-proto._tcp` port 8084.
- The restart supervisor and the `/api-chipper/*` routes it drives.
- Command-line flags and the explicit, logged path resolution for the asset root and the data root.

## Exit criteria

- A hash-parity unit test passes against the live Go-produced hash for ESN 00303f28. This is the critical test, because existing jdocs on disk already hold Go-produced hashes.
- Side-by-side running against the Go server on the alternate ports 18080, 1880, and 1443 behaves the same.
- A real cutover test on a copied data directory succeeds: the robot re-establishes in under sixty seconds, the `vic.RobotLifetimeStats` version advances past 313, and no jdoc regresses.
- `dns-sd -B _app-proto._tcp` shows `escapepod`.
- `grpcurl` reflection lists the three inbound services.
- A double restart through the API rebinds cleanly, which requires the supervisor to await the old task before rebinding.

## Dependencies

P0. The listener design, the proto crate, and the vendored escape-pod certificate all come from P0, and the S1 spike is what makes the rustls listener credible enough to build on.

## Status

Not started. Two pieces of P1 surface are being written early as part of the P4 slice and are deliberately incomplete: the `/ok` and `/ok:80` connection-check routes return the right bodies but their side effects, the peer-IP jdocs ping and the mDNS run, are deferred to this phase and recorded as a deviation.
