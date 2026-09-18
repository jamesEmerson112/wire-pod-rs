# Recorded deviations from the Go server

Every deliberate difference between the early Rust SDK-app slice and the Go server at
`origin/main` (`81fa3b3`). The project rule is that vendored things stay byte-identical and that
behavioural differences are documented, never silent, so anything not listed here is a bug.

Each entry gives what differs, why, and where it is tested or otherwise recorded. Go citations
are `path:line` under `E:/GitHub/wire-pod/chipper/`, which is where the Go checkout lives
since the repositories moved to `E:/GitHub`.

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

**Where tested.** Three tests hold the three parts of this, because the factory and the registry
own different parts of a connect. `crates/wirepod-vector/tests/loopback.rs::connect_issues_no_rpc`
asserts that building the connection issues no RPC at all, which is where the dead stream would
have been opened if the port had reproduced it. The liveness call belongs to the registry, so the
`BatteryState`-and-nothing-else property is
`crates/wirepod-core/tests/registry.rs::a_connect_issues_only_the_liveness_call_and_caches_the_entry`,
which asserts the recorded calls are exactly `[BatteryState]`. The connection id `wirepod` is
pinned by `loopback.rs::the_event_stream_request_is_the_stim_shape` on the stream that
`begin_event_stream` does open, so the two streams cannot be confused later.

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
(`webserver.go:32-74`); the slice serves `get_bot_status` and stubs the other 21 at 404. A bare
`grep -n 'case "'` on that file returns 25 lines, but three of them are the `level` switch inside
`handleGetLogsJSON` (`webserver.go:292`, `:294`, `:296`) and are not routes; see
`sdkapp-routes.md`.

The deferred `/api/*` arms, in the order they appear in `pkg/wirepod/config-ws/webserver.go`:

| Route | Go line |
|---|---|
| `/api/add_custom_intent` | 32 |
| `/api/edit_custom_intent` | 34 |
| `/api/get_custom_intents_json` | 36 |
| `/api/remove_custom_intent` | 38 |
| `/api/set_weather_api` | 40 |
| `/api/get_weather_api` | 42 |
| `/api/set_kg_api` | 44 |
| `/api/get_kg_api` | 46 |
| `/api/set_stt_info` | 48 |
| `/api/get_download_status` | 50 |
| `/api/get_stt_info` | 52 |
| `/api/get_config` | 54 |
| `/api/get_logs` | 56 |
| `/api/get_debug_logs` | 58 |
| `/api/get_logs_json` | 60 |
| `/api/is_running` | 64 |
| `/api/delete_chats` | 66 |
| `/api/get_ota` | 68 |
| `/api/get_version_info` | 70 |
| `/api/generate_certs` | 72 |
| `/api/is_api_v3` | 74 |

`/api/is_running` is worth calling out by name. It answers the literal `true` and is the health
probe every runbook in this repo uses, so it will answer 404 against the Rust server until the
`/api/*` routes land.

Three more Go routes are registered outside both prefixes and are not carried by the Rust router
at all, so each of them reaches the file-server fallback rather than the handler Go has:

- `/sdk-app` (`server.go:811`), the `sdkapp` file server. Its 404 is observably not the one the
  Rust fallback answers: the `sdkapp` copy of `DisableCachingAndSniffing` sets three headers
  rather than four (`server.go:791-798`), and `serveError` then deletes `Cache-Control`, so the
  live response carries `Content-Type`, `Pragma` and `X-Content-Type-Options` and **no**
  `Expires`, where the Rust fallback adds `Expires: 0`. The trailing-slash form `/sdk-app/` misses
  the exact pattern, falls through to the web root, and does carry `Expires`.
- `/session-certs/` (`webserver.go:429`), the certificate handler, a subtree pattern.
- `/cam-stream` (`server.go:818`). It is not an `/api-sdk/*` arm; the route, the multipart framing
  and the JPEG re-encode are Tier C, and the ownership, operation lock, settle and byte meter the
  route needs are all in the slice already. See `camstream.md`.

No test asserts any of the three, for the same reason no test asserts a deferred arm: a stub is
not a contract.

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
the zero result and that the map did not grow. `RobotRegistry::meters_len` exists so that
`crates/wirepod-core/tests/registry.rs::reading_a_meter_never_creates_one` can assert the same
thing through the registry, which is the type the handlers actually hold.

---

## 8. `inhibitCreation` is replaced by per-ESN connect locks

**Go.** A single package-level `inhibitCreation` flag serialises robot creation across every
serial. `getRobot` spins on it at `robot.go:407-412`, so a slow or hanging dial for one robot
stalls every `/api-sdk/*` request for every robot. The same flag also spans the whole of
`removeRobot`, set at `robot.go:456` and cleared at `robot.go:479` with the three second settle in
between, so a removal stalls every lookup for every serial for those three seconds and the
resuming caller then finds the robot gone and dials afresh.

**Rust.** A map of per-ESN `tokio::sync::Mutex` connect locks, with the inner lock held across the
dial. The same serial dials once; robot A's dial never blocks robot B.
`RobotRegistry::disconnect` takes that same lock for the whole removal, so both halves of the Go
flag are reproduced per serial rather than globally.

**Why.** It preserves the property the global flag was reaching for, which is that one serial does
not dial twice concurrently and is not handed the entry a removal is tearing down, while removing a
global stall that the Go source itself works around elsewhere. Go commit `255a737` made the camera
operation lock per-ESN for exactly this reason, so this is consistent with the direction the Go
code was already moving.

**What differs on the removal half.** Three things, all deliberate. A removal stalls only requests
for the serial being removed, where Go stalls every serial. The entry leaves the directory at the
start of the removal rather than at the end, so the stalled caller misses on its own peek and
dials, which is the state Go's caller resumes into; the difference is only visible in the window
where Go's caller is blocked and can see nothing at all. And a caller that had already taken the
entry before the removal started keeps its `Arc` and its live connection, exactly as a Go handler
holding the `Robot` value `getRobot` returned keeps its `*vector.Vector`.

