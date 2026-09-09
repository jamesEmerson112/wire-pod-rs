# The eight Go tests and their Rust counterparts

`chipper/pkg/wirepod/sdkapp/sdkapp_test.go` is 496 lines and holds the first and only tests the
Go `chipper/` tree has ever had. They arrived with the Vector Brain work in three of the eight
commits between `685f853` and `origin/main`: `255a737` created the file with the two camera
tests, `083e6b0` added the four event-stream tests, and `18ecd8a` added the two meter tests.

They are the primary oracle for the early Rust slice. Each one pins a concurrency invariant that
a route-by-route JSON diff cannot express, so the Rust ports are what gate the slice rather than
any parity harness.

## Package state and the two test seams

All eight tests drive package-level mutable globals in
`chipper/pkg/wirepod/sdkapp/robot.go`: the `robots` slice at `robot.go:20`, the single
`robotsMu` mutex at `robot.go:32`, the camera registry `camStreams` plus `camGen` at
`robot.go:47-48`, the per-robot operation locks `camOps` at `robot.go:62`, the event registry
`eventStreams` plus `eventGen` at `robot.go:195-196`, and the meters `camMeters` at
`robot.go:161`.

Because that state is global, the file forbids `t.Parallel` outright and every test begins with
`resetSdkState(esn)` at `sdkapp_test.go:42-52`, which takes `robotsMu`, reinstalls a single robot
with a nil `Vector`, and clears the four maps and two generation counters. The nil `Vector` is why
nothing in the tests may reach a real RPC.

Two seams exist purely for testability, and both are documented as such in the Go source.

`eventReceiver` at `server.go:637` is a one-method interface holding just the `Recv` the stim
loop uses. The generated gRPC client satisfies it structurally, so there is no adapter and no
change at the production call site. `runEventStream` at `server.go:644` takes it as a parameter.

`enableImageStreaming` at `server.go:670` is a package variable holding a function, not a
function, specifically so a test can substitute a fake and assert the ordering of the on and off
calls. Nothing in production reassigns it.

The Rust slice replaces both with dependency injection. The equivalent of `eventReceiver` is the
`EventReceiver` trait in `crates/wirepod-core/src/robot/conn.rs`, and the equivalent of the
`enableImageStreaming` variable is the `CameraControl` trait in the same module, supplied through
`AppState` rather than through a mutable global. Because the state hangs off an `Arc` per test
instead of off the package, there is no Rust analogue of `resetSdkState` and no serial-execution
constraint.

The other test helpers are deliberately not ported. `busyWait` at `sdkapp_test.go:168-174` is a
CPU-calibrated delay of 400 000 additions; a literal Rust translation would be optimised away
without `std::hint::black_box`, silently closing the window the test depends on and passing
regardless of the fix. `waitFor` at `sdkapp_test.go:394-404` polls a condition every millisecond
against a two second wall-clock deadline, and at both of its call sites it is already a no-op,
because `claimEventStream` sets the streaming flag synchronously at `robot.go:227` before the
receiver goroutine is spawned.

Per decision D5 the Rust tests use no `tokio::time::pause` and no `start_paused`. Every timing
constant in the slice has an injection seam, so the settles are configured to zero and the
deadlines to milliseconds, and the only wall-clock element is a real-time
`tokio::time::timeout` ceiling of two seconds that fires only on a regression.

---

## 1. `TestCamStreamHandoffKeepsCameraOn`

`sdkapp_test.go:66-127`.

**What it pins.** Whenever a live camera owner exists, the last thing the robot was told must
have been "on". The test states the invariant at `sdkapp_test.go:60-61`.

**How it works.** It swaps `enableImageStreaming` for a fake that, on a disable, burns
`busyWait()` before recording, then runs 60 iterations. Each iteration resets the state, claims
the camera directly with `claimCamStream` so the outgoing owner records no "on" call, and then
runs two goroutines concurrently: one calls `finishCamStream` for the departing handler, the
other calls `startCamStream` for the replacement. After joining, it reads the registry entry and
asserts the last recorded call was `true`.

