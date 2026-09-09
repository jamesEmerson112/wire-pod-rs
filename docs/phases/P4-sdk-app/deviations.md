# Recorded deviations from the Go server

Every deliberate difference between the early Rust SDK-app slice and the Go server at
`origin/main` (`81fa3b3`). The project rule is that vendored things stay byte-identical and that
behavioural differences are documented, never silent, so anything not listed here is a bug.

Each entry gives what differs, why, and where it is tested or otherwise recorded. Go citations
are `path:line` under `C:/Users/voan2/Documents/GitHub/wire-pod/chipper/`.

---

## 1. No connect-time `EventStream`

**Go.** `newRobot` opens two things at connect time. After the `BatteryState` liveness check it
opens a second `EventStream` at `pkg/wirepod/sdkapp/robot.go:371-382`, with the whitelist
`["stimulation_info"]` and no `connection_id`, stores the client on the robot struct, and never
reads it. The stim feature uses a different stream, opened by `begin_event_stream`.

**Rust.** `RobotRegistry::get_or_connect` issues only `BatteryState`. The dead stream is not
opened.

**Why.** It is a robot-visible RPC that nothing consumes. Reproducing it would mean holding one
extra open stream per cached connection for no observable benefit, and the robot pays for it.
The difference is one fewer open stream on the robot per connected serial.

**Where tested.** `crates/wirepod-vector/tests/loopback.rs` asserts that a connect issues
`BatteryState` and nothing else. The same test asserts the connection id `wirepod` on the stream
that `begin_event_stream` does open, so the two streams cannot be confused later.

---

## 2. `/ok` and `/ok:80` side effects deferred to P1

**Go.** `connCheck` at `pkg/wirepod/sdkapp/jdocspinger.go:193-220` writes `ran` after calling
`RunMDNS("t")` synchronously when `runMDNS=true`. Otherwise, when the pinger is enabled, it
splits the peer IP out of `r.RemoteAddr`, marshals the bot-info store, and either calls
`ShouldPingJdocs` followed by `pingJdocs` for a known peer IP, or spawns `go RunMDNS(peerIP)` for
an unknown one, before writing `ok`.

**Rust.** The slice reproduces the bodies only: `ok`, or `ran` when `runMDNS=true`. No mDNS call,
no jdocs ping, no peer-IP branch.

**Why.** Both side effects need subsystems the slice does not have. mDNS registration and
browsing and the jdocs store are P1 work, and the slice deliberately binds no ports and writes no
disk state.

**Where tested.** The bodies are asserted in `crates/wirepod-server/tests/contract_bodies.rs`.
The peer-IP rule itself is implemented as `PingerState::note_check`, which mirrors
`ShouldPingJdocs` and is exercised by a unit test in `crates/wirepod-core/tests/bot_status.rs`
rather than by the handler. P1 wires it into the handler alongside mDNS and the jdocs store.

---

## 3. `BotInfoWire` projection for `get_sdk_info`

**Go.** `vars.RobotInfoStore` is a plain struct with exactly `global_guid` and
`robots[{esn, ip_address, guid, activated}]`. `json.Unmarshal` drops every other key in the bot
json file, and the marshal at `pkg/wirepod/sdkapp/server.go:190` therefore re-emits only those
fields, in declaration order.

**Rust.** Two types. The disk struct `BotInfo` carries `#[serde(default)]` and a
`#[serde(flatten)]` extras map, per the repo-wide rollback-safety rule, so unknown and fork-only
fields survive a load and save. A separate `BotInfoWire` projection without the extras is what
`get_sdk_info` serialises.

**Why.** Without the split, the two rules collide. Keeping the extras makes rollback to the Go
server safe, but re-emitting them would change the `get_sdk_info` body for any bot json carrying
extra keys, and a flattened map re-serialises in sorted order rather than in Go's declaration
order. The projection keeps both properties: the disk round trip is lossless and the HTTP body
stays byte-exact with Go.

**Where tested.** `crates/wirepod-core/tests/bot_status.rs` asserts the disk round trip preserves
unknown fields and that the wire projection drops them.
`crates/wirepod-server/tests/sdk_api.rs` asserts the `get_sdk_info` key order, the absence of
extras, and the 500 with a trailing newline when the robot list is empty.

---

## 4. Extras are sorted on a disk round trip

**Go.** Not applicable. Go drops unknown fields entirely, so there is no ordering to preserve.