**And one thing the flag does not do at all.** `inhibitCreation` is a plain `bool` that `getRobot`
spins on rather than a lock (`robot.go:407-412`), and the only writer that raises it is `newRobot`
itself, on its first line (`robot.go:325`). Two requests for one serial that both clear the spin
before either reaches that line both scan the slice and find nothing (`robot.go:413-417`), and both
append (`robot.go:393-396`), so the slice holds the same ESN twice. `removeRobot` then walks the
whole slice and pays the three second settle once per match (`robot.go:458-475`), so the
disconnect for that serial costs six seconds and drops both entries. `RobotRegistry` is keyed by
`Esn`, so it holds at most one entry per serial and always pays exactly one settle.

**Where tested.** `crates/wirepod-core/tests/registry.rs` asserts that a second request for the
same serial waits for the first dial and reuses its connection, that a request for a different
serial does not wait, and, in
`a_request_during_a_disconnect_waits_and_dials_a_fresh_connection`, that a request arriving inside
a removal waits it out and then dials a fresh connection rather than being handed the entry being
torn down.

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
failure or an expiry it hands the feed back through the guard's own `finish`, which runs the
generation-checked release and issues the disable, and returns the error. A start that returns an
error leaves no owner behind and hands out no `CamGuard`.

**Why.** Releasing on the error path reaches the same end state Go reaches through its defer, one
step earlier. The guard's `Drop` would reach it too, because the guard is a local of
`start_cam_stream` and is dropped either way, but going through `finish` explicitly is what keeps
the release and the disable inside a single hold of the camera operation lock, which the drop path
cannot manage. The release is generation-checked, so a start whose enable failed after a
replacement already took the feed changes nothing and issues no disable.

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

## 15. The camera guard cleans up when it is dropped

**Go.** `camStreamHandler` runs in a goroutine, and a goroutine always runs to completion. Its
`defer finishCamStream(robotObj, gen)` at `pkg/wirepod/sdkapp/server.go:737` therefore always
runs, whatever the handler returns through. A browser that goes away cancels `r.Context()`, which
is what ends the frame loop, but it does not stop the goroutine, so the cleanup still happens.

**Rust.** An axum handler is a future, and a future is dropped outright when the request is
aborted. `CamGuard` therefore does its cleanup from `Drop` as well as from `finish`. The drop
performs the same generation-checked release, synchronously, and if that release succeeded it
spawns a task that takes the camera operation lock and issues `EnableImageStreaming(false)` on the
same `timings.enable` deadline. With no runtime available to spawn onto it logs a warning and
stops. `finish` remains the explicit path and the guard remains `#[must_use]`.

**Why.** Without it, a handler future dropped between the claim and the enable leaves the feed
claimed by a generation nobody holds, with a cancellation token nothing will ever cancel and a
robot whose camera may stay on. The next claim displaces the stale entry, so the state is
recoverable, but until then the robot reports as streaming and burns power for nothing. The guard
is built immediately after the claim for the same reason: the settle and the enable are both
awaits, and a guard constructed after them cannot give back a claim taken before them.

**Consequence to remember.** The drop path releases outside the operation lock, because `Drop`
cannot await one, so the release and the disable are not the single atomic step that `finish`
makes them. The spawned disable closes that gap rather than living with it: it takes the operation
lock and re-reads ownership before issuing anything, so a replacement that claimed while the
disable was still queued keeps its camera, and a replacement that claims later queues on the same
lock and turns the camera back on afterwards. Either order ends with the camera on for whoever
owns the feed. What the split release still costs is a brief moment where the feed reads as
unclaimed while the previous owner's camera is still on, which the explicit `finish` never shows.

A second consequence lands on the failed-enable path in deviation 13. That path now hands the feed
back through `finish`, so a robot that answers neither the enable nor the disable costs two
`timings.enable` deadlines before the handler answers, where Go answers as soon as `CameraFeed`
returns an error (`server.go:743-746`) and lets its deferred cleanup run afterwards. That is
accepted for this slice: the deadline is five seconds, both calls go to a robot that has already
stopped answering, and nothing else is waiting on the handler.

**Where tested.** `crates/wirepod-core/tests/cam_ownership.rs` drops a start inside the settle,
drops a start parked inside the enable, and drops a live guard. Each case asserts that the claim
is gone synchronously and that the camera log ends with a `false` once the spawned disable has
run. A fourth case claims with a new owner in the window the drop opens and asserts the camera log
never records a `false` after that claim.

---

## 16. `disconnect` turns a still-claimed camera off after the settle

Go's `removeRobot` (`robot.go:455-480`) cancels the camera and stim contexts, sleeps three
seconds and drops the entry. It never sends `EnableImageStreaming(false)` itself: the disable is
left to the departing `/cam-stream` handler, whose deferred `finishCamStream` runs because a
goroutine always runs to completion. If that handler is already gone, nothing turns the camera
off and the robot streams to nobody until the next claim.

The Rust registry's `disconnect` follows Go's order, and then, after the settle, issues one
best-effort `enable_image_streaming(false)` when the owner it stopped still holds the feed. The
check is `cam.current()` against the generation read before the settle, and it runs under the
robot's camera operation lock, so the disable is ordered against `start_cam_stream` and
`CamGuard::finish` by the lock Go names at `robot.go:50-55`. That makes it the disconnect-path
analogue of `finishCamStream` (`server.go:700-707`): a `/cam-stream` request that claims the feed
during the three second settle keeps its camera, where an unconditional disable would switch it
off underneath a live owner and stall the viewer's image with no error. The call is bounded by
`timings.enable` and its result is discarded, which is how Go treats the result of its own enable
at `server.go:673-678`. The effect on the wire is one extra RPC, and only in the case where Go
would have left the camera on.

**Where tested.** Three registry tests.
`disconnect_stops_both_streams_pays_the_settle_and_keeps_the_meter` pins that the disable is sent
for a feed the disconnect stopped, `disconnect_leaves_a_camera_claimed_during_its_settle_alone`
pins that a claim landing inside the settle keeps its camera, and
`disconnecting_a_robot_whose_camera_was_never_claimed_sends_nothing` pins that a robot whose
camera was never opened sees nothing on the wire but its connect-time liveness call.

---

## 17. `serde_json` does not escape `<`, `>` and `&` where Go's `json.Marshal` does

**Go.** `encoding/json` escapes those three characters inside strings unless the caller turns it
off with `Encoder.SetEscapeHTML(false)`, which nothing in wire-pod does. Both writers on this
surface therefore escape: `json.Marshal(vars.BotInfo)` for `/api-sdk/get_sdk_info`
(`server.go:190`) and `json.NewEncoder(w).Encode` for `/api/get_bot_status`
(`webserver.go:308`). Verified by running the two calls against the string `a<b>c&d`, which both
render as `a\u003cb\u003ec\u0026d`.

