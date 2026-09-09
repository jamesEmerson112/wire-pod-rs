# Early SDK-app slice: design

Read this before touching code. It is the approved design for the early Rust implementation
slice of the Vector Brain SDK-app work, which lands ahead of roadmap phases P1 through P3 and
must be unit-testable with no robot, no privileged port and no disk state.

The companion documents are `sdkapp-routes.md` for the route table, `sdkapp-state.md` for the Go
state being ported, `camstream.md` for the camera pipeline, `dashboard-client.md` for what the
vendored JavaScript demands, `go-tests.md` for the eight Go tests and their Rust counterparts,
and `deviations.md` for every deliberate difference from Go.

## Decisions

### D1. axum 0.7 with tonic 0.12

`tonic 0.12.3` depends on axum 0.7, and `Routes::into_axum_router()` returns an axum 0.7
`Router`. `Cargo.lock` holds exactly one axum, 0.7.9, and one `matchit`, 0.7.3. This is an
amendment to the approved plan's "axum 0.8", which cannot be combined with tonic 0.12 through
`into_axum_router` without either upgrading tonic or hand-wrapping the gRPC service.

The practical consequence is that `matchit` 0.7 treats a colon as the path-parameter sigil, so
`/ok:80` is never registered as a route. It is matched as a literal path inside the router
fallback, which is the trick spike S1 already proved. When tonic is eventually upgraded past
0.12, `matchit` moves to 0.8, a literal colon becomes legal, and the fallback trick becomes
unnecessary; that is a router rewrite plus a change to the one test, so keep the `/ok:80`
handling in a single function and let the test assert behaviour rather than mechanism.

### D2. Crate placement

`wirepod-core` owns the robot seam, expressed in domain types so that core has no dependency on
`wirepod-proto` and no dependency on tonic. It holds the traits `RobotConn`, `EventReceiver`,
`FrameStream`, `CameraControl`, `FrameSink` and `RobotConnFactory`; the per-robot ownership state
machines; the never-pruned camera meters; the registry; the event loop; the frame pump; the
bot-info and pinger stores; and `AppState` with a builder.

`wirepod-vector` depends on core and on proto. It holds the tonic client wrapper that attaches
the bearer authorisation metadata per call, the `Streaming` adapters, the conversion from
`tonic::Status` to `ConnError`, a connection factory with an injectable endpoint builder, and a
`test-util` fake robot server.

`wirepod-server` depends on both and assembles nothing but the router and the handlers.

The dependency direction is therefore `proto <- vector`, `core <- vector`, `core <- server` and
`vector <- server`. Core sits at the bottom and depends on neither of the other two.

This keeps `AppState` in core, as the approved plan requires, and it mirrors Go's own narrow
`eventReceiver` seam. The reason for putting the seam in core rather than in vector is a cycle
that would otherwise appear at P3: the registry must read the bot-info store to resolve a serial
into a target and a GUID, so if the registry lived in vector then vector would depend on core,
core could not hold the registry, and `AppState` would have to be assembled in `wirepod-server`.
At P3, `wirepod-ttr`, `wirepod-llm` and `wirepod-intent` all need `AppState`, which would make
them depend on `wirepod-server` while `wirepod-server` already depends on them. Expressing
`RobotConn` in small domain types removes the only objection to core ownership.

### D3. The registry is a struct, not an mpsc actor

`RwLock<HashMap<Esn, Arc<RobotEntry>>>` for the directory, plus per-ESN connect locks held as
`Mutex<HashMap<Esn, Arc<tokio::sync::Mutex<()>>>>` with the inner lock held across the dial. The
same serial therefore dials once, and robot A's dial never blocks robot B. This is an amendment
to the approved plan, which named an actor.

Two reasons. Go commit `255a737` was explicitly designed so that one unresponsive robot cannot
stall another robot's camera, and a single-task actor would serialise every camera operation,
including the 500 millisecond settle and the enable RPC, across all robots. Second, an actor
forces all eight ported tests into a tokio runtime, while the per-entry design lets five of them
run with no runtime at all. Go's global `inhibitCreation` flag, which stalls everyone, is
replaced by the per-ESN lock and recorded as an improvement in `deviations.md`.

