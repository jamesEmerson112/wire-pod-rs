# P4: SDK app + web UI + camera

## Goal

Port the outbound half of the server and the surface the browser talks to. The robot is a gRPC server as well as a client, and this phase builds the client that dials it, together with the HTTP API and the static web UI that drive it. It is sized L in the master plan. Because the web UI is vendored byte-identically from the Go repository, its JavaScript is a contract: the response bodies this phase produces have to match Go closely enough that unmodified pages keep working.

## Scope

- The full twenty-five-RPC vector client: bidirectional behavior control, the event stream, the camera feed, external audio stream playback at 8 kHz in 1024-byte chunks every 60 ms, `PullJdocs`, faces, and photos.
- The `update_settings` REST call and the port 8889 console-variable wake-word trigger.
- All forty-five `/api-sdk/*` routes and all twenty-two `/api/*` routes. The master plan says twenty-five for the second figure. A recount of the `apiHandler` switch at `webserver.go:32-74` found twenty-two route arms; the other three are the log-level cases inside `handleGetLogsJSON`, which select a log level and are not routes. See [P4-sdk-app/sdkapp-routes.md](P4-sdk-app/sdkapp-routes.md).
- The `/session-certs/*` routes.
- `/cam-stream`, serving MJPEG with JPEG re-encoded at quality 50.
- Static serving of the vendored `webroot/`.

## Exit criteria

- `xtask parity-http` runs a route-by-route JSON diff against the Go server, with volatile fields normalized. This is the primary gate.
- The real web UI and the real robot are driven end to end.

## Dependencies

P0 through P3 in the plan's ordering, because the SDK client is what P3's conversation loop speaks through and because the routes need P1's configuration, logging, and jdocs stores. An early slice is being implemented ahead of that ordering; see below.

## The parity specification

The detailed specification for this phase lives in [P4-sdk-app/](P4-sdk-app/), which holds the route table, the ownership state machines, the camera stream contract, the dashboard client contract, the mapping from the Go tests, the recorded deviations, and the design of the early slice. It is separated out because it is far too large for one phase document, and because it was written from six read-only surveys of the Go server rather than from the plan alone.

## The early slice

An early slice of this phase is being implemented ahead of P1 through P3. It covers the pieces added by the September Vector Brain dashboard work in the Go fork: the `net_probe` route, camera and event stream ownership, stim state, the per-robot camera meter, and the bot-status empty-array fix. Those five behaviors arrived with the fork's first Go tests, eight of them, which pin the concurrency semantics precisely enough to port as Rust tests. The slice cannot be tested against a robot yet, so everything in it is unit-testable without one, using fakes behind traits and a loopback tonic server. The decisions that shape it are these.

- **D1, axum 0.7 rather than 0.8.** `tonic 0.12.3` depends on axum 0.7 and its `Routes::into_axum_router()` returns an axum 0.7 `Router`, and the lock file holds only 0.7.9, so the plan's choice of axum 0.8 is amended. The `/ok:80` route with its literal colon keeps the router-fallback trick the S1 spike proved.
- **D2, the seam traits live in `wirepod-core` and core does not depend on `wirepod-proto`.** Core owns `RobotConn`, `EventReceiver`, `FrameStream`, `CameraControl`, `FrameSink`, and `RobotConnFactory` in domain types, along with the ownership state machines, the meters, the registry, and `AppState`. The crate `wirepod-vector` depends on core and on proto and holds the tonic client, and `wirepod-server` depends on both.
- **D3, the registry is a plain struct rather than an mpsc actor.** It is a read-write-locked map of entries plus a per-ESN connect lock held across the dial, so the same serial dials only once while one robot's dial never blocks another's. This amends the plan's actor choice and also improves on Go, whose `inhibitCreation` flag stalls every robot at once.
- **D4, synchronous state sits behind `std::sync::Mutex` and is never held across an await.** Camera ownership, event ownership, and stim are per-entry fields; the camera meter lives in a separate map that is never pruned, so totals survive eviction and reconnect; the camera operation lock is a tokio mutex held across the settle and the enable, and the lock order is operation lock first, ownership second. Go's `gen` is a reserved word in edition 2024, so the type is named `Generation`.
- **D5, timings are injectable and no test pauses the clock.** The 500 ms settle, the 5 s probe and enable deadlines, the 3 s disconnect settle, and the 300 s idle limit are all fields of a `Timings` struct whose defaults one test asserts. Tests use zero settles and millisecond deadlines with real-clock ceilings that only fire on a regression.
- **D6, the slice is scoped in three tiers.** Tier A is pure logic tested with plain `#[test]`. Tier B is asynchronous and HTTP work driven through `tower::ServiceExt::oneshot` plus one loopback tonic fake. Tier C is excluded until P1 and P4 proper and covers real listeners, TLS, mDNS, static serving, the logger ring, the jdocs store, the MJPEG camera route, and the background tasks.
- **D7, no new packages enter `Cargo.lock`.** A workspace dependency table is introduced first in a commit that leaves the lock byte-identical, and every later dependency commit is gated on the sorted set of name, version, and checksum triples being unchanged. The `rttMs` number reaches the wire through `serde_json`'s `raw_value` feature, which adds no package.
- **D8, `async_trait` provides the dyn-compatible seams.** It is already in the lock file. The fallback, if it ever becomes a problem, is to hand-desugar the traits into boxed futures.
- **D9, documentation lives in the repository.** The phase folder and the plan copy under `docs/` replace home-directory notes as the place project knowledge is kept.
- **D10, a `cargo xtask` alias is added.** `[alias] xtask = "run -p xtask --"` goes into `.cargo/config.toml` so the documented invocation actually works, and the two places that print the non-working form are corrected.
- **D11, vendored text assets are stored with CRLF line endings.** Those are the bytes of the Windows Go checkout and also what production serves, so git, the working tree, `MANIFEST.sha256`, the Go checkout, and the installed copy all agree. `sync-assets --check` is therefore meaningful only against a Windows checkout of the Go repository until P9 chooses a cross-platform normalization.

## Status

Spec written; early slice landed on master through C12 (core, vector, the ten `/api-sdk` slice routes, `/api/get_bot_status`, `/ok`; 207 tests). P4 proper not started.