**Rust.** `serde_json` has no such option and emits those characters literally, so a string
carrying one of them produces a body that differs from Go's byte for byte.

**Why it is accepted.** Nothing that reaches either body can contain them. `/api-sdk/get_sdk_info`
serialises a serial, an IPv4 address, a base64 GUID and a boolean; base64's alphabet is
`A-Za-z0-9+/=`. `/api/get_bot_status` serialises a serial, an IPv4 address, one of three fixed
status words and an integer. Only a hand-edited bot-info file could produce a difference, and the
half that matters, the GUID the robot authenticates with, would already be invalid.

**Consequence to remember.** Any later route that serialises free text, a robot name or an LLM
reply through `serde_json` will differ from Go on those three characters. The fix, if one is ever
needed, is a custom `serde_json::ser::Formatter`, not a post-hoc string replace.

**Where recorded.** Here. No test pins it, because pinning it would mean asserting the difference
rather than the contract.

---

## 18. Two residuals in the mux path canonicalisation

**Go.** `findHandler` canonicalises the request path before any pattern is considered
(`net/http/server.go:2660-2699`). It cleans the escaped path with `cleanPath`, answers a 301 to
the cleaned path when that changed it, and matches with each segment unescaped, because the
routing tree's `firstSegment` calls `pathUnescape` (`net/http/routing_tree.go:205-215`). The
trailing-slash redirect runs ahead of the cleaned-path one and builds its target from the
**decoded** path, `cleanPath(u.Path) + "/"` (`server.go:2734`).

**Rust.** `crates/wirepod-server/src/mux.rs` reproduces both steps and `router::canonicalise`
runs them as a layer in front of the route table. Two things are not reproduced exactly.

1. The trailing-slash redirect target is built from the cleaned **escaped** path rather than the
   cleaned decoded path. The two differ only for a request that both needs cleaning and carries
   an escape in its prefix segment, such as `//api%2Dsdk`, where Go answers
   `Location: /api-sdk/` and Rust answers `Location: /api%2Dsdk`. The client takes one extra hop
   and lands in the same place.
2. A segment whose decoded form carries a byte that cannot be written back into a URI path stays
   escaped through both the match and the handler dispatch. That covers a decoded `/`, `?`, `#`,
   `%`, a space, a control byte and any non-ASCII byte. For a segment that has to match a
   pattern, both servers answer the same 404, because no pattern this server registers contains
   such a byte; `GET /ok%2F80` was probed live and answers the file-server 404 on both. For a
   segment that only has to reach a dispatch `switch`, Go compares the decoded form and Rust
   compares the escaped form, so a route name containing one of those bytes would reach Go's arm
   and not Rust's. No route name on either prefix contains one.

**Why.** Rewriting the request URI is what puts the decoded path in front of the route table,
`fallback` and both dispatch switches at once, and a `http::Uri` cannot hold a path byte that
would change how the path splits into segments or that its parser rejects. Refusing the segment
leaves it in the form it arrived in, which is the same thing Go's own `pathUnescape` does for a
segment it cannot decode. Chasing the first residual would mean carrying a second, decoded copy
of the path through the middleware for a redirect target no client asks for.

**Where tested.** `crates/wirepod-server/tests/routing.rs`:
`a_path_is_matched_and_dispatched_by_its_unescaped_form` and
`a_path_that_needs_cleaning_is_a_301_to_the_cleaned_path` pin the reproduced behaviour against
fourteen live Go responses, including the `%2F` case that both residuals leave alone. The unit
tests in `mux.rs` pin `clean` and `unescape_segments` directly. Nothing asserts either residual,
because asserting it would build the difference into the type.

---

## 19. An abandoned `net_probe` writes nothing where Go writes a `Canceled` body

**Go.** The probe deadline is `context.WithTimeout(r.Context(), npTimeout)` (`server.go:99`), so it
is a child of the request context and a client that closes its connection cancels the RPC as surely
as the five second deadline does. `ProtocolVersion` then returns
`rpc error: code = Canceled desc = context canceled`, the handler takes its error branch, and
`fmt.Fprint(w, "error: "+err.Error())` at `server.go:112` writes that string to a `ResponseWriter`
whose connection is already gone. The write is discarded by `net/http`; the handler goroutine still
runs it.

**Rust.** Axum drops the handler future when the request is aborted, and the probe awaits the RPC
inline rather than spawning it, so the dropped future drops
`tokio::time::timeout(state.timings().probe, entry.conn.protocol_version(..))` and the RPC future
with it. Nothing runs after that point, so no error string is produced and no body is written.

**Why it is accepted.** The difference is only observable from inside the process. The one client
this affects has already gone away, so neither body reaches anybody, and the Go one is written into
a discarded buffer. Reproducing it would mean either catching the drop, which a future cannot do
from the inside, or spawning the RPC so it outlives the handler, which is the opposite of what is
wanted: an abandoned probe should stop costing the robot an RPC, not keep one in flight.

**Two related shapes that are not deviations.** The deadline itself comes from `Timings::probe` on
`AppState` rather than from a `npTimeout` constant, and the default is Go's five seconds, so the
wire behaviour is the same and a test can drive the lost-probe body without waiting for it. The
elapsed time is measured against the injectable `Clock` rather than `Instant::elapsed`, and the
production clock is `SystemClock`, which is `Instant` underneath; the injection is what lets a test
move a `ManualClock` across a round trip held open by a gate and assert `13.482` exactly.

**Where tested.** `crates/wirepod-server/tests/sdk_api.rs`:
`a_probe_abandoned_by_its_client_leaves_no_second_call_behind` parks a request inside the RPC,
aborts its task, and asserts the join handle reports cancellation, that the robot recorded exactly
one `ProtocolVersion`, and that the next probe answers normally. Nothing asserts the absent body,
because asserting it would be asserting the difference rather than the contract.

The reason this difference is accepted is a property of its own, and
`an_abandoned_probe_leaves_no_rpc_in_flight` is what holds it. The fake robot's call log cannot
tell a dropped round trip from one that is still parked, because the call is recorded before it
parks, so that test watches the strong count of the `Arc<dyn RobotConn>` instead and requires it
back at its settled value once the handler's task is gone. A probe that had been spawned rather
than awaited inline would still be holding the owned reference `tokio::spawn` forces it to take.