**Seams used.** The `enableImageStreaming` package variable, plus `claimCamStream`,
`startCamStream`, `finishCamStream` and direct access to `camStreams` under `robotsMu`.

**Bug guarded.** Before `255a737`, `releaseCamStream` dropped the registry entry and returned,
and only then did the departing handler issue the disable. A replacement arriving in that window
found no previous owner, skipped the settle, turned the camera on, and then the departing disable
landed on top and left a dead stream. The fix is the per-robot operation lock held across both
the registry update and the RPC. The commit message records that it failed three runs out of
three with the lock removed.

**Rust mapping.** `crates/wirepod-core/tests/cam_ownership.rs`, delivered by commit C6. Only two
interleavings exist under the fix, and both were traced during the design, so the port is two
deterministic cases rather than a repeat count. When `finish` wins the operation lock the release
succeeds, the entry is deleted, a disable is recorded, and the replacement then claims with no
previous owner, skips the settle and records an enable. When `start` wins, it claims, displaces,
cancels, settles, records an enable, and the departing `finish` then sees a newer generation, so
`release` returns false and no disable is issued. Either way the last recorded call is an enable.

The fake's disable must record after an awaited gate, because the property is about which RPC
lands last, not which is issued first; the record-after-latency ordering at
`sdkapp_test.go:73-78` is load-bearing. A multi-threaded stress variant closer to Go's 60
iterations is kept behind `#[ignore]` for anyone who wants the probabilistic form.

One Go detail worth keeping honest: the `owner == nil { continue }` escape at
`sdkapp_test.go:113-117` is unreachable, because `startCamStream` always claims. The Rust port
asserts the owner is present rather than skipping.

---

## 2. `TestReleaseCamStreamIgnoresSupersededOwner`

`sdkapp_test.go:131-164`.

**What it pins.** A displaced camera handler must not release, or clear the streaming flag of,
the owner that displaced it. Seven assertions in sequence: the first claim does not displace, the
second does and gets a different generation, releasing with the first generation returns false,
the streaming flag is still true, releasing with the second generation returns true, and only
then is the flag false.

**Seams used.** None. `claimCamStream`, `releaseCamStream` and `isCamStreaming` only. No
goroutines, no sleeps, no fakes.

**Bug guarded.** The generation-fencing half of `68ce134` and `255a737`. Before ESN-keyed
generations, cleanup was by slice index, and `removeRobot` at `robot.go:455-480` invalidates
indices by rebuilding the slice through a filter.

**Rust mapping.** `crates/wirepod-core/tests/cam_ownership.rs`, delivered by commit C4. A plain
`#[test]` with no runtime; `CancellationToken::new()` and `cancel()` need none. The state machine
under test is `CamOwner` in `crates/wirepod-core/src/robot/session.rs`.

---

## 3. `TestEventStreamNeverHasTwoOwners`

`sdkapp_test.go:253-302`.

**What it pins.** Repeated stop-and-begin cycles never leave two live stim receivers. Twenty-five
cycles, each of which claims the stream, spawns a goroutine running `runEventStream` against a
fake receiver, stops the stream, and requires the goroutine to exit within two seconds. After the
loop it asserts the observed peak receiver count is at most one and that the stream is no longer
marked as streaming.

**Seams used.** `eventReceiver`, through a `fakeReceiver` at `sdkapp_test.go:188-214` whose
`Recv` selects on a context and a channel, plus `claimEventStream`, `runEventStream`,
`stopEventStream` and `isEventStreaming`.

**Bug guarded.** `083e6b0`. `stop_event_stream` used to set a flag and nothing else. The receiving
goroutine sampled that flag at the top of its loop, which it reaches only after `Recv` returns,
and `Recv` blocks until the robot sends a stim event. On a quiet robot the goroutine outlived its
stop, a later begin saw the flag false and admitted a second goroutine, and both then wrote the
stim value while only the newer one was reachable by a stop.