**Rust.** Unknown fields captured by `#[serde(flatten)]` land in a `serde_json::Map`, which is a
`BTreeMap` because serde_json's `preserve_order` feature is off. They therefore re-serialise in
sorted key order rather than in the order they appeared in the file.

**Why.** The property the rollback rule actually needs is that unknown fields survive a round
trip, which flatten already gives. Turning on `preserve_order` to keep the original order would
pull `indexmap` into `serde_json`'s dependency array in `Cargo.lock`
(`preserve_order = ["indexmap", "std"]` in serde_json 1.0.151). The package itself is not new,
because `indexmap 2.14.0` is already resolved in the lock from the spike graph, so decision D7's
`(name, version, checksum)` gate would not catch it; the exclusion is a scope call for this
slice. The feature would also be unified across every workspace crate that uses `serde_json`.

**Where recorded.** Here, and in the amendments section of `docs/plan.md`. The disk round trip
test in `crates/wirepod-core/tests/bot_status.rs` asserts survival, not order.

---

## 5. Line-ending policy for vendored assets (D11)

**Go.** The Go checkout on this machine has `core.autocrlf = true` and no `.gitattributes`, so
its working tree is CRLF, and the installed production copy under
`C:\Program Files\wire-pod\chipper\webroot` is CRLF as well.

**Rust.** Text assets under `assets/` are stored with the bytes of that Windows checkout, which
is CRLF, and `.gitattributes` sets `* -text` so Git never converts anything. The assets were
originally committed under `core.autocrlf=true` before `.gitattributes` arrived, which left 42 of
65 tracked files with LF-only blobs while the working tree was CRLF; commit C0 renormalises once
so that Git, the disk, the manifest, the Go checkout and production all agree.

**Why.** The manifest hashes and the drift check are only meaningful if one set of bytes is
authoritative, and production serves CRLF.

**Consequence to remember.** `cargo run -p xtask -- sync-assets --from ../wire-pod --check` is
meaningful only against a Windows checkout of the Go repo with `core.autocrlf=true`. A Linux
checkout would report every text file as drifted. P9 chooses a cross-platform normalisation.

**Where recorded.** Decision D11 in `early-slice-design.md`, the C0 commit message, and
`CLAUDE.md`.

---

## 6. Go routes the slice defers answer 404 as stubs

Go's `SdkapiHandler` has 45 `/api-sdk/*` case arms at `origin/main`. The slice implements nine of
them, plus `debug`, which has no case arm in Go and reaches the 404 default there too. The nine
in-slice arms are `conn_test`, `net_probe`, `begin_event_stream`, `stop_event_stream`,
`get_stim_status`, `begin_cam_stream`, `stop_cam_stream`, `disconnect` and `get_sdk_info`.

The other 36 arms exist in Go and answer 404 in the slice. This is a stub, not a contract. No
test asserts a 404 for any path in this list, and the routing test asserts 404 only for a fixed
list of paths Go genuinely lacks.

The deferred arms, in the order they appear in `pkg/wirepod/sdkapp/server.go`:

| Route | Go line |
|---|---|
| `/api-sdk/alexa_sign_in` | 130 |
| `/api-sdk/alexa_sign_out` | 136 |
| `/api-sdk/cloud_intent` | 142 |
| `/api-sdk/eye_color` | 151 |
| `/api-sdk/custom_eye_color` | 156 |
| `/api-sdk/volume` | 165 |
| `/api-sdk/locale` | 170 |
| `/api-sdk/location` | 175 |
| `/api-sdk/timezone` | 180 |
| `/api-sdk/get_sdk_settings` | 197 |
| `/api-sdk/play_sound` | 231 |
| `/api-sdk/get_battery` | 283 |
| `/api-sdk/time_format_12` | 301 |
| `/api-sdk/time_format_24` | 305 |
| `/api-sdk/temp_c` | 309 |
| `/api-sdk/temp_f` | 313 |
| `/api-sdk/button_hey_vector` | 317 |
| `/api-sdk/button_alexa` | 321 |
| `/api-sdk/assume_behavior_control` | 325 |
| `/api-sdk/release_behavior_control` | 329 |
| `/api-sdk/say_text` | 333 |
| `/api-sdk/move_wheels` | 347 |
| `/api-sdk/move_lift` | 360 |
| `/api-sdk/move_head` | 370 |
| `/api-sdk/get_faces` | 380 |
| `/api-sdk/rename_face` | 391 |
| `/api-sdk/delete_face` | 410 |
| `/api-sdk/add_face` | 425 |
| `/api-sdk/mirror_mode` | 440 |
| `/api-sdk/get_image_ids` | 525 |
| `/api-sdk/get_image` | 537 |
| `/api-sdk/get_image_thumb` | 555 |
| `/api-sdk/delete_image` | 573 |
| `/api-sdk/get_robot_stats` | 591 |
| `/api-sdk/print_robot_info` | 602 |
| `/api-sdk/trigger_wake_word` | 609 |