---

## 20. A stream setup abandoned by a stop logs nothing where Go logs `context canceled`

**Go.** `begin_event_stream` opens the stream on `streamCtx`, a cancellable child of the robot
context (`server.go:472`, `server.go:484`). A `stop_event_stream` that lands while the goroutine is
still inside `EventStream` cancels that context, the call fails, and the goroutine takes its error
branch: `logger.Println("event stream: " + err.Error())` followed by
`releaseEventStream(robotObj.ESN, gen)` (`server.go:496-500`). The log line reads
`event stream: rpc error: code = Canceled desc = context canceled`.

**Rust.** `open_event_stream` takes no token, so the spawned task selects on the
`CancellationToken` and the open together, biased towards the token. A stop that lands during the
open therefore abandons it and the task returns through a third arm that releases the claim without
logging anything. The claim is still handed back, generation checked, so the state machine ends in
the same place Go's does.

**Why it is accepted.** This is the same family as the recorded difference that `run_event_stream`
selects on the token: cancellation in this port is a token rather than an error the callee reports,
so the teardown paths that Go can only observe as a failed call have no error to log. The
alternative, letting the open run to completion so it can fail on its own, would leave a dial to an
unresponsive robot in flight after the stop that was meant to end it, which is worse than a missing
log line. The visible consequence arrives with P1's logger ring, where a clean stop will show no
`event stream:` entry at all.

**Where tested.** The reachable half is.
`crates/wirepod-server/tests/sdk_api.rs`'s `a_stream_setup_failure_never_reaches_the_body` pins that
a genuine setup error is logged rather than written to the body and that the claim is handed back,
and `a_begin_straight_after_a_stop_is_admitted` pins that a stop frees the stream for the next
begin. The abandoned-open arm itself has no test: reaching it needs an open held across a stop, and
asserting the outcome would be asserting the absence of a log line.

---

## 21. `disconnect` stops no per-robot timer, because there is none to stop

**Go.** Every connect spawns one `connTimer` goroutine per robot, addressed by the robot's
position in the `robots` slice (`pkg/wirepod/sdkapp/robot.go:399`). `removeRobot` with the source
`"server"`, which is exactly what `/api-sdk/disconnect` passes (`server.go:606`), appends that
position to a package-level `timerStopIndexes` (`robot.go:461-464`); the goroutine notices its own
index in that list on its next one second tick, removes it again and returns
(`robot.go:433-445`). The whole mechanism is by position, and `removeRobot` rebuilds the slice by
filtering (`robot.go:457-460`), so every surviving robot's goroutine keeps the position it was
started with while the slice under it has shifted. Its range guard runs once before the loop
(`robot.go:425-427`) and never again, so after a removal a surviving goroutine reads
`robots[ind].ConnTimer` and calls `removeRobot(robots[ind].ESN, "connTimer")` (`robot.go:446-448`)
against whichever robot now occupies that position, or past the end of the slice. The stop list is
matched by position too, so a robot appended at a freed index can have its timer stopped by an
entry left behind for the robot that vacated it.

**Rust.** There is no per-robot task and no index. The idle rule is
`RobotRegistry::idle_candidates`, a pure function over each entry's `last_touch`, and the
directory is keyed by `Esn`. `RobotRegistry::disconnect` therefore removes the entry and stops the
two streams, and has nothing else to stop.

**Why.** Keying on the serial rather than on a slice position is what makes the removal
self-contained, and it is the same decision that made the connect lock per-ESN (deviation 8). It
removes the three failure modes above rather than reproducing them, which is safe because none of
them is a behaviour anything can depend on: a robot dropped by the wrong timer, or a timer
stopped for the wrong robot, is a reconnect on the next request either way, and the out-of-range
read is a panic.

**What is still missing.** The sweeper that will call `evict_idle` is Tier C and does not exist
yet, so nothing acts on the idle rule at all; that is already recorded under "Additional recorded
differences". Once it lands it must stay keyed by serial.

**Where tested.** `crates/wirepod-core/tests/registry.rs` covers the idle rule and the disconnect
independently of any timer. At the HTTP layer,
`crates/wirepod-server/tests/lifecycle.rs::disconnect_then_conn_test_dials_a_fresh_connection`
pins that a disconnected robot reconnects on the next request, which is the only part of Go's
timer bookkeeping a client can observe.

---

## 22. `disconnect` does not clear `BcAssumption`, because the flag does not exist yet

**Go.** `removeRobot` sets `robots[ind].BcAssumption = false` between stopping the two streams and
sleeping the settle (`robot.go:471`). The flag is set by `/api-sdk/assume_behavior_control`
(`bcassume.go:32`), cleared by `/api-sdk/release_behavior_control` (`server.go:330`), and polled
every 500 ms by the goroutine that holds the `BehaviorControl` stream open (`bcassume.go:80-88`),
which is how that stream is told to let go. Clearing it inside `removeRobot` is what stops a
behaviour-control stream from surviving a disconnect.

**Rust.** `RobotEntry` has no such field and `RobotRegistry::disconnect` clears nothing. Both
behaviour-control routes are deferred to P4 and answer the stub 404 (deviation 6), so nothing
sets the flag and there is nothing to clear.

**Why.** The flag is only meaningful next to the stream it controls, and inventing a field for a
route that is not in the slice would be state with no writer and no reader.

**Consequence to remember.** This is a note for whoever lands
`assume_behavior_control`. Whatever holds that claim has to be released by
`RobotRegistry::disconnect`, in the same place the two stream stops are, or a `BehaviorControl`
stream outlives the disconnect that was supposed to end it and the robot stays under SDK control
with no connection behind it. Go's unguarded `bool` is not the shape to copy; the two stream
owners in `robot/session.rs` are, since both already cancel as well as clearing a flag.

**Where recorded.** Here. No test pins it, because the routes that would set the flag are not in
the slice.

---

## 23. A failed dial reads differently after `desc = `