The connect-time `BatteryState` liveness deadline is `Option<Duration>` defaulting to `None`,
which reproduces Go exactly. The dead connect-time `EventStream` that Go opens and never reads is
not opened, and the loopback test asserts that a connect issues only `BatteryState`.

The registry API is `get_or_connect`, `peek`, `touch`, `disconnect`, `evict_idle` and
`read_meter`.

### D4. Synchronous state on `std::sync::Mutex`, never held across an await

Camera ownership, event ownership and the stim value are per-entry fields guarded by
`std::sync::Mutex`, with no `.await` inside any critical section. That is what keeps them pure,
directly callable and testable without a runtime.

The camera meter is the exception to "per entry". It lives in a separate, never-pruned
`CamMeters` keyed by `Esn`, so that totals survive eviction and a later reconnect exactly as
Go's `camMeters` survives `removeRobot`. A test pins it. If the meter were folded into the
evictable entry, which is the natural-looking simplification, every reconnect would look like a
server restart to the dashboard's differencing logic and would cost a throughput sample.

The camera operation lock is a `tokio::sync::Mutex`, because it is genuinely held across the
settle and the enable RPC, exactly as Go holds a `sync.Mutex` across the same work.

Camera handlers hold a `#[must_use] CamGuard` with an explicit `async fn finish(self)`. Rust's
`Drop` cannot be async and spawning from `Drop` is a known hazard, so the `Drop` impl only logs a
leak. The combination of the compiler warning and the loud runtime log is the closest safe
analogue to Go's `defer finishCamStream`.

### D5. Timing is injectable, and no test pauses the clock

The settle of 500 milliseconds, the probe deadline of 5 seconds, the enable deadline of 5
seconds, the disconnect settle of 3 seconds and the idle limit of 300 seconds are all fields of a
`Timings` struct with a `Default` impl that a test asserts.

Tests use zero settles, millisecond deadlines, and real-clock `tokio::time::timeout` ceilings of
two seconds that fire only on a regression. There is no `start_paused` anywhere in the slice.
Auto-advance fires only when the runtime is idle with a pending timer, and the camera handoff
case has a task blocked on a mutex rather than on a timer, which is the classic case where
auto-advance is subtle; a paused-clock hang burns the CI job's default timeout with no
diagnostic. `start_paused` remains the right tool for P4's 300 second sweeper and the pinger
ticker, which have no injection seam.

Go's `busyWait` and `waitFor` helpers are not ported. See `go-tests.md` for why each is unsound
to translate.

### D6. Slice scope, in three tiers

The tiers are set out in full in the "Tier A, B and C scope" section below.

### D7. No new packages in `Cargo.lock`

`[workspace.dependencies]` is introduced first, in its own commit, which leaves the lock
byte-identical and proves the table is correct.

Every dependency the slice adds is already resolved in the lock at the exact version named,
because `wirepod-proto` already depends on tonic with default features, which pulls axum 0.7,
hyper 1, tower, h2, tokio and `async-trait` into CI's build today. The remaining additions,
`serde`, `serde_json`, `tokio-stream`, `tokio-util`, `http`, `http-body-util`, `bytes` and
`tracing`, are pure Rust and already locked from the spike graph.

Two specifics. `tokio-util` is taken with default features, not with the `rt` feature.
`CancellationToken` lives in an ungated module, and `rt` would activate the optional
`futures-util` dependency and mutate the lock. `serde_json` is taken with the `raw_value`
feature, which adds no packages, so that `rttMs` can reach the wire through a `RawValue` built
from `go_json_f64`; serde_json's own f64 writer always emits a decimal point and cannot produce
Go's `0`.

`arc-swap` and any JPEG codec are genuinely new to the lock and are excluded from the slice.

### D8. `async_trait` for the dyn-compatible seams

Native `async fn` in traits is stable on 1.92 but is not dyn-compatible, so `Arc<dyn RobotConn>`
would not compile. `async-trait 0.1.x` is already in the lock through tonic's codegen feature, so
using it adds no new package and no new download. Making the seam generic instead would infect
`AppState` and the axum `Router` with a type parameter.

`async_trait` requires `Send` futures, so every test fake must be `Send`, and trait methods must
not be generic, which is required for dyn-compatibility anyway. If clippy objects to the macro
output, the fallback is hand-desugaring to `Pin<Box<dyn Future + Send + '_>>`.