The same rule applies on the `/api/` side. `config-ws/webserver.go` has 22 dispatch arms
(`webserver.go:32-74`); the slice serves `get_bot_status` and stubs the rest at 404. A bare
`grep -n 'case "'` on that file returns 25 lines, but three of them are the `level` switch inside
`handleGetLogsJSON` (`webserver.go:292`, `:294`, `:296`) and are not routes; see
`sdkapp-routes.md`.

`/cam-stream` is a separate case. It is not an `/api-sdk/*` arm; the route, the multipart framing
and the JPEG re-encode are Tier C, and the ownership, operation lock, settle and byte meter the
route needs are all in the slice already. See `camstream.md`.

---

## 7. `readCamMeter`'s insert-on-read is dropped

**Go.** `readCamMeter` at `pkg/wirepod/sdkapp/robot.go:182-185` calls `getCamMeter`, which creates
an entry when one is missing, so reading an unknown ESN allocates. Entries are never deleted
(`robot.go:163-166`). In production this is bounded because `getRobot` fails first for an unknown
serial, so it is not an unbounded-growth vector today.

**Rust.** `CamMeters::read` returns `(0, 0)` for an unknown ESN without allocating. `CamMeters::get`
still creates on first use, which is the path the frame pump takes.

**Why.** A read that mutates is a hazard rather than a feature, and nothing observable depends on
the entry existing. The zero-for-unknown answer, which `net_probe` needs for a robot whose camera
has never been opened, is preserved exactly.

**Where tested.** `crates/wirepod-core/tests/cam_meter.rs`, the port of Go test 7, asserts both
the zero result and that the map did not grow.

---

## 8. `inhibitCreation` is replaced by per-ESN connect locks

**Go.** A single package-level `inhibitCreation` flag serialises robot creation across every
serial. `getRobot` spins on it at `robot.go:407-412`, so a slow or hanging dial for one robot
stalls every `/api-sdk/*` request for every robot.

**Rust.** A map of per-ESN `tokio::sync::Mutex` connect locks, with the inner lock held across the
dial. The same serial dials once; robot A's dial never blocks robot B.

**Why.** It preserves the property the global flag was reaching for, which is that one serial does
not dial twice concurrently, while removing a global stall that the Go source itself works around
elsewhere. Go commit `255a737` made the camera operation lock per-ESN for exactly this reason, so
this is consistent with the direction the Go code was already moving.

**Where tested.** `crates/wirepod-core/tests/registry.rs` asserts that a second request for the
same serial waits for the first dial and reuses its connection, and that a request for a different
serial does not wait.

**Recorded as an improvement**, not a bug fix, in the amendments section of `docs/plan.md`.

---

## 9. Connect-time liveness deadline is `None`

**Go.** The `BatteryState` liveness check at `robot.go:365` uses `context.Background()` and has no
deadline. A robot that is powered off but whose IP still routes hangs the dial indefinitely.

**Rust.** `RobotRegistry` carries `liveness_deadline: Option<Duration>` defaulting to `None`,
which reproduces Go exactly.

**Why.** Adding a deadline is not on the plan's list of allowed bug fixes, and the blast radius is
identical either way now that the connect lock is per-ESN. Making it an `Option` field rather than
a hardcoded absence means P1 can set it in one line and record the change as its own deviation.

**Where recorded.** Here and in `sdkapp-state.md`. There is no test for the hang, by design.

---

## 10. `get_image_ids` stays `null` when empty

This is a pinned quirk rather than a deviation. It is listed here so nobody removes it.

**Go.** An empty photo list serialises as the JSON literal `null`, not `[]`.

**Client.** `chipper/webroot/sdkapp/js/main.js:166` compares the raw response text against the
string `"null"` to decide whether to show the "no photos found" message. An empty array would
fall through to `JSON.parse` and render an empty list with no explanation.

**Slice status.** The route is deferred, so nothing serves it yet. The `null` behaviour is pinned
now, ahead of the route, so that whoever implements it at P4 has to opt into `null` explicitly
rather than stumble into `[]`.