**Go.** Nothing dials at connect time. `newRobot` builds the client through the SDK's
`vector.New`, which hands the target to hugh's `Client.Connect`, and that calls `grpc.Dial` with no
`WithBlock`
(`github.com/digital-dream-labs/hugh@v0.0.0-20210210154335-f4159b9fcd5f/grpc/client/client.go:87`).
The dial is therefore lazy and an unreachable robot surfaces at the first RPC, which is the
`BatteryState` liveness check on the next line of `newRobot` (`robot.go:365-369`). What that call
returns is grpc-go's own text rather than anything wire-pod writes: the pick fails and is wrapped
as `status.Error(codes.Unavailable, err.Error())`
(`google.golang.org/grpc@v1.82.1/picker_wrapper.go:176`), the error inside it is the balancer's
last `transport.ConnectionError`, which prints as `connection error: desc = %q`
(`google.golang.org/grpc@v1.82.1/internal/transport/transport.go:697-699`), and the quoted string
is `transport: Error while dialing: ` followed by the dial error
(`google.golang.org/grpc@v1.82.1/internal/transport/http2_client.go:230`; the lowercase variant on
line 228 needs `FailOnNonTempDialError`, which nothing here sets). On Windows the dial error itself
is `dial tcp <addr>: connectex: ` plus the OS sentence, because `net` wraps a failed connect as the
`connectex` syscall error (`C:/Program Files/Go/src/net/fd_windows.go:155`).

**Rust.** `TonicConnFactory::connect` dials eagerly and maps the failure through `dial_error`
(`crates/wirepod-vector/src/error.rs:62-70`), which reports `Unavailable` and builds the
description by walking `tonic::transport::Error`'s `source` chain and joining it with `": "`. The
chain is walked because `tonic::transport::Error` alone renders as the useless `transport error`
and only the causes underneath it name the refusal. Against a closed port on this machine the body
reads `error: rpc error: code = Unavailable desc = transport error: tcp connect error: tcp connect
error: No connection could be made because the target machine actively refused it. (os error
10061)`, where the Go server would have written `error: rpc error: code = Unavailable desc =
connection error: desc = "transport: Error while dialing: dial tcp <addr>: connectex: No connection
could be made because the target machine actively refused it."`.

**Why.** The half that matters is identical. Both sides answer HTTP 200 with the `error: ` prefix
(`server.go:61-62`) and both carry the code prefix `rpc error: code = Unavailable desc = ` exactly,
which is what `ConnError`'s `Display` exists to guarantee and what any consumer keying on the
status can read. What differs is the free text after it, and that text is assembled by the runtime
from an operating-system message: grpc-go quotes a `transport: Error while dialing` string, tonic
and hyper produce a `tcp connect error` chain, and the same Windows sentence sits inside both.
Reproducing Go's wording would mean hand-writing grpc-go's framing around a `std::io::Error` and
guessing at how it formats on a platform this port also has to run on, which is a fabricated string
pretending to be a transport's own.

**Who sees it.** The dashboard, verbatim. `assets/webroot/sdkapp/js/vectorbrain.js:876-880` reads
the `net_probe` body as text, strips the `error:` prefix and the whitespace after it, and throws
the remainder as the message; the catch at `vectorbrain.js:1069-1077` stores that message in
`netError` and `renderNet` prints it in the probe row (`vectorbrain.js:1023`). So the difference is
visible to a person watching an unreachable robot, as a differently worded reason, not as a
different verdict.

**Where recorded.** Here. No test pins the wording, deliberately: the string is half operating
system and half runtime, so an assertion on it would fail on the next tonic or hyper release and on
any platform whose `connect` failure reads differently, while proving nothing about the contract.
What is pinned instead is the part that is a contract.
`crates/wirepod-server/tests/sdk_api.rs::a_failing_dial_reaches_the_body_as_the_grpc_status_text`
asserts the body starts with `error: rpc error: code = Unavailable desc = ` and matches the
`ConnError` the fake factory was given, and
`crates/wirepod-vector/tests/loopback.rs::a_failed_call_renders_the_way_grpc_go_prints_it` pins the
same rendering for a status a robot actually returned.

---

## 24. The TLS handshake is rustls, not grpc-go's `crypto/tls`

**Go.** The SDK dials with certificate verification switched off. `vector.New` passes
`client.WithInsecureSkipVerify()`
(`github.com/fforchino/vector-go-sdk@v0.0.0-20231108155304-62168f3595d6/pkg/vector/vector.go:44`),
hugh turns that into `&tls.Config{InsecureSkipVerify: true}` with no `RootCAs` and no `ServerName`
(`github.com/digital-dream-labs/hugh@v0.0.0-20210210154335-f4159b9fcd5f/grpc/client/client.go:124-128`),
and hands it to `credentials.NewTLS` (`client.go:55-56`). `credentials.NewTLS` then applies its own
defaults: it appends `h2` to `NextProtos`, raises `MinVersion` to TLS 1.2, and, because the caller
set no `CipherSuites`, fills the list with every suite `tls.CipherSuites()` reports minus the ones
RFC 7540 appendix A forbids (`google.golang.org/grpc@v1.82.1/credentials/tls.go:239-255`). The
target is `robot.IPAddress + ":443"` (`pkg/wirepod/sdkapp/robot.go:336`).

**Rust.** `crates/wirepod-vector/src/tls.rs` builds a rustls `ClientConfig` on the ring provider
with a `ServerCertVerifier` that returns `ServerCertVerified::assertion()` for every chain,
`alpn_protocols` set to `h2` alone, and the default protocol versions, which with the `tls12`
feature are TLS 1.2 and TLS 1.3. `TonicConnFactory::insecure_tls` dials `https://<ip>:443` through
`Endpoint::connect_with_connector`, because tonic's own `tls` feature would add
`rustls-native-certs` and its companions to `Cargo.lock` and the slice's lock gate forbids that.

**What is the same.** Certificate verification is off on both sides, for the same unavoidable
reason: Anki signed the robot's certificate with a key nothing on this machine has, so there is no
chain to build and no name to match. The offered ALPN list is `h2` on both sides, hugh having added
nothing for grpc-go to append to. Neither side sends SNI for an IP literal: hugh leaves
`ServerName` empty and Go's `crypto/tls` fills it from the dial target only when that target is not
an IP, while rustls sends no SNI extension for a `ServerName::IpAddress`, which is what
`server_name` resolves a dotted quad to. And the handshake signature is still verified on both
sides. `InsecureSkipVerify` in Go skips chain building and the hostname match and nothing else, so
the accept-all verifier delegates `verify_tls12_signature` and `verify_tls13_signature` back to the
crypto provider rather than asserting them; a verifier that asserted those too would accept a
handshake Go rejects.