**Structural weaknesses to correct in the port.** Two of the test's assertions are weaker than
they look. The `waitFor(isEventStreaming)` at `sdkapp_test.go:278` is a no-op, because the
flag is set synchronously by the claim before the goroutine is spawned. The peak assertion is
vacuous, because the test joins each cycle before the next claim, so structurally at most one
goroutine can be inside `runEventStream` at a time. The real assertion is the two second
deadline.

**Rust mapping.** `crates/wirepod-core/tests/event_ownership.rs`, delivered by commit C5. Five
cycles rather than twenty-five, each bounded by a real-clock `tokio::time::timeout` of two
seconds that only fires on a regression. The Rust `run_event_stream` selects on a
`CancellationToken` as well as on `recv`, which makes the fake receiver a trivial channel wrapper
with no context awareness, and lets the test use a receiver that ignores cancellation entirely
and still assert prompt teardown. That is a strictly stronger property than the Go test proves.
Readiness is signalled by a oneshot fired by the receiver task, never by a polling loop.

---

## 4. `TestStopEventStreamEndsReceiverPromptly`

`sdkapp_test.go:306-334`.

**What it pins.** A single stop must end a receiver that is parked in `Recv` on a robot that is
sending nothing, within two seconds, and must leave the stream unmarked afterwards.

**Seams used.** The same `fakeReceiver` plus `claimEventStream`, `runEventStream`,
`stopEventStream` and `isEventStreaming`.

**Bug guarded.** The same `083e6b0` finding as test 3, isolated to one cycle. The Go source calls
this the part the old code could not do at all.

**Rust mapping.** `crates/wirepod-core/tests/event_ownership.rs`, commit C5. The single-cycle
form of the previous test, with the same two second real-clock ceiling and the same
cancellation-ignoring receiver.

---

## 5. `TestClaimEventStreamRefusesWhileOwned`

`sdkapp_test.go:338-360`.

**What it pins.** The event stream is exclusive, not preemptive. A second claim while the stream
is owned is refused, and a claim arriving immediately after a stop is admitted.

**Seams used.** None. `claimEventStream` and `stopEventStream` only, with no concurrency at all.

**Bug guarded.** The double-click case from `68ce134` and `083e6b0`: re-selecting the Stim tile
must reuse the running stream rather than start a second one. The third assertion pins the
deliberate decision at `robot.go:248-252` that `stopEventStream` releases ownership in the same
critical section as the cancel, so a begin arriving straight after a stop is not turned away
while the old receiver is still unwinding. Without that, the dashboard's 500 ms poller would read
`error: must start event stream` until its breaker gave up.

This asymmetry against the camera is deliberate and is explained at `robot.go:208-213`. The
camera has no stop protocol, so a reloaded `<img>` must be able to take the feed; stim has an
explicit stop, so a second begin is a double click.

**Rust mapping.** `crates/wirepod-core/tests/event_ownership.rs`, commit C4. A plain `#[test]`
with no runtime, driving `EventOwner` in `crates/wirepod-core/src/robot/session.rs`.

---

## 6. `TestSupersededReceiverCannotWriteStimState`

`sdkapp_test.go:364-390`.

**What it pins.** A receiver still unwinding from a cancelled `Recv` must not overwrite the value
its replacement just published. The sequence claims, writes 0.25 through the owning generation,
stops, claims again, writes 0.75 through the new generation, then attempts a write of 0.1 through
the stale generation and asserts the value is still 0.75.

**Seams used.** None. `claimEventStream`, `setStimStateIfOwner`, `stimState` and
`stopEventStream`, with no concurrency.

**Bug guarded.** The write-side generation fence from `083e6b0`. It also indirectly covers the
removal of the old index-based `robots[robotIndex].StimState` write, which `removeRobot`'s slice
rebuild could aim at a different robot or off the end.

**Rust mapping.** `crates/wirepod-core/tests/event_ownership.rs`, commit C4. Plain `#[test]`, no
runtime. The port adds one assertion the Go test does not make: that the stim value is zero
immediately after the stop. `stopEventStream` zeroes it at `robot.go:258`, but the Go assertions
never observe it, so a port that omitted the zeroing would still pass them.

---

## 7. `TestCamMeterKeepsRobotsApart`

`sdkapp_test.go:415-443`.