### D9. Docs live in the repo

Under `docs/phases/`, with the plan copied to `docs/plan.md` and amendments recorded there. The
home-directory copy of the plan gets a pointer paragraph, not an edit in place.

### D10. Add a cargo alias for xtask

`[alias] xtask = "run -p xtask --"` in `.cargo/config.toml`, and fix the two stale strings that
document the invocation, in `README.md` and in the xtask usage message. The existing `[env]`
block in that file is load-bearing for the vendored libopus build under CMake 4.x and stays.

### D11. Line-ending policy

Text assets are stored with the bytes of the Windows Go checkout, which is CRLF, and which is
also what production serves. The repository renormalises once so that Git, the disk, the
manifest, the Go checkout and production all agree.

Record that `sync-assets --check` is meaningful only against a Windows checkout of the Go repo
with `core.autocrlf=true` until P9 chooses a cross-platform normalisation.

## Module layout

Reproduced verbatim from the approved plan.

```
crates/wirepod-core/src/
  lib.rs                 #![deny(clippy::await_holding_lock)]; modules; re-exports
  esn.rs                 Esn (trim + ascii lowercase), Generation (opaque, first issued is 1; never named `gen`)
  gofmt.rs               go_format_f32, go_json_f64 -> String (+ RawValue helper), table-tested against docs/phases/P4-sdk-app/gofmt-probe/expected.txt
  clock.rs               Clock, SystemClock, ManualClock
  timings.rs             Timings {settle 500ms, probe 5s, enable 5s, disconnect_settle 3s, idle 300s} with Default
  robot/conn.rs          RobotConn, EventReceiver (EventItem::{Stim(StimEvent), Other}), FrameStream, CameraControl, FrameSink, FrameOutcome,
                         RobotConnFactory, ConnTarget, ConnError{code,desc}, StimEvent{value, velocity}, CameraFrame, BatteryReading
  robot/session.rs       CamOwner, EventOwner (+ stim), SdkSession {cam, events, cam_op: tokio Mutex, last_touch}
  robot/meter.rs         CamMeter {AtomicU64 bytes, frames}; CamMeters {Mutex<HashMap<Esn, Arc<CamMeter>>>} never pruned; get() creates, read() does not
  robot/cam.rs           start_cam_stream -> CamGuard (#[must_use], async finish, Drop logs), finish, cam_stream_pump, PumpExit
  robot/events.rs        EVENT_WHITELIST, EVENT_CONNECTION_ID, run_event_stream
  robot/registry.rs      RobotEntry, RobotRegistry {entries, connect_locks (per-Esn tokio Mutex), meters: CamMeters, timings, liveness_deadline: None},
                         get_or_connect, peek, touch, disconnect, evict_idle, read_meter; GetRobotError (NotFound Display "error: robot not found in SDK info file")
  store/bot_info.rs      BotInfo (disk shape, flatten extras) + BotInfoWire (Go json.Marshal shape), resolve()
  store/bot_status.rs    BotStatus {esn, ip, status, timesince}, PingerState {note_check(&BotInfo, peer_ip), tick, snapshot, set_enabled}
  state.rs               AppState {bot_info, pinger, registry, timings}, AppStateBuilder
  test_support.rs        feature "test-util": FakeRobotConn, FakeConnFactory, FakeReceiver, FakeFrameStream, RecordingCamera, RecordingSink
crates/wirepod-core/tests/  cam_ownership.rs (Go 1,2)  event_ownership.rs (Go 3,4,5,6)  cam_meter.rs (Go 7,8)  registry.rs  bot_status.rs  gofmt.rs  timings.rs
crates/wirepod-vector/src/  lib.rs  error.rs  stream.rs  conn.rs (TonicRobotConn, "authorization: Bearer <guid>")  factory.rs (TonicConnFactory, EndpointBuilder)
                            test_support.rs (feature "test-util": spawn_fake_robot() -> (SocketAddr, FakeRobotHandle) on 127.0.0.1:0, plaintext h2)
crates/wirepod-vector/tests/loopback.rs
crates/wirepod-server/src/  lib.rs  router.rs (build_router, exact-prefix routes, 301 for bare prefixes, fallback for /ok:80 and the Go 404, listener_specs)
                            form.rs (Go FormValue merge)  reply.rs  sdkapp/{mod,net_probe,stim,cam,sdk_info,literals}.rs (SLICE_ROUTES)  api/{mod,bot_status}.rs  conncheck.rs  state.rs
crates/wirepod-server/tests/  contract_bodies.rs  sdk_api.rs  routing.rs  api_contract.rs  live_seam.rs (uses wirepod_vector::test_support)
docs/plan.md                          copy of the approved plan + Amendments section (authoritative; the ~/.claude copy gets a pointer)
docs/phases/README.md                 index P0..P10 with status
docs/phases/P0-foundations.md … P10-cutover.md
docs/phases/P4-sdk-app/{sdkapp-routes,sdkapp-state,camstream,dashboard-client,go-tests,deviations,early-slice-design}.md
docs/phases/P4-sdk-app/gofmt-probe/{main.go,expected.txt}
docs/phases/pending-upstream.md       four kercre123 commits classified; plus a "local branches" section for feature/vector-brain-dashboard and the stash
```