**What differs.** The cipher suites offered, and the floor. Go offers Go's list, which is every
non-forbidden suite `crypto/tls` knows including the CBC-mode TLS 1.2 suites, and refuses anything
below TLS 1.2. rustls offers only the suites the ring provider carries, which is AES-GCM and
ChaCha20-Poly1305 over ECDHE and nothing in CBC mode, and refuses anything below TLS 1.2 because
rustls 0.23 implements nothing below it. So the intersection with a peer is narrower than Go's on
TLS 1.2. That matters only if Vector's gateway offers a CBC suite and nothing else, which is not
known and is the reason the live handshake is still on the "not yet verifiable" list. TLS 1.2 is
deliberately left enabled for the same reason.

**Why the verifier is not a bug fix.** Accepting every certificate is what makes the connection
possible at all, and it is what every Vector SDK does. There is no stricter option that still
works: pinning the robot's certificate would need a copy of it per robot, which nothing on disk
has, and the escape-pod certificate under `assets/epod/` is the server's own, not the robot's.

**Where tested.** `crates/wirepod-vector/tests/tls.rs` runs the production connector against a fake
robot serving the vendored `assets/epod/ep.crt`, which is self-signed and trusted by no root store,
and asserts that the handshake completes, that the accepted chain is that certificate, that ALPN
settles on `h2`, and that a full `BatteryState` round trip works through the resulting channel. A
TLS 1.2 only fake proves the 1.2 path, and a dial to a closed port proves the failure still maps to
`Unavailable` with the status prefix deviation 23 describes. The unit tests in
`crates/wirepod-vector/src/tls.rs` pin the ALPN list, the verifier's acceptance, and that it still
reports the provider's signature schemes.

---

## Reserved for Phase 1

Phase 1 reserves the numbers 25 through 42, one per decision taken when the phase was planned.
The full text of each belongs to commit C23, which writes the entries proper; until then this
list is what keeps a source file that cites one of these numbers pointing at something. A line
marked **landed** describes code that is already on `master`, and the rest describe work the
phase has not reached yet.

25. mDNS registration goes through `mdns-sd` rather than Go's `kercre123/zeroconf`. The record on
    the wire is identical and only the library's own timing differs.
26. The TLS listener advertises ALPN `h2` and `http/1.1`, and the plain ports accept h2c by prior
    knowledge, with `--alpn off` as the escape hatch back to Go's no-ALPN behaviour.
27. Every port is bound on `0.0.0.0` and on `[::]` without taking `socket2`, and a failure to bind
    the IPv6 socket is a warning rather than a fatal error.
28. The JWT signature slot carries CSPRNG bytes rather than an RS512 signature, because the robot
    parses the token with `ParseUnverified` and no peer ever receives a key.
29. **Landed.** Every state file is written as a temporary file beside the target followed by a
    rename over it, inside `spawn_blocking`, where Go truncates in place with `os.WriteFile`.
    There is one write per mutation, where two of Go's four jdocs callers write the same bytes
    twice. The mode is a parameter because Go's writers of one file disagree about it, and like
    Go's `O_CREATE` open it is applied only when the file is created, carried over from an
    existing file on Unix and ignored on Windows. Since C8 the writes to one file are serialised
    by a write gate that marshals inside the turn, so the file can never hold a state the
    in-memory list has already moved past. Implemented in
    `crates/wirepod-core/src/persist.rs` and tested by `crates/wirepod-core/tests/persist.rs` and
    `crates/wirepod-core/tests/jdocs.rs`.
30. An empty `ReadDocsReq.items` no longer indexes element zero, which is one of the licensed Go
    bug fixes.
31. Four other Go panics become log lines: the peer address with no colon, an empty `NamedJdocs`,
    a nil peer, and a nil PEM block.
32. `ReadSessionCerts` logs instead of panicking on a file that is not PEM, and keeps Go's early
    return.
33. The restart path works in IP mode, where Go panics on a nil `serverTwo` and silently does not
    restart.
34. A rebind failure is returned to the HTTP caller that asked for the restart, while a bad key
    pair at boot still exits 1 the way Go does.
35. There is no vestigial hugh listener, and `DDL_RPC_PORT` is read only where `ReadConfig` reads
    it.
36. No session certificate is downloaded from the DDL servers, which are dead, and the same log
    line is written in its place.
37. `ping_jdocs` drops the connection it opened where Go leaks it.
38. **Landed.** Path resolution is explicit and logged, and Go's Linux `os.Getwd` heuristic at
    `vars.go:205-227` is not reproduced, so the caller passes the home directory to `sdk_ini_dir`
    on every platform. Implemented in `crates/wirepod-core/src/paths.rs`, which cites this number
    in its module doc, on `AssetDir` for the reporting half and on `sdk_ini_dir` for the
    heuristic, and tested by `crates/wirepod-core/tests/paths.rs`.
39. The startup path writes log lines Go does not, and writes no plaintext GUID line where Go
    does.
40. `/session-certs/<esn>` is served, and the other twenty-one `/api/*` arms stay stubs for this
    phase.
41. `use_ip` answers an error, because certificate generation is P7 work.
42. **Landed.** The two `float32` configuration fields are rendered by `go_json_f32`, which
    reproduces Go's `float32Encoder` by formatting at 32 bits, so a `top_p` of `0.7` stays `0.7`
    where a 64-bit rendering of the same value would be `0.699999988079071`. Implemented in
    `crates/wirepod-core/src/gofmt.rs` and used by `crates/wirepod-core/src/config.rs`; pinned by
    the `f32json` section of the Go probe recording through
    `crates/wirepod-core/tests/gofmt_f32.rs` and `crates/wirepod-core/tests/config.rs`.

---

## Candidates recorded in commit bodies, to be written by C23

These are differences the Phase 1 commits found, described in the commit body that introduced
them and deliberately left unnumbered there, because numbering is C23's work. They are collected
here so that none is lost between now and then. Nothing in this list has a number yet, and adding
one is C23's decision rather than this file's.

- The base64 decoder is stricter than Go's `StdEncoding`, which skips carriage returns and
  newlines anywhere in its input and discards non-zero bits left in a padded final quantum. The
  direction is one way: a non-canonical spelling Go verifies becomes a `Decode` error here, never
  the reverse, and Go's own encoder never writes such a spelling (`668129b`, `7fcb764`).