Note the asymmetry with `/api/get_bot_status`, which must serialise `[]` and never `null`
(`pkg/wirepod/sdkapp/jdocspinger.go:42-45`). Both are deliberate on both sides.

---

## 11. Go sends no `Content-Type` for a zero-byte body

A P4 note rather than a slice deviation, recorded here because it is easy to get wrong once the
deferred routes are written.

Go's `net/http` sniffs a content type only when the handler writes a non-empty body and set no
header. Every text body in this surface therefore arrives as a sniffed
`text/plain; charset=utf-8`, which is exactly what axum's `String` responder produces, so those
routes need no special handling. But the four routes that write zero bytes on their error paths,
`move_wheels`, `move_lift`, `move_head` and `play_sound`, get no `Content-Type` header at all.
Reproducing that needs an empty `Body`, not an empty `String`, because axum's `String` responder
sets the header unconditionally.

None of those four routes is in the slice. The note exists so the distinction is not discovered
by a failing parity diff later.

---

## 12. `Esn` trims where Go's `EqualFold` does not

**Go.** Every serial lookup is `strings.EqualFold(serial, robot.ESN)`, in `getRobot`
(`robot.go:414`), in `removeRobot` (`robot.go:459`) and in each of the small accessors.
`EqualFold` folds case but does not trim, so a `serial` form value that arrives with a leading or
trailing space matches nothing. In `getRobot` that falls through to `newRobot`, whose own
`EqualFold` scan of the bot info file fails the same way and returns
`error: robot not found in SDK info file`. The serial that Go stores on the robot record is
already trimmed, because `newRobot` writes `strings.TrimSpace(strings.ToLower(serial))`
(`robot.go:335`).

**Rust.** `Esn::new` trims and ASCII-lowercases once on construction, and every lookup is then a
plain equality or hash comparison. A serial with surrounding whitespace therefore finds the robot
in Rust where Go would answer that it was not found.

**Why.** Normalizing on construction is what makes `Eq` and `Hash` agree with `EqualFold` without
a custom comparator on every map, and the alternative would be to carry the untrimmed string and
compare case-insensitively at each of the nine call sites. The stored keys agree either way,
because Go trims before it stores, so the difference is confined to a request whose `serial` has
whitespace around it. The web UI sends the serial from the bot info file and never adds
whitespace, so nothing in the shipped client can reach the case.

There is a second, smaller difference in the same method. Rust lowercases ASCII only, where Go's
`strings.ToLower` is Unicode aware. Serials are hexadecimal, so no serial the robots produce
contains a character the two treat differently.

**Where tested.** `crates/wirepod-core/tests/identity.rs` asserts the trimming, the case folding
and that both reach `Hash`, which is what makes an ESN-keyed map stand in for the `EqualFold`
scans. No test asserts the Go behaviour, because reproducing it would mean building the deviation
into the type.

---

## 13. A failed camera enable hands the claim straight back

**Go.** `enableImageStreaming` at `pkg/wirepod/sdkapp/server.go:670-679` calls
`EnableImageStreaming` and discards both return values, so `startCamStream` cannot tell whether
the camera actually came on. It claims, settles, calls the switch and returns the generation
regardless (`server.go:684-695`). A robot that refuses or never answers the enable therefore
leaves the handler owning a feed that is not running, and the ownership is given back only when
the handler's deferred `finishCamStream` eventually runs.

**Rust.** `start_cam_stream` bounds the enable with `timings.enable` and reads its result. On a
failure or an expiry it runs the same generation-checked release the guard would have run, which
also issues the disable, and returns the error. A start that returns an error leaves no owner
behind and hands out no `CamGuard`.

**Why.** The `CamGuard` is `#[must_use]` and has an explicit `finish`, so there is no deferred
cleanup to fall back on: returning an error while keeping the claim would leak ownership with
nothing left holding the guard that could release it. Releasing on the error path reaches the
same end state Go reaches through its defer, one step earlier. The release is generation-checked,
so a start whose enable failed after a replacement already took the feed changes nothing and
issues no disable.

**Consequence to remember.** The disable is issued on this path where Go issues none, because Go
never learns the enable failed. It is one extra `EnableImageStreaming(false)` to a robot that has
just refused or ignored an `EnableImageStreaming(true)`, and it is bounded by the same deadline.

**Where tested.** `crates/wirepod-core/tests/cam_ownership.rs` asserts that an enable which never
answers maps to `rpc error: code = DeadlineExceeded desc = context deadline exceeded` and that the
claim is gone afterwards.