### What C10 built, where it differs from the block above

Four notes, so that the layout above stays the plan's text and the differences stay visible.

`literals.rs` is `crates/wirepod-server/src/literals.rs` rather than `sdkapp/literals.rs`. Its
constants cover all three surfaces, not just `sdkapp`: the `/api/*` CORS values and its `not
found` body, the root file server's `404 page not found` body and its four headers, and the 301
link text. Reaching them through `crate::sdkapp::literals` from `api` and `router` would have
been the wrong shape.

`crates/wirepod-server/src/test_support.rs` is new, behind the same `test-util` feature
`wirepod-core` and `wirepod-vector` use, with the same self dev-dependency. It holds `TestServer`,
which builds an `AppState` over core's fakes and a `ManualClock` and hands back the router; the
free `send_to`, which drives a router through `tower::ServiceExt::oneshot`; the `request` builder;
and `Reply`, a collected response. Four test binaries needed the same three helpers, and an
integration-test binary cannot import another one.

There is no `crates/wirepod-server/src/state.rs`. The axum state is `Arc<wirepod_core::AppState>`
directly, so a server-side wrapper would be a newtype with nothing in it. P1 is the first plausible
reason to add one, when the supervisor's cancellation token and the mDNS handle need somewhere to
live that is not core.

`sdkapp/net_probe.rs`, `sdkapp/stim.rs` and `sdkapp/cam.rs` do not exist yet; their routes are
marked arms in `sdkapp/mod.rs` and land with C11 and C12. `begin_cam_stream` needs no module
because Go's arm is a no-op whose only statement is commented out. `tests/live_seam.rs` lands with
C11 for the same reason.

## Lock ordering and the `await_holding_lock` rule

`crates/wirepod-core/src/lib.rs` carries `#![deny(clippy::await_holding_lock)]`. The
synchronous state machines exist so that five of the eight ported tests need no runtime, and the
single rule that makes `std::sync::Mutex` sound inside an async crate is that no guard ever
crosses an `.await`. A deny-level lint makes that a compile error rather than a review comment,
and CI already runs clippy with `-D warnings`.

The lint does not catch every shape. A guard moved into a struct that is then awaited, or a
future that captures a guard indirectly, can escape it. The structural mitigation is that every
ownership method is small, takes no closures, and returns owned values only, so a guard has
nowhere to escape to.

Lock order is fixed: the camera operation lock first, ownership state second. The operation-lock
map is read only to clone an `Arc<tokio::sync::Mutex<()>>` and is released before the inner lock
is acquired, so the map lock is never held while waiting. That mirrors Go's documented rule at
`robot.go:64-66`, where `camOpMu` takes and releases `robotsMu` on its own before any caller
takes an operation lock.

## The `Generation` naming note

Go names the field and every local `gen`: `robot.go:43`, `:108`, `:121`, `:191`, `:225`, `:236`,
`:300` and `:304`. In edition 2024, which this workspace uses, `gen` is a reserved keyword.
`struct CamOwner { gen: Gen }`, `let gen = ...` and `fn gen()` are hard parse errors and would
need the raw identifier `r#gen`.