- Go's `LogTrayChan`, the sixty-four slot non-blocking channel, is not ported; it belongs with the
  P9 tray shell (`fb3e5ff`).
- Go's `DEBUG_LOGGING` stdout mirror is not ported, because a tracing formatting layer already
  does that job and is the layer that takes the filter (`fb3e5ff`).
- The log component is derived from the tracing target where Go passes it explicitly at every call
  site, and that same target is what decides whether a line reaches the ring at all (`fb3e5ff`).
- Unknown and fork-only keys in `apiConfig.json` survive a read-modify-write where Go's decoder
  drops them; position is not preserved, so a surviving key is written last within its own object
  and the survivors are sorted (`db1ca2a`, `613a90c`).
- `gohome_percent` is an `i32` where Go's `int` is 64 bits, so a value between the two ranges is a
  type error here and an accepted value there (`db1ca2a`, `613a90c`).
- The two configuration failure arms' second log line carries this module's text rather than Go's
  `*fs.PathError` or `*json.UnmarshalTypeError` (`db1ca2a`, `613a90c`).
- Configuration write errors are returned rather than discarded, where Go ignores `os.WriteFile`'s
  result at all three sites (`db1ca2a`, `613a90c`).
- `Env` reads each variable once instead of at each point of use, where Go calls
  `os.Getenv("STT_SERVICE")` four times, so a variable changed mid-boot cannot be seen two ways
  here (`db1ca2a`, `613a90c`).
- A JSON string carrying a lone-surrogate escape is a type error that leaves the field alone,
  where Go's `unquoteBytes` substitutes U+FFFD and stores the result (`613a90c`).
- In the jdocs store, an empty list is written as the literal `null`, which is what Go's nil slice
  marshals to and what every state Go can reach produces; unknown keys survive at both levels
  where Go's decoder drops them, re-serialised in sorted order; and every write hands back its
  error where Go discards it (`aa524a7`, `b93db05`).
- The three removals and the primary walk over the transient token stores log a line and carry on
  where Go indexes past the end of its slice and takes the process down. Nothing is removed either
  way, so the surviving entries match Go's exactly; only the server still running differs. This
  follows the same policy as reserved deviation 31, which does not itself list this panic
  (`84bed48`).
- Go's two session slices are merged into one list. Go appends the certificate and the name
  separately with no lock between them (`token.go:276-278`) and every reader indexes the
  certificate slice with the name slice's index, so two concurrent associations can leave the
  lists a different length and a later reader then takes the wrong certificate or panics. One list
  cannot reach that state (`84bed48`).
- `take_primary_matches` runs every removal, and its log lines, before the caller runs the
  per-match file writes Go interleaves between them (`jdocs/server.go:96-105`), so the removal
  lines come before those lines rather than after. The control flow cannot be split, because which
  element the loop reads next depends on the removal having happened (`84bed48`).
- The whole primary walk runs under one guard, where Go's slices have no lock at all, so a walk
  sees the store as it was when it took the guard and no append can interleave with it. That is
  what makes the skip quirk reproducible, and it narrows the outcomes of a concurrent append from
  Go's open set to two (`84bed48`).
- `EqualFold` over the token stores is `eq_ignore_ascii_case`, as elsewhere in this crate, so Go's
  full Unicode simple case folding is not reproduced. No peer address or serial can reach this
  code carrying a character the two disagree on (`84bed48`).
- `Debug` on the three token-store entry types and on `TokenStores` prints lengths rather than
  GUIDs, hashes and certificate bytes, so a `{:?}` in a handler cannot put a secret into the log
  ring the web UI serves. Go has no equivalent and writes a plaintext GUID line at startup, which
  reserved deviation 39 already covers from the other direction (`84bed48`).
- The JWT signature slot carries 128 CSPRNG bytes rather than an RS512 signature. This is reserved
  deviation 28. Go generates a throwaway 1024-bit RSA key per request (`token.go:266`), keeps
  nothing and publishes no public half, and the robot parses with `ParseUnverified`
  (`vector-cloud/internal/token/identity/identity.go:158`), which is the only parse of a token in
  either tree, so no verifier exists on either side of the wire. 128 bytes is the length such a
  signature has, and they are drawn rather than fixed so that two tokens issued in the same second
  still differ (`6d72056`).
- A failed random draw for the token id or the signature is a returned `RandomError` rather than
  Go's panic in `uuid.New` (`token.go:181`) and discarded `_` at `token.go:266`. This is the C2
  note's rule applied to the second draw site (`6d72056`).
- `write_token_hash` returns the rewrite's error, where Go discards `os.WriteFile`'s result inside
  `WriteJdocs` (`vars.go:317`) and returns nil unconditionally (`6d72056`).
- A marshal failure inside `write_token_hash` is unreachable here rather than survivable: Go logs
  it and then stores the nil bytes as an empty `json_doc` anyway (`token.go:115-119`), which would
  empty the document (`6d72056`).
- Unknown keys inside a `vic.AppTokens` document and inside one client token survive a
  read-modify-write, where Go's decoder drops them; survivors are re-serialised in sorted order.
  This is the same choice C8 made for the jdocs file itself (`6d72056`).
- An empty `ClientTokenManager` is written as `{"client_tokens":null}`, which is what Go's nil
  slice marshals to and the only empty state Go can reach: `WriteTokenHash` declares `var
  tokenJson ClientTokenManager` (`token.go:102`) and only appends to it (`token.go:114`). A `[]`
  put into a `json_doc` by hand therefore comes back out as `null`, which is the one thing the
  round trip does not preserve. Same shape mismatch and same resolution as the jdocs list above
  (`6d72056`).
- The `requestor_id` claim carries the serial exactly as the bot-info file spells it, because
  `token.go:223` concatenates `robot.Esn` verbatim and `StoreBotInfo` wrote it trimmed but never
  lowercased (`botInfoStorer.go:134`). `Requestor::Robot` therefore holds a raw `String` rather
  than an `Esn`, whose `Esn::new` would ASCII-lowercase it. This is parity rather than a
  difference, and it is recorded because the two obvious spellings disagree only for a serial no
  live robot has: every `thing` this machine has seen is lowercase (`6d72056`).