**What it pins.** Meters are per-ESN and an ESN nobody has streamed reads as zero rather than
panicking, because `net_probe` answers for robots whose camera has never been opened. Four
assertions: the two meters are distinct objects, each reads back its own totals, and a third,
never-seen ESN reads `(0, 0)`.

**Seams used.** None. `getCamMeter`, `readCamMeter` and direct atomic adds on the struct fields.

**Bug guarded.** `18ecd8a`, and the older index-keying defect it inherits. Two robots streaming
at once must not be counted together.

**Rust mapping.** `crates/wirepod-core/tests/cam_meter.rs`, commit C4. Plain `#[test]`, no
runtime, over `CamMeter` and `CamMeters` in `crates/wirepod-core/src/robot/meter.rs`.

The port adds an assertion and drops a Go behaviour. Go's `readCamMeter` calls `getCamMeter` at
`robot.go:183`, so reading an unknown ESN inserts an entry as a side effect, and entries are never
deleted. In production that is gated by `getRobot` failing first for an unknown serial, so it is
not an unbounded-growth vector today, but it is a read that mutates. The Rust `read` returns
`(0, 0)` without allocating, and the test asserts both the zero result and that the map did not
grow.

---

## 8. `TestCamMeterCountsExactlyUnderConcurrency`

`sdkapp_test.go:449-496`.

**What it pins.** The counters are genuinely atomic and lose no updates. Eight writer goroutines
each add 1234 bytes and one frame two thousand times, while a reader goroutine spins on
`readCamMeter`, the accessor `net_probe` uses, so that the map lookup under `robotsMu` runs
concurrently with the lock-free adds. After joining, the totals must be exactly 19 744 000 bytes
and 16 000 frames.

**Seams used.** None. `getCamMeter`, `readCamMeter`, and the raw `uint64` fields that
`sync/atomic` can address.

**Bug guarded.** `18ecd8a`. This is the test that earns `-race`: the frame loop adds without
holding `robotsMu` while `net_probe` reads from another goroutine, so making the counters
non-atomic fails two independent ways, through race detection and through a short total.

**Rust mapping.** `crates/wirepod-core/tests/cam_meter.rs`, commit C4. A plain `#[test]` using
`std::thread::scope`, with no runtime and no async. In Rust the property is largely discharged by
the type system, since a shared `Arc<CamMeter>` with plain `u64` fields does not compile, so the
port keeps a reduced smoke test over the accessor plumbing rather than the full Go shape.
`Relaxed` ordering is sufficient for the only asserted property, the exact total. Go's reader
goroutine is an unyielding spin that takes the package mutex every iteration and would starve the
writers on a single-core runner, so the Rust reader is bounded rather than free-running. Both
implementations read `(bytes, frames)` non-atomically as a pair, which is acceptable because the
dashboard differences two samples; the Rust code says so in a comment.

---

## Where each Go test lands

| Go test | Rust test file | Commit | Runtime needed |
|---|---|---|---|
| 1 handoff keeps camera on | `crates/wirepod-core/tests/cam_ownership.rs` | C6 | Yes, two deterministic interleavings plus an `#[ignore]` stress variant |
| 2 superseded release ignored | `crates/wirepod-core/tests/cam_ownership.rs` | C4 | No |
| 3 never two event owners | `crates/wirepod-core/tests/event_ownership.rs` | C5 | Yes, five cycles under a two second ceiling |
| 4 stop ends a parked receiver | `crates/wirepod-core/tests/event_ownership.rs` | C5 | Yes, one cycle under a two second ceiling |
| 5 claim refuses while owned | `crates/wirepod-core/tests/event_ownership.rs` | C4 | No |
| 6 superseded receiver cannot write stim | `crates/wirepod-core/tests/event_ownership.rs` | C4 | No |
| 7 meters per robot, zero for unknown | `crates/wirepod-core/tests/cam_meter.rs` | C4 | No |
| 8 exact totals under concurrency | `crates/wirepod-core/tests/cam_meter.rs` | C4 | No |