So the type is `Generation` and the fields and locals are `generation`. This affects
`robot/session.rs`, `robot/cam.rs`, `robot/events.rs` and every ported Go test. The type is
opaque: only equality matters, the counter is not exposed, and the first issued value is 1.

## The `Timings` struct

One struct in `crates/wirepod-core/src/timings.rs`, carried on `AppState`, with a `Default` impl
whose values a test asserts so nobody changes them silently.

| Field | Default | Go source |
|---|---|---|
| `settle` | 500 ms | the sleep in `startCamStream` when it displaced a previous owner |
| `probe` | 5 s | `npTimeout`, the `net_probe` deadline |
| `enable` | 5 s | the deadline on the `EnableImageStreaming` RPC |
| `disconnect_settle` | 3 s | the sleep in `removeRobot`, once per matched robot |
| `idle` | 300 s | the `connTimer` idle limit |

Handlers read these from `AppState` rather than from constants, from day one, because P1 will
introduce a config type and any handler that reads a constant gets rewritten.

## Test-support feature convention

Both `wirepod-core` and `wirepod-vector` expose their fakes behind a non-default Cargo feature
named `test-util`, in a `test_support` module. Items behind `#[cfg(test)]` are not visible across
crates, and the server's handler tests need core's fakes, so a feature is the mechanism.

`wirepod-core`'s `test-util` exports `FakeRobotConn`, `FakeConnFactory`, `FakeReceiver`,
`FakeFrameStream`, `RecordingCamera` and `RecordingSink`. `ManualClock` is not in that
list: per the module layout block above it lives in `clock.rs` unconditionally, so it is
always available and is not feature-gated.

`wirepod-vector`'s `test-util` exports `spawn_fake_robot()`, which returns a `SocketAddr` and a
handle for an in-process `ExternalInterfaceServer` bound to `127.0.0.1:0` over plaintext h2. It
lives in `src/`, not in `tests/`, precisely so that both `wirepod-vector/tests/loopback.rs` and
`wirepod-server/tests/live_seam.rs` can use it; an integration test binary under `tests/` is not
importable from another crate.

Consumers list the crate a second time as a dev-dependency with `features = ["test-util"]`. Under
resolver 3 this unifies only for `cargo test`, so `cargo build` and the default `cargo clippy`
invocation stay free of test-only code. If that turns out to be wrong and dead-code warnings fail
the `-D warnings` gate, the fallback is a dedicated dev-only test-fixture crate, which costs one
workspace member the plan does not budget for.

The fake robot binds `127.0.0.1:0` explicitly, never `0.0.0.0` and never a fixed port, so Windows
Defender does not prompt.

## Tier A, B and C scope

### Tier A: pure, plain `#[test]`, no runtime

`Esn` (trim plus ASCII lowercase) and `Generation`. `CamOwner`, which is preemptive: a new claim
displaces and cancels the previous owner and reports that it did so. `EventOwner`, which is
exclusive: a claim while owned is refused, the stim write is generation-fenced, and a stop zeroes
the stim value in the same critical section as the release. `CamMeter` and `CamMeters`.

The `gofmt` module. `go_format_f32` reproduces Go's `%v` on a float32, which is
`strconv.FormatFloat(v, 'g', -1, 32)`: shortest round-trip digits, exponent form when the decimal
exponent is below -4 or at least 6, so `5e-05` and `1e+06`, and the spellings `+Inf`, `-Inf` and
`NaN`. `go_json_f64` returning a `String` reproduces `encoding/json`: `14` rather than `14.0`,
plain form inside `[1e-6, 1e21)`, and exponent form outside it, so `1e-7` and `1e+21`. Both are
table-tested against `gofmt-probe/expected.txt`, which is the recorded output of a Go program
committed beside it rather than a hand-written expectation.

`BotInfo` as the on-disk struct, with `global_guid` followed by
`robots[{esn, ip_address, guid, activated}]`, `#[serde(default)]` and a `#[serde(flatten)]` extras
map for rollback safety, plus the `BotInfoWire` projection without extras so `get_sdk_info` stays
byte-exact with Go's marshal. Lookup is last-match-wins, because Go's loop has no break, with the
global GUID as the fallback when a robot's own GUID is empty.