- `write_token_hash`'s existing-document arm is dead. Go looks the document up under the bare
  serial (`token.go:101`) and stores it under `vic:` plus the serial (`token.go:125`), and nothing
  anywhere writes a jdoc under a bare serial, so `jdocExists` is always false, `token.go:103-107`
  always runs, and the decode at `token.go:108` only ever sees the empty string. The port
  reproduces both arms rather than fixing the lookup, because normalising either spelling would
  make the document accumulate and change a file the Go server reads back. One consequence is
  itself unreachable for the same reason: a type error part way through an existing `json_doc`
  leaves the manager empty here where Go keeps whatever decoded before the fault (`6d72056`).
- `pull_jdocs` refuses an answer whose `NamedJdocs` list is empty with an `Internal` error reading
  `robot answered PullJdocs with no documents`, where all three Go call sites index `NamedJdocs[0]`
  unchecked and panic. This is the empty `NamedJdocs` panic reserved 31 already names, recorded
  here because it now has code and a test behind it (`6a05378`).
- `pull_jdocs` refuses an entry whose `doc` field is absent with an `Internal` error naming the
  kind, where Go dereferences the nil pointer. Reserved 31 does not name this panic, so it is a
  candidate of its own rather than part of that entry. Treating the absent document as the default
  one instead would let `AddJdoc` replace a good `vic.RobotSettings` on disk with an empty one and
  log nothing (`6a05378`).
- That absent-document refusal covers every entry, including the ones Go never reads. All three Go
  sites stop at index zero, so a good first document followed by an entry carrying none is usable
  there and refuses the whole pull here. No Go request asks for more than one kind, so nothing in
  the port can reach the difference (`6a05378`).
- A `JdocType` number outside the four reads as `ROBOT_SETTINGS`, which is proto3's zero value and
  what an absent field decodes to. None of the three Go call sites reads `jdoc_type` at all, so no
  caller can observe the choice (`6a05378`).

---

## Additional recorded differences

**`run_event_stream` selects on the cancellation token.** Go's loop relies on the receiver
observing a cancelled context. The Rust loop selects on a `CancellationToken` as well, which makes
teardown work even against a receiver that ignores cancellation entirely. That is a strictly
stronger property, but it has one visible consequence: a receiver that would have returned an
error on cancellation may now not run its error path, so the `event stream: <err>` log line does
not appear for a clean stop. That log is the only current visibility into stream teardown and
becomes user-visible once P1 lands the logger ring.

**The idle boundary is just past 300 seconds, not at it.** `connTimer` zeroes `ConnTimer` and then
loops on "sleep one second, test `ConnTimer >= 300`, increment" (`robot.go:429-452`), so the test
reads 0 one second after the reset and first reads 300 three hundred and one seconds after it. Go
therefore keeps a robot that has been idle for exactly 300 seconds and removes it a second later,
which is why `idle_candidates` compares with `>` rather than `>=`. Go's own boundary drifts later
still, because `time.Sleep(time.Second)` sleeps at least a second and the drift accumulates over
three hundred iterations; the port does not reproduce that drift, so under a per-second sweeper it
removes at the early end of the window Go removes in. The one-second correction was found by an
adversarial review of the C8 commit; the earlier `>=` was a misreading of the check-then-increment
order.

**Nothing drives idle eviction yet.** `RobotRegistry::idle_candidates(now)` is pure, in the slice
and table-tested either side of the boundary, but the background sweeper task that calls
`evict_idle` is Tier C. Until P4 adds the task, a connected robot entry and its gRPC channel live
for the process lifetime. The
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

1. The inbound rustls listener and the mDNS registration against the real robot. Both require
   binding 443 and 5353, which means stopping the production Go server and taking the robot
   offline for about five minutes. `RUNBOOK-S1.md` is the procedure.
2. Settled on 2026-09-09 by the first real-robot trial (`RUNBOOK-SDK-TRIAL.md`):
   `ProtocolVersion(client_version = 5, min_host_version = 0)` answers `SUCCESS` with
   `host_version = 5` on ESN 00303f28. The slice still discards the verdict by construction; the
   answer is only visible on the `wirepod_vector::conn` debug log line.
3. Settled on 2026-09-09. The Go server's `net_probe` measured 8 to 28 milliseconds over thirteen
   probes, and the Rust trial binary measured 19 to 24 milliseconds once the custom TLS connector
   set `TCP_NODELAY` the way Go (`net/tcpsock.go:290`) and tonic's stock connector do. Before that
   fix it measured 51 to 73 milliseconds, which is the Nagle stall between HTTP/2's separate
   HEADERS and DATA writes and the robot's delayed ACK. The Go comment's roughly 14 milliseconds is
   the right order of magnitude.
4. Whether the 500 millisecond camera settle is long enough on real hardware. None of the four Go
   commit messages justifies the value. The Rust handoff tests run with a zero settle, so a settle
   that is too short would pass every test and still fail on the robot.
5. Whether the MJPEG framing renders in the browser once the `/cam-stream` route lands. The body
   can never be byte-compared against Go, because Go decodes and re-encodes every frame with its
   own quantisation tables.
6. Jdoc hash parity against the live Go-produced hash for ESN 00303f28. That is P1's critical
   gate, and the slice touches no hashing at all. Settled on 2026-09-17: the `#[ignore]`d live
   gate `the_live_stored_hash_verifies_against_the_live_guid` in
   `crates/wirepod-core/tests/token_hash.rs` passes on this machine, so the ported hashing
   reproduces the value the Go server wrote and the robot's existing association survives a
   cutover. The gate asserts and prints nothing about what it loads, and it skips when `APPDATA`
   is unset or either file is missing.
7. The Ubuntu CI run for the Rust slice itself. The first green run on both runners was on the
   C3 commit (`5f7170f`, run 34305117622, 2026-09-09), after the toolchain file gained `rustfmt`
   and `clippy`. The core and vector commits after it stay unverified on Linux until they are
   pushed.
8. The outbound TLS handshake against the robot's own gateway. `TonicConnFactory::insecure_tls`
   is proven against a loopback fake serving the escape-pod certificate, which settles the
   verifier, the ALPN and the TLS 1.2 path, but not whether the suites the ring provider offers
   intersect the ones Vector's gateway accepts. Deviation 24 records why the two lists differ.
   Settled on 2026-09-09: the handshake completed against the robot's gateway in the first trial,
   so the ring provider's suites do intersect the gateway's, and every slice route answered over
   that connection. Deviation 24's description of the offered lists stands.