---

## 14. An out-of-range gRPC status code renders as `Unknown`, not `Code(N)`

**Go.** `codes.Code.String()` in grpc-go switches on the 17 defined codes and falls through to
`"Code(" + strconv.FormatInt(int64(c), 10) + ")"` for anything else (`codes/code_string.go`). A
status carrying code 42 therefore reaches the dashboard as
`rpc error: code = Code(42) desc = ...`.

**Rust.** `StatusCode::from_wire` maps anything outside `0..=16` to `StatusCode::Unknown`, so the
same status would render as `rpc error: code = Unknown desc = ...`. That follows tonic, whose
`Code::from_i32` collapses unrecognised values to `Unknown` before the conversion in
`wirepod-vector` ever sees them.

**Why.** Reproducing `Code(N)` would mean carrying the raw integer through a type whose whole
purpose is to name the 17 codes, and tonic has already discarded it by the time the conversion
runs. The case is unreachable in practice: the peer is a grpc-go server, which only ever sends
codes it has names for.

**Where recorded.** Here, and on the doc comment of `StatusCode::from_wire` in
`crates/wirepod-core/src/robot/conn.rs`. There is no test, because the value the test would need
cannot arrive from tonic.

---

## Additional recorded differences

**`run_event_stream` selects on the cancellation token.** Go's loop relies on the receiver
observing a cancelled context. The Rust loop selects on a `CancellationToken` as well, which makes
teardown work even against a receiver that ignores cancellation entirely. That is a strictly
stronger property, but it has one visible consequence: a receiver that would have returned an
error on cancellation may now not run its error path, so the `event stream: <err>` log line does
not appear for a clean stop. That log is the only current visibility into stream teardown and
becomes user-visible once P1 lands the logger ring.

**Nothing drives idle eviction yet.** `RobotRegistry::evict_idle(now)` is pure, in the slice and
table-tested at the 300 second rule, but the background sweeper task that calls it is Tier C. Until
P4 adds the task, a connected robot entry and its gRPC channel live for the process lifetime. The
exposure is bounded by the number of distinct serials ever requested, which is small, but it must
not survive into the P10 soak. The asymmetry that `/cam-stream` never resets the idle timer while
every `/api-sdk/*` request does is implemented and tested now, so the rule is pinned even though
nothing acts on it.

**Shortest float digits round half to even.** This is a parity item rather than a difference, and
it is recorded because it is invisible until it is wrong. When a value sits exactly halfway
between the two shortest digit strings of a given width, both round-trip, and Go's `strconv`
takes the one whose last digit is even (`strconv/ftoaryu.go`, `ryuDigits32`) while Rust's
shortest formatter takes the larger one. `go_format_f32` and `go_json_f64` reproduce Go's choice,
and the rule is pinned by the `math.Float32frombits` and `math.Float64frombits` cases in the
gofmt probe, which are written as bit patterns because a decimal literal for them would beg the
question.

---

## Not yet verifiable

These are open questions rather than deviations. Nothing in the slice can settle them, because
they need either a live robot, a listener on a privileged port, or a CI run on a runner this work
never pushes to.

1. The rustls handshake and the mDNS registration against the real robot. Both require binding
   443 and 5353, which means stopping the production Go server and taking the robot offline for
   about five minutes. `RUNBOOK-S1.md` is the procedure.
2. Whether `ProtocolVersion(client_version = 5, min_host_version = 0)` answers `SUCCESS` or
   `UNSUPPORTED` on this robot. The slice does not care, because the verdict is discarded by
   construction, but the answer is unknown.
3. The roughly 14 millisecond round trip that motivated choosing `ProtocolVersion` over
   `BatteryState` for the probe. It comes from the Go source comment, not from a measurement made
   during this work.
4. Whether the 500 millisecond camera settle is long enough on real hardware. None of the four Go
   commit messages justifies the value. The Rust handoff tests run with a zero settle, so a settle
   that is too short would pass every test and still fail on the robot.
5. Whether the MJPEG framing renders in the browser once the `/cam-stream` route lands. The body
   can never be byte-compared against Go, because Go decodes and re-encodes every frame with its
   own quantisation tables.
6. Jdoc hash parity against the live Go-produced hash for ESN 00303f28. That is P1's critical
   gate, and the slice touches no hashing at all.
7. The Ubuntu CI run. The workflow triggers only on push and pull request, and nothing in this
   work is pushed, so the Linux half of the matrix stays unverified until the user asks for a
   push.