`PingerState` with `note_check(&self, info: &BotInfo, peer_ip: &str) -> bool`, `tick`, `snapshot`
and `set_enabled`.

`ConnError` carrying a code and a description, whose `Display` reproduces grpc-go's
`rpc error: code = <Code> desc = <message>`, with the deadline case byte-exact because it is by
far the most common failure the dashboard shows. `tonic::Status::to_string()` produces a
different shape and would look wrong in the panel.

The response builders, and a `literals` module holding every plain-text body as a constant with a
verbatim test per constant.

The `null` versus `[]` asymmetry is documented, not typed.

### Tier B: async and HTTP, driven through `tower::ServiceExt::oneshot` plus one loopback fake

`run_event_stream`, which selects on a `CancellationToken` and yields `EventItem::{Stim, Other}`.
`start_cam_stream` and `finish_cam_stream` under the operation lock. `cam_stream_pump` writing
through a `FrameSink`. The registry, including `touch`, `disconnect` with its settle, and a pure
`evict_idle(now)`. `TonicRobotConn`.

Routes: `/ok` and `/ok:80`, bodies only, with the pinger and mDNS side effects deferred to P1 and
recorded in `deviations.md`. The exact path `/api-sdk/` and the wildcard `/api-sdk/*rest` on one
handler, because axum's wildcard does not match the bare prefix and Go's subtree pattern does.
`/api-sdk` answering 301 to `/api-sdk/` like Go's mux. The preamble reproduced faithfully: the
connect is always attempted, and `get_sdk_info` and `debug` ignore a connect error and skip the
timer reset, which is exactly the Go exemption and no more. Dispatch driven by a `SLICE_ROUTES`
constant covering `conn_test`, `net_probe`, `begin_event_stream`, `stop_event_stream`,
`get_stim_status`, `begin_cam_stream` (a no-op that answers `done`), `stop_cam_stream`,
`disconnect`, `get_sdk_info` and `debug`. The exact path `/api/` and the wildcard `/api/*rest`
with CORS on every response, serving `get_bot_status`, plus the `/api` 301. And the router
fallback answering Go's file-server 404 with its exact headers.

### Tier C: excluded until P1 or P4

Real listeners, TLS, mDNS and the supervisor. Static file serving. The logger ring and
`/api/get_logs_json`. The jdocs store, `get_sdk_settings` and `get_robot_stats`. `get_battery`,
whose protobuf `omitempty` behaviour omits `battery_volts` entirely at 0.0 volts and which is the
first P4 follow-up. The `/cam-stream` HTTP route with its multipart framing and JPEG re-encode,
which needs a codec and whose body can never be byte-compared anyway. The background pinger tick
and the idle sweeper task, whose rules are implemented and tested but whose drivers are not.
Behavior control. And `arc-swap`.

One Tier C note to carry forward: Go sends no `Content-Type` when a handler writes zero bytes, as
`move_wheels`, `move_lift`, `move_head` and `play_sound` do on their error paths. Those need an
empty `Body`, not an empty `String`. Every other text body is a sniffed
`text/plain; charset=utf-8`, which axum's `String` responder already produces.

## The 404 policy

There are three distinct cases and they must not be confused.

Paths Go genuinely lacks. A 404 is the contract. The routing test asserts 404 for a fixed list of
these and only for them.

Routes Go has that the slice defers. A 404 is a stub. These are listed by name in
`deviations.md`, and no test asserts their status, so that P4 does not have to hunt down and
delete wrong assertions.

Routes the slice serves. Asserted by body, status and headers in the contract tests.

The ordering matters as much as the status. Go runs the connect preamble for every `/api-sdk/*`
path, including unknown ones, so an unknown path with an unknown serial answers the doubled
`error: error: robot not found in SDK info file` at HTTP 200, not a 404. Registering the slice
routes individually in axum would 404 first and lose that ordering, which is why there is one
catch-all handler with an internal match.

## The `Cargo.lock` gate

Run on C3 and on every commit that touches a manifest.

The check is that the sorted set of `(name, version, checksum)` triples in `Cargo.lock` is
unchanged. Workspace-member dependency arrays will change, and those are reviewed by eye.