Five of the eight need no async runtime, which is the direct consequence of keeping the ownership
state machines pure and synchronous on `std::sync::Mutex`.

## What the Go tests do not cover

The file and the commit messages both say so. `camStreamHandler` itself, `SdkapiHandler` and
`/api-sdk/net_probe` are covered by inspection and by a live check against the robot
(`sdkapp_test.go:408-411`), not by any test. `removeRobot`'s teardown path is untested. And the
event-delivery arm of `runEventStream` at `server.go:655-659` is never executed, because no test
ever sends on the fake receiver's channel; only the cancellation arm runs.

That last one matters for the port. The Go loop decides a stim value is present with
`strings.Contains(fmt.Sprint(stimInfo), "velocity")`, and proto3 omits zero-valued scalars from
the text form, so the rule is exactly `velocity != 0`. The fake sets `Velocity: 1` deliberately
at `sdkapp_test.go:203-208` to stop a test passing for the wrong reason, but nothing exercises it.
The Rust slice models the rule as `EventItem::{Stim, Other}` plus a check on `velocity != 0.0`,
and covers the delivery arm with its own tests.

## Rust-only tests the slice adds

The eight ports are the concurrency oracle. Everything else the slice pins is new, because Go has
no test for it. These are listed in full in `early-slice-design.md` and summarised here by
purpose.

**Byte-exact bodies and their literals.** Every constant in the `literals` module is asserted
verbatim, so a typo in `done`, `success`, `ok`, `ran` or `error: must start event stream` fails
CI rather than reaching the robot. A parity document nobody re-reads is a wish; a constant with a
test is a gate.

**The single most load-bearing negative test.** `get_stim_status`'s idle body must fail to parse
as JSON. Nothing on the Go side tests it, and if a later refactor makes it a JSON error object the
dashboard breaks silently rather than loudly. See `dashboard-client.md` for why.

**Preamble ordering.** The doubled `error: error:` prefix; the preamble running before the 404
for an unknown path; `get_sdk_info` and `debug` ignoring a connect failure and skipping the
timer reset, driven by a connection factory that always fails.

**Routing.** That the exact paths `/api-sdk/` and `/api/` reach their handlers, which a bare
axum wildcard does not do; that the bare prefixes answer 301 with a `Location` header; that
`/ok:80` resolves through the fallback while `/ok:81` is a 404; that the router builds without
panicking; and that one router value answers identically for every listener specification, which
is how the "same routes on every port" rule is tested without binding a port.

**Go formatting.** Table tests for `go_format_f32` and `go_json_f64` against the recorded output
of the probe program in `gofmt-probe/`, covering the exponent switch, the infinities and NaN, and
the integral cases where Rust and serde_json would otherwise emit a fractional part.

**`net_probe` specifics.** The body byte for byte, `0` rather than `0.0`, a timeout counted as a
lost probe, the verdict never read (the fake answers `UNSUPPORTED` and the body is still a
success), `camOn` following ownership, and the counters staying monotone across a stop.

**Stores and registry.** The bot-info disk round trip preserving unknown fields while the wire
projection drops them; last-match-wins lookup and the global GUID fallback; the bot-status
thresholds and the stored-case ESN; the form merge where a body value shadows a query value and
both `serial=null` and `serial=` mean no robot; the frame pump counting before the sink,
continuing past a skip, and returning on stream error, cancellation or a closed sink; the idle
rule just past 300 seconds with `touch` resetting it; and the meter surviving eviction and reconnect,
which is the only thing guarding a refactor that would fold the meter into the evictable entry.

**Defaults.** That `Timings::default()` holds the Go values, so nobody silently changes the 500
millisecond settle or the 5 second deadlines.

**The loopback seam.** One integration test in `crates/wirepod-vector/tests/loopback.rs` against
an in-process fake robot on `127.0.0.1:0` proves the wire shape: `client_version = 5` and
`min_host_version = 0` on the probe, the whitelist `["stimulation_info"]`, the connection id
`wirepod` on the stream the slice opens, the literal bearer authorisation metadata on every call,
that connect issues only `BatteryState`, and that zero-velocity events are skipped.