The naive form of this gate, grepping for a new `source = "registry..."` line, is a false green.
`Cargo.lock` v4 records a dependency array per package including for workspace members, so
adding `tokio` to `wirepod-core` changes the lock without adding a source line. A version bump
also leaves the source line as unchanged diff context. On C3 specifically the gate is stronger:
`git diff --exit-code Cargo.lock` must pass, because introducing the workspace dependency table
and routing the two existing manifests through it changes no resolution at all.

## Commit sequence C3 through C12

Every commit must leave `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo build` and
`cargo test` green on Windows locally, and `cargo clippy --all-targets` is run as well, because
CI's clippy invocation compiles test code without linting it. Stage by explicit path; never use
`git add -A`. No attribution trailers.

**C3, workspace dependency table.** Add `[workspace.dependencies]` at the root and switch
`wirepod-proto` and `xtask` to `.workspace = true`. Gate: `git diff --exit-code Cargo.lock`.

**C4, core identity, timings, formatting, ownership and meters.** `Esn`, `Generation`, the
`Timings` struct with its asserted default, the `gofmt` module with its table test against
`expected.txt`, `CamOwner`, `EventOwner` with the stim fence, and `CamMeter` plus `CamMeters`.
Delivers the ports of Go tests 2, 5, 6, 7 and 8. No async anywhere in this commit.

**C5, core seam traits and the event loop.** The trait definitions, including `CameraControl` and
`FrameSink` even though their implementations arrive in C6, so that the `test-util` fakes compile.
`ConnError` with its Go status text. The `test-util` fakes. `run_event_stream` with cancellation.
Delivers the ports of Go tests 3 and 4. This is the commit that introduces `async_trait`, in
isolation, before any handler depends on it.

**C6, core camera guard, operation lock, handoff and frame pump.** `CamGuard`, the per-ESN
operation lock, `start_cam_stream` and `finish_cam_stream`, and `cam_stream_pump` with its
`PumpExit` outcomes. Delivers the port of Go test 1 as two deterministic interleavings, plus the
pump tests for counting before the sink, continuing past a decode skip, and returning on stream
error, cancellation or a closed sink.

**C7, core stores and clock.** `BotInfo` and `BotInfoWire` with the disk round trip and the wire
projection, the resolve rules, `BotStatus`, `PingerState`, and the `Clock` trait with
`SystemClock` and `ManualClock`. Tests cover the status vocabulary and thresholds, the round
trip, last-match-wins and the global GUID fallback.

**C8, core registry and `AppState` builder.** The registry with its entries, connect locks,
meters and timings, and the builder. Tests cover the idle rule just past 300 seconds, `touch` resetting
it, the disconnect settle, the per-ESN connect lock, and that the meter survives eviction and
reconnect. Acceptance: `cargo test -p wirepod-core` runs the eight ported tests plus the
additions in seconds, and each Go-derived test is confirmed to fail when its guard is removed.

**C9, vector client, adapters, factory and the loopback test.** `TonicRobotConn` attaching the
bearer authorisation metadata per call, the `Streaming` adapters, `TonicConnFactory` with its
injectable `EndpointBuilder`, and `test_support::spawn_fake_robot`. Plus
`crates/wirepod-vector/tests/loopback.rs`. This is the first commit in the suite that opens a
socket, and it is loopback only. Acceptance: it passes without a firewall prompt.

**C10, server router and the stateless routes.** The Go form-value extractor, the reply helpers,
the `literals` module, and the router with its exact-prefix routes, the 301s for the bare
prefixes, the Go-style fallback, `/ok` and `/ok:80`, the `/api-sdk/` preamble, the stateless
routes, `get_sdk_info`, `debug`, `/api/get_bot_status`, `SLICE_ROUTES` and `listener_specs`.
Write the router-builds-without-panic test first, because `matchit`'s colon handling is a
build-time hazard.

**C11, server `net_probe` and the stim routes.** `net_probe` with its number reaching the wire
through a `RawValue`, and the stim routes. Plus `crates/wirepod-server/tests/live_seam.rs`
against `spawn_fake_robot`.

**C12, server disconnect and camera stop.** `disconnect`, `stop_cam_stream`, and the tests for
the idle-timer asymmetry, which is that every `/api-sdk/*` request touches the timer while
`/cam-stream` never does.
