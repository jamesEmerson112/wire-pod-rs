This file is the approved master plan for the Rust port of the wire-pod `chipper` server. It was written and approved in a planning session and originally lived at `~/.claude/plans/breezy-knitting-cray.md`, outside the repository. It has been copied here unchanged so that project knowledge lives with the project. This copy is now the authoritative one. The home-directory original is kept only as a historical artifact; when the two differ, this file wins.

The plan below is the roadmap at the level of phases, crates, and locked decisions. Detail for each individual phase lives in `docs/phases/`, one document per phase, with an index at `docs/phases/README.md`. The plan text below is unedited except where a statement in it turned out to be factually wrong about the Go source or about a library, in which case the sentence is corrected in place and the correction is recorded in the Amendments section with its evidence. As decisions change during implementation, they are recorded in the `## Amendments` section appended at the very end of this file rather than by rewriting the plan, so the original reasoning stays readable next to what replaced it.

---

# Port wire-pod chipper to Rust (`wire-pod-rs`)

## Context

The user wants a full-parity Rust rewrite of the wire-pod server (`chipper/`, 11,792 LOC Go, zero tests) that serves their Anki Vector robot (ESN 00303f28 @ 192.168.8.203, escape-pod mode, host 192.168.8.226). The robot's firmware is immutable — the external contract must be reproduced exactly. Decisions locked in by the user:

- **Scope:** chipper only. `vector-cloud/` stays Go (chipper never imports it; only coupling is the SSH-onboarding upload of `pod-bot-install.sh` + `vic-cloud`, which has an existing download-from-GitHub fallback).
- **Strategy:** full parity — all 6 STT engines, Lua scripting, a plugin system, BLE onboarding, weather, all gRPC/HTTP surfaces, plus the fork's custom features (battery watchdog, leveled logger, reasoning-model LLM support, early intent ack, stream fixes).
- **Platform:** Windows-first (current deployment is a packaged Windows app), Linux x86_64 + aarch64 (Jetson) soon after.
- **Location:** a NEW repo — `C:\Users\voan2\Documents\GitHub\wire-pod-rs` (git init locally; GitHub push when user asks — never add Claude attribution to commits).

Live environment for verification: Go `chipper.exe` running (ports 80/443/8080/8084), data dir `%APPDATA%\wire-pod`, install dir `C:\Program Files\wire-pod\chipper`, robot holds an ESTABLISHED heartbeat conn to :80. Health probe: `curl http://localhost:8080/api/is_running` → `true`.

## Verified facts that shape the plan (from exploration + live probes)

- **D1:** `epod/ep.key` is RSA-**2048** (not 1028) — rustls serves it fine. The 1028-bit issue only affects *generated IP-mode* certs, which this install doesn't have (**D2:** live config is `epconfig: true`, certs dir has only `server_config.json`).
- **D3:** BLE is behind the `inbuiltble` build tag and is **NOT in the packaged Windows build** — the 17 `/api-ble/*` routes are stubs today. BLE becomes an optional feature, Linux-first, late phase.
- **D4:** The Windows tray/installer/firewall shell is a **separate Go repo** (`C:\Users\voan2\Documents\GitHub\WirePod`, ~900 LOC `cross/podapp` + `cross/win`) — porting chipper alone yields no shippable Windows app. Phase 9 re-implements the shell in Rust; the existing Go installer is kept initially (it only copies files + writes registry/firewall).
- **D5:** The robot speaks **TLS 1.3 and negotiates ALPN h2** (verified via openssl against the bot's gateway) — strong evidence the inbound grpc-go client works against rustls. Still spiked first (P0-S1).
- Protos are NOT in the repo — vendored from Go module cache: `digital-dream-labs/api@v0.0.0-20210824232136` (chipperpb wire package `chippergrpc2`, jdocs, token) and `fforchino/vector-go-sdk@v0.0.0-20231108155304` (12 protos, service `Anki.Vector.external_interface.ExternalInterface`). Both import `google/api/annotations.proto` + `http.proto` — vendor those too.
- Magic constants (byte-exact): appkey `oDoa0quieSeir6goowai7f`, GlobalGUID `tni1TRsTRTaNSapjo0Y+Sw==` + its hardcoded hash doc, BLE auth token `2vMhFgktH3Jrbemm2WHkfGN`, mDNS record (`escapepod` / `_app-proto._tcp` / port 8084 / TXT `txtv=0 lo=1 la=2`), `server_config.json` shape, epod cert/key.
- Token hashing must match Go byte-for-byte (existing jdocs hold Go-produced hashes): GUID = b64(16 rand bytes); hash = b64(SHA256(token‖salt)‖salt), sizes 16/16/32; constant-time compare. JWT: RS512, throwaway RSA key per call (robot never verifies sig; of the seven claims it requires six as JSON strings, namely `token_id`, `token_type`, `user_id`, `requestor_id`, `iat` and `expires`, and reads `permissions` only when it is present and is an object, `vector-cloud/internal/token/identity/token.go:96-161`; RFC3339Nano-ish timestamps).
- Audio: first byte `0x4F` ⇒ Ogg-Opus else raw PCM 16k s16le; 300 Hz single-pole high-pass + gain 5 then 1.5 **per chunk with filter-state reset each chunk** (preserve — audible behavior); WebRTC VAD mode 2, 320-byte/10 ms frames, end-of-speech at `InactiveFrames>=23 && ActiveFrames>18`.
- Ports 80 and 8080 serve the **same mux** (every route on both). No method checking. CORS `*` on `/api/`. `/api/get_kg_api` key-leak is kept as-is (webroot JS prefills the form from it).
- `/api/set_kg_api` decodes the **whole** Knowledge struct — all state structs need `#[serde(default)]` + `#[serde(flatten)] extra` so fork/unknown fields survive round-trips (rollback safety). `battery.gohome_percent` is `Option<i32>` (nil=default 25, 0=disabled) with `skip_serializing_if`.

## New-repo asset strategy

Copy once at repo creation from `wire-pod/chipper/`: `webroot/` (29 MB, byte-identical — the web UI contract), `intent-data/` (14 locales), `weather-map.json`, `epod/ep.crt` + `ep.key`, `stttest.pcm`, and `vector-cloud/pod-bot-install.sh` (into `assets/`). Add `cargo xtask sync-assets --from ../wire-pod` that re-copies + diffs, run during the side-by-side parity period to catch drift. `vic-cloud` binary is fetched via the existing GitHub-download fallback (plus `--vic-cloud-path` override).

Path resolution (explicit, logged at INFO on startup — fixes the Go CWD-dependence):
- AssetRoot: `--asset-dir`/`WIREPOD_ASSET_DIR` → exe dir → repo-relative fallback.
- DataRoot: `--data-dir`/`WIREPOD_DATA_DIR` → `%APPDATA%\wire-pod` (when `--packaged`) → `./`.

## Workspace layout (13 crates)

```
wire-pod-rs/
  xtask/                  # parity harness, sync-assets, packaging
  assets/                 # vendored webroot, intent-data, epod, weather-map.json, stttest.pcm, pod-bot-install.sh
  crates/
    wirepod-proto/        # tonic codegen via protox (no protoc): 3 dd-l + 12 vector-sdk + google/api protos
    wirepod-core/         # AppState, config, paths, logger ring, jdocs/botinfo/session-cert stores, pinger
    wirepod-audio/        # Ogg/Opus decode, high-pass+gain, VAD, SpeechRequest       [pure, golden-tested]
    wirepod-stt/          # SttEngine trait + engines behind Cargo features
    wirepod-intent/       # intent matching, params, words2num, localization          [pure, table-tested]
    wirepod-llm/          # provider switch, reasoning models, sentence splitter
    wirepod-vector/       # outbound robot client (25 RPCs + /v1/update_settings + :8889 consolevar)
    wirepod-plugin-api/   # host/guest ABI types
    wirepod-plugins/      # Extism WASM host + mlua host + exec custom-intents
    wirepod-ttr/          # ConversationTask, weather, battery watchdog
    wirepod-server/       # TLS listener, tonic services, axum router, mDNS, supervisor
    wirepod-setup/        # cert gen, SSH onboarding, BLE (feature "ble")
    wirepod-app/          # bin chipper(.exe): CLI, wiring; Phase 9 adds tray/single-instance
```

Key crate choices: tokio, tonic 0.12 + prost + **protox**, axum 0.8, **hyper-util `auto` builder** (= the cmux replacement: sniffs h2 preface when ALPN absent), tokio-rustls (+ non-default `tls-native` escape hatch), **mdns-sd**, arc-swap (whole-struct config replace), `ogg` + `opus` (vendored libopus — same lib as Go ⇒ bit-identical decode; drops shipped DLLs), `webrtc-vad` (same C source as Go), `vosk` (reuse shipped libvosk.dll), whisper-rs, pv_leopard (official), reqwest(rustls), **mlua lua51+vendored** (gopher-lua is 5.1; Luau rejected — would break user scripts), **extism** (WASM plugins; PDKs let Go plugin authors recompile not rewrite), russh (pure-Rust SSH), btleplug (deferred feature), rsa+rcgen (**generate 2048-bit certs going forward**; ring rejects <2048; preflight ERROR for legacy keys), rust-ini, image/jpeg-encoder, tracing (+custom ring layer), uuid/base64/sha2/subtle/chrono, tray-icon + windows crate (Phase 9). Weak-on-Windows flags: btleplug (deferred per D3), coqui libstt (Linux-only, feature-off, best-effort — project discontinued).

## Core architecture decisions

1. **`Arc<AppState>`** replaces the ~30 unsynchronized `pkg/vars` globals: `ArcSwap` for read-mostly whole-replace data (config, intents, custom intents, certs), `RwLock` for bot_info/jdocs/session_certs, `Mutex` for chats (cap 16, drop oldest 2 at exactly 16) and the peer-IP-keyed assoc stores, an **actor** (`RobotRegistry`) for per-bot SDK connections (lifecycle fits an actor, not a lock). Persistence: synchronous on mutation via `spawn_blocking` + **write-temp + atomic rename** (fixes the live torn-write exposure — `vic.RobotLifetimeStats` is at v313 and climbing).
2. **`ConversationTask`** unifies `kgsim.go` StreamingKGSim + `kgsim_cmds.go` DoGetImage (near-duplicates; fork fixes were only applied to the first): one `select!` loop over an mpsc sentence stream, a `CancellationToken` (touch/wake-word interrupts from EventStream), and a first-token deadline; **RAII `BehaviorControlGuard`** releases behavior control on every path. Dual response path preserved (gRPC intent ack + SDK-driven speech). Fork behaviors pinned by named tests: EOF flush of unpunctuated tails, stream-error unhang, early `intent_greeting_hello` ack for slow reasoning models, custom prompt LAST.
3. **STT:** one binary, `SttEngine` trait returning `Text(String) | Intent(IntentResult)` (Houndify bypasses local matching), engines behind features (`stt-vosk` default), runtime selection via `STT_SERVICE`/config, `--list-stt`. Replaces Go's 6 compile-time binaries.
4. **Plugins:** Extism WASM host, guest ABI mirroring Utterances/Name/Action (`wirepod_manifest()`, `wirepod_action(...)->{intent,speech}`, opt-in host fns log/http/robot), loaded from `<data_dir>/plugins/*.wasm`, `/api/reload_plugins`. Works on Windows for the first time. **Lua stays the primary extensibility path**: fresh `Lua` per execution in `spawn_blocking` + semaphore (gopher-lua's shared-LState was unsafe anyway); ship stdlib subset json/time/strings/base64/filepath/http + loud "unsupported module" stubs.
5. **Supervisor** replaces `RestartServer()`: CancellationToken + `serve_with_shutdown`, **await old task before rebind**, errors returned to the HTTP caller (no `process::exit` in restart path). Drives `/api-chipper/{restart,use_ip,use_ep}`.
6. **TLS listener:** rustls with `alpn_protocols = [h2, http/1.1]` + hyper auto preface-sniffing (superset of today's no-ALPN cmux). One axum Router merging `tonic::service::Routes` + `/ok` + `/ok:80` (test the literal-colon segment vs matchit explicitly; raw-path fallback as escape hatch — it's the liveness heartbeat). Port 8084 duplicate listener in EP mode unless `NO8084`.
7. **Logger:** tracing + custom layer into `ring[500]` of `Entry{t(ms),level,comp,bot,msg}` (JSON tags load-bearing for webroot JS — pinned by test), ANSI stripping, LOG_FILE sink, DEBUG_LOGGING gate, legacy LogList/LogTray shims for `/api/get_logs`.
8. **Bug fixes allowed (not contract):** nil-client on unknown LLM provider (add default arm), `GetActionsFromString` ||-less panic, `DoSayText` discarded sanitize, `ActionNewRequest==ActionPlaySound` collision, jdocs `Items[0]` unchecked. Keep: `/api/get_kg_api` plaintext key (UI depends on it).

## Phases (each ends robot- or side-by-side-testable; Windows-first)

**P0 — Foundations + de-risk spikes (M).** Workspace, 13 crate stubs, vendored protos compiling via protox, CI (windows+ubuntu: build/test/clippy/fmt). Four PASS/FAIL spikes: **S1** rustls+hyper-auto on :443 with epod cert, Go stopped ~5 min — robot conn-check succeeds, ESTABLISHED conn appears (run ALPN-on AND ALPN-off); **S2** `vosk` crate links on Windows against shipped libvosk.dll, transcribes `stttest.pcm` to same text as Go; **S3** opus+webrtc-vad build on Windows, decode/VAD decisions diff clean vs Go dump; **S4** mlua+extism hello-world on MSVC. **Gate: S1 failing both ways ⇒ stop, switch to tls-native before proceeding.**

**P1 — Robot connects & authenticates (L).** Config read/write (byte round-trip vs live apiConfig.json), logger+ring, token svc (3 RPCs), jdocs svc (WriteDoc/ReadDocs incl. the peer-IP association state machine, session-cert writes, botSdkInfo/jdocs/sdk_config.ini), StreamingConnectionCheck, mDNS register+browse, supervisor + `/api-chipper/*`, CLI flags. **Verify:** hash-parity unit test against the live Go-produced hash for ESN 00303f28 (critical); side-by-side on ports 18080/1880/1443; then real cutover test on a copied data dir — robot re-establishes <60 s, `vic.RobotLifetimeStats` version advances past 313, no jdoc regresses; `dns-sd -B _app-proto._tcp` shows escapepod; grpcurl reflection lists 3 services; double-restart via API rebinds cleanly.

**P2 — Voice commands (L).** SpeechRequest (opus sniff, decode, filters with per-chunk state reset, VAD), vosk engine, intent matching (14 locales + custom intents + words2num + param extraction), chipper gRPC streaming handlers. **Verify:** golden diff of post-filter PCM + VAD decisions vs Go (via a throwaway `WIREPOD_DUMP_DIR` branch on the Go side, never merged); table tests over all locale keyphrases; live "set a timer for 10 seconds" etc. works.

**P3 — LLM/KG/weather/commands (XL, largest).** 3a provider layer (openai/together/custom, reasoning-model params, default `gpt-5.6-luna`, RememberedChats); 3b sentence splitter vs recorded SSE transcripts (**gate: green before 3c**); 3c ConversationTask; 3d weather + LLM command tags + DoSayText/TTS routing (OpenAI voice path + native voice). **Verify:** named fork-behavior tests; 10-question live A/B vs Go; behavior-control leak test (kill LLM endpoint mid-stream ⇒ robot autonomous again <5 s); early-ack fires with reasoning_effort=medium; weather-map golden.

**P4 — SDK app + web UI + camera (L).** Full 25-RPC vector client (BehaviorControl bidi, EventStream, CameraFeed, ExternalAudioStreamPlayback 8 kHz/1024 B/60 ms, PullJdocs, faces, photos) + update_settings REST + :8889 wake-word trigger; all 45 `/api-sdk/*` + 25 `/api/*` + `/session-certs/*` + `/cam-stream` (MJPEG q50) + static serving. **Verify:** `xtask parity-http` route-by-route JSON diff vs Go (primary gate); drive the real web UI + robot end-to-end.

**P5 — Watchdog, logger polish, Lua (M).** Battery watchdog (30 s poll, volts→percent curve mirroring `webroot/js/battery.js` — table-tested, 3-low hysteresis, OVERRIDE_BEHAVIORS + silent DriveOnCharger, 10 min cooldown/3 attempts/30 min backoff, dock-cause attribution); Lua host + `/api-lua/run_script`. **Verify:** curve/hysteresis unit tests; live or mocked go-home fires once and cools down; existing user Lua scripts run or fail loudly.

**P6 — WASM plugins (M).** Extism host, ABI, port both sample plugins (whatdate, sdkTest) as guests, `/api/reload_plugins`, PLUGINS.md. **Verify:** samples work on Windows+Linux; panicking plugin is isolated.

**P7 — Setup: certs, SSH, BLE (M).** 2048-bit cert generation + sub-2048 preflight; `server_config.json` writer (byte-exact); russh SSH onboarding using vendored `pod-bot-install.sh` + vic-cloud download fallback; `/api-ssh/*`, `/api/generate_certs`; BLE behind `--features ble`, Linux-first, off by default (parity with D3). **Verify:** onboard a factory-reset/spare bot end-to-end; server_config byte-matches Go's.

**P8 — Remaining STT engines (M).** whisper.cpp → openai-whisper → leopard → houndify (hand-rolled streaming, `SttOutcome::Intent`) → coqui (best-effort Linux FFI; drop + document if unworkable). **Verify:** each transcribes stttest.pcm same as Go; runtime switching works.

**P9 — Packaging (L).** Windows app shell in `wirepod-app` (tray-icon, named-mutex single-instance, crash dump, windowed subsystem, icon/manifest) replacing `WirePod/cross/podapp`+`cross/win`; keep the Go installer initially, point it at the Rust exe, **add UDP 5353 firewall rule**; install layout matches `C:\Program Files\wire-pod\chipper` (can drop libopus-0.dll/libogg/MinGW runtime DLLs ≈29 MB); Linux systemd unit + aarch64 (Jetson) build; Docker. **Verify:** install on a snapshot/spare machine, tray + autostart + `is_running` <30 s; systemd on Linux; Jetson build boots.

**P10 — Cutover + soak (S).** 24 h soak (behavior-control leaks, RSS, mDNS, jdoc monotonicity) with scripted hourly exercise of voice/LLM/camera/watchdog; timestamped data-dir backup before first cutover; Go kept as rollback ≥1 month. Cutover = stop Go chipper.exe, start Rust against the same `%APPDATA%\wire-pod`; robot follows escapepod.local mDNS. Rollback = reverse (schema-safe both directions via serde flatten/round-trip tests).

## Parity test strategy

- `xtask parity-http` (all safe routes, volatile-field normalization, structural JSON diff), `parity-audio` (stttest.pcm goldens), `parity-grpc` (recorded exchange replay).
- Goldens harvested from a throwaway `parity-capture` branch of the Go server (JSON-dumps gRPC exchanges + post-filter PCM under `WIREPOD_DUMP_DIR`); goldens committed under `wire-pod-rs/tests/goldens/`.
- Unit tests for all pure logic (none exist in Go today): token hashing vs live Go hashes (highest-stakes), JWT claim shape, intent tables ×14 locales, sentence splitter (fork fixes), words2num, battery curve, audio filters/VAD goldens, config round-trip, weather map.

## Top risks

| Risk | Sev | Mitigation / early check |
|---|---|---|
| grpc-go client vs rustls handshake | High→reduced (D5) | P0-S1 spike both ALPN modes; tls-native fallback |
| vosk crate Windows linking | High | P0-S2 vs shipped DLL; fallback hand-FFI (small C API) |
| Behavior-control leak bricks robot autonomy | High | RAII guard + leak tests + soak |
| LLM streaming regressions undo fork fixes | High | Named tests from recorded SSE before porting (P3b gate) |
| Windows app shell in separate repo (D4) | High | Scoped into P9 explicitly (~900 LOC) |
| `/ok:80` literal-colon route vs axum matchit | Med | P1 explicit test + raw-path fallback |
| mDNS on Windows firewall | Med | UDP 5353 installer rule + `/api/mdns_status` |
| Coqui discontinued | Low | Feature-off, Linux-only, best-effort, droppable |

## Execution notes

- First implementation session: create `wire-pod-rs` repo, P0 workspace + asset copy + proto vendoring + spikes. Spike S1 needs the Go server stopped ~5 min (robot offline briefly) — do it, then restart Go.
- Never add Claude attribution to any commit in the new repo.
- `.env`/API keys never committed; Rust server reads existing `%APPDATA%\wire-pod\apiConfig.json` (key stays where it is).
- P0–P2 are the make-or-break sequence; P3 is the largest body of work.

## Amendments

Entries are added newest first, each dated, recording a decision that supersedes something in the plan text above.

## Amendments (2026-09-18, P1 token service)

**1. The JWT, as the C11 deep pass settled it.** Four things in the verified-facts line about the
token above are now either corrected or backed by evidence. The signature slot carries 128 bytes
drawn from the operating system's random source rather than an RS512 signature over a throwaway
key, because no verifier exists on either side of this wire: the chipper tree holds exactly one
`jwt.` use and one `SignedString` and no parse at all, and the robot's only parse is
`ParseUnverified` (`vector-cloud/internal/token/identity/identity.go:158`), whose implementation
(`golang-jwt/jwt@v3.2.2/parser.go:96-148`) counts three dot-separated parts and never refers to the
third. That is now numbered deviation 28 in `docs/phases/P4-sdk-app/deviations.md`, with the
recorded `empty_signature_segment` and `garbage_signature_segment` verdicts as its executable
argument. The claim count is corrected in the line above: the robot requires six claims as JSON
strings and reads `permissions` only when it is present and is an object
(`vector-cloud/internal/token/identity/token.go:96-161`), so a seventh required claim was never
there. The timestamps come from three separate `time.Now()` reads per association, at
`token.go:195`, `token.go:196` and `token.go:110`, so `iat`, `expires` and the stored `issued_at`
carry three different sub-second fractions and the port reads its injected clock three times to
match. And the whole bundle was compared against the running Go server rather than argued from the
source: `crates/wirepod-vector/tests/live_token.rs` takes one `RefreshToken` over TLS to
`127.0.0.1:443`, which `GetEsnFromTarget` (`token.go:58-74`) cannot match to any stored robot, so
the call takes the write-free arm at `token.go:231-239` and the modification times of the three
state files are asserted unchanged either side of it. `jsonwebtoken` is dropped from the crate list
above, because nothing in the port signs or verifies a token; `rsa` and `rcgen` stay, since that
pair is for certificate generation in P7. The specification C17 implements the service from is
`docs/phases/P1-robot-connect-auth/token.md`.

## Amendments (2026-09-09, P4 early slice)

These entries record what changed during the early Phase 4 slice, which was planned separately on
2026-09-08 and executed as commits `19fdca4` through `fceaa22` on `master`. Each entry says what
the plan text above said, what was done instead, why, and where the decision is recorded. Go
citations are `path:line` under `C:/Users/voan2/Documents/GitHub/wire-pod/chipper/`, and
`robot.go`, `server.go` and `jdocspinger.go` without a directory mean `pkg/wirepod/sdkapp/`. The
numbered deviations from Go live in `docs/phases/P4-sdk-app/deviations.md`, entries 1 to 23, and
the per-commit record is the execution log kept with the planning notes outside the repository.

**1. axum 0.7, not axum 0.8.** The crate-choice paragraph above names axum 0.8. `tonic 0.12.3`
depends on axum 0.7 and `Routes::into_axum_router()` returns an axum 0.7 `Router`, and the
resolved lock holds only `axum 0.7.9` with `matchit 0.7.3`. Taking axum 0.8 would mean either two
axum versions in the graph or a tonic upgrade, neither of which the slice needed. The server crate
is built on axum 0.7, and the `/ok:80` literal-colon route keeps the S1 spike's router-fallback
trick rather than relying on the matcher. Recorded as decision D1 in
`docs/phases/P4-sdk-app/early-slice-design.md`.

**2. The robot registry is a struct with per-ESN connect locks, not an mpsc actor.** Core
architecture decision 1 above says per-bot SDK connections are an actor because the lifecycle fits
an actor rather than a lock. `RobotRegistry` is instead an `RwLock<HashMap<Esn, Arc<RobotEntry>>>`
plus a map of per-ESN `tokio::sync::Mutex` connect locks, with the inner lock held across the
dial. This replaces Go's package-level `inhibitCreation` flag, which `getRobot` spins on at
`robot.go:407-412` and which therefore stalls every serial while any one serial dials. The Rust
shape keeps the property the flag was reaching for, that one serial never dials twice
concurrently, and drops the global stall. The registry landed in `ad227bd` and the reasoning is
deviation 8.

**3. The removal half of `inhibitCreation` is reproduced per serial too.** The same Go flag spans
the whole of `removeRobot`, raised at `robot.go:456` and cleared at `robot.go:479` with a three
second settle in between, so a removal stalls every lookup for every serial.
`RobotRegistry::disconnect` takes the same per-serial connect lock for the whole removal and
removes the entry before the settle rather than after it, so a caller waiting on that serial
misses on its own peek and dials afresh, which is the state Go's caller resumes into. Because the
directory is keyed by `Esn` it holds at most one entry per serial and always pays exactly one
settle, where Go's slice can hold the same ESN twice and pay the settle once per copy. This half
was added by the C8 adversarial review in `dc15e04` and is the middle of deviation 8.

**4. The robot seam lives in `wirepod-core`, and `wirepod-core` does not depend on
`wirepod-proto`.** The crate table above gives `wirepod-vector` the outbound robot client and says
nothing about where the abstraction over it lives. The slice puts the traits `RobotConn`,
`EventReceiver`, `FrameStream`, `CameraControl`, `FrameSink` and `RobotConnFactory` in
`wirepod-core` in domain types, with `wirepod-vector` depending on core and proto to implement
them and `wirepod-server` depending on both. The dependency direction is `proto <- vector`,
`core <- vector <- server` and `core <- server`. This keeps `AppState` in core as the plan says,
mirrors Go's own narrow `eventReceiver` seam, and is what lets every core and server test run
without a socket. Recorded as decision D2 in `early-slice-design.md`.

**5. The Go server now has eight tests, and they gate this slice.** The plan above says the Go
source is 11,792 LOC with zero tests, and the parity test strategy assumes goldens have to be
harvested because no Go tests exist. That is no longer true for the SDK-app package:
`chipper/pkg/wirepod/sdkapp/sdkapp_test.go` at `origin/main` holds eight tests that pin camera
handoff, the superseded owner's inability to release, single event-stream ownership, prompt
receiver teardown, claim refusal while owned and admission right after a stop, the stim fence, per
robot meters, and exact meter counting under concurrency. All eight were ported to Rust as strict
supersets of their Go assertions, each was confirmed to fail with its guard removed, and they are
mapped one to one in `docs/phases/P4-sdk-app/go-tests.md`. Nothing else in the Go tree has tests,
so the plan's strategy stands everywhere else.

**6. The `/cam-stream` route is deferred, and its body can never be byte-compared.** Phase 4 above
lists `/cam-stream` (MJPEG q50) as in scope and names `xtask parity-http` route-by-route JSON diff
as the primary gate. The slice implements the ownership, the settle, the enable, the release and
the frame pump behind the `FrameSink` seam, but not the HTTP route, because it needs a JPEG codec
and because Go decodes and re-encodes every frame with its own quantisation tables, so the bytes
are not reproducible even in principle. The parity gate for that route has to be visual rendering
in the browser, not a diff. Recorded in the Tier C list in `early-slice-design.md` and as open
question 5 in the "Not yet verifiable" section of `deviations.md`.

**7. `arc-swap` and any JPEG codec are deferred.** The crate-choice paragraph above names
`arc-swap` for whole-struct config replace and `image`/`jpeg-encoder` for the camera path. Both
are genuinely new packages in `Cargo.lock`, and the slice was run under a rule that no new
`(name, version, checksum)` triple may appear. Nothing in the slice needs either, because there is
no config reload yet and no JPEG re-encode. Both come back when P1 lands config and when the
`/cam-stream` route lands.

**8. The Phase 4 SDK-app slice was implemented ahead of Phases 1 to 3.** The phase list above runs
P0 to P10 in order. The September work in the Go fork added new Phase 4 behaviours, and the user
chose to port them immediately rather than let them age. The slice is therefore built on stubs
behind the seam traits: fake connections, fake receivers, fake frame streams and a manual clock in
`test_support`, plus one loopback tonic fake on `127.0.0.1:0` for the vector crate. Nothing in the
slice binds a privileged port, writes disk state or talks to the robot, so P1 to P3 are unblocked
and unchanged. The scope split is Tier A, Tier B and Tier C in `early-slice-design.md`.

**9. Timing is injectable, and no test uses `start_paused`.** The plan above does not say how time
is controlled in tests. The slice puts the settle of 500 ms, the probe deadline of 5 s, the enable
deadline of 5 s, the disconnect settle of 3 s and the idle limit of 300 s in a `Timings` struct
whose `Default` a test asserts. Tests pass zero settles and millisecond deadlines and wrap every
await in a real-clock `tokio::time::timeout` ceiling that only fires on regression. Tokio's
`start_paused` was rejected because it makes a hung await look like a passing test rather than a
hang. Recorded as decision D5.

**10. `readCamMeter`'s insert-on-read is dropped.** Go's `readCamMeter` at `robot.go:182-185`
calls `getCamMeter`, which creates a map entry when one is missing, so reading an unknown ESN
allocates. `CamMeters::read` returns `(0, 0)` without allocating, and `CamMeters::get` still
creates on first use, which is the path the frame pump takes. The zero-for-unknown answer that
`net_probe` needs is preserved exactly. Deviation 7, tested in
`crates/wirepod-core/tests/cam_meter.rs` and through `RobotRegistry::meters_len`.

**11. The connect-time liveness deadline is `None`, reproducing Go.** Go's `BatteryState` liveness
check at `robot.go:365` uses `context.Background()` and has no deadline, so a powered-off robot
whose IP still routes hangs the dial forever. `RobotRegistry` carries
`liveness_deadline: Option<Duration>` defaulting to `None`. Adding a deadline is not on the plan's
list of allowed bug fixes in core architecture decision 8, and the blast radius is now bounded by
the per-ESN connect lock. Making it a field rather than a hardcoded absence means P1 can set it in
one line and record that as its own deviation. Deviation 9.

**12. The dead connect-time `EventStream` is not opened.** Go's `newRobot` opens a second
`EventStream` at `robot.go:371-382` with the whitelist `["stimulation_info"]` and no
`connection_id`, stores the client and never reads it. The Rust connect issues only
`BatteryState`. The stream is a robot-visible RPC that nothing consumes, and reproducing it would
hold one extra open stream per cached connection at the robot's expense. Deviation 1, pinned by
`crates/wirepod-vector/tests/loopback.rs`, which asserts that a connect issues `BatteryState` and
nothing else and that the stream `begin_event_stream` does open carries the connection id
`wirepod`.

**13. The side effects of `/ok` and `/ok:80` are deferred to P1.** Listener design decision 6
above treats `/ok` and `/ok:80` as conn-check endpoints, and Phase 1 owns mDNS and jdocs. Go's
`connCheck` at `jdocspinger.go:193-220` does more than answer: it calls `RunMDNS("t")`
synchronously when `runMDNS` is set, and otherwise splits the peer IP out of `r.RemoteAddr` and
either pings jdocs for a known peer or spawns `RunMDNS(peerIP)` for an unknown one. The slice
reproduces the bodies only, `ok` or `ran`. The peer-IP rule itself is implemented as
`PingerState::note_check`, which mirrors `ShouldPingJdocs`, and is unit-tested away from the
handler so that P1 only has to wire it in. Deviation 2.

**14. `BotInfoWire` splits the disk shape from the wire shape.** The parity rule above requires
every persisted state struct to carry `#[serde(default)]` and a `#[serde(flatten)]` extras map so
unknown and fork-only fields survive a round trip. For bot info that collides with byte-exactness:
Go's `vars.RobotInfoStore` has exactly `global_guid` and
`robots[{esn, ip_address, guid, activated}]`, and the marshal at `server.go:190` re-emits only
those, in declaration order. The slice keeps two types, a `BotInfo` disk struct with the extras
and a `BotInfoWire` projection without them that `get_sdk_info` serialises. Both properties hold
at once. Deviation 3.

**15. Extras re-serialise in sorted order on a disk round trip.** Fields captured by
`#[serde(flatten)]` land in a `serde_json::Map`, which is a `BTreeMap` because serde_json's
`preserve_order` feature is off, so unknown keys come back sorted rather than in file order.
Turning `preserve_order` on would pull `indexmap` into serde_json's dependency array and unify the
feature across every workspace crate that uses serde_json. The property the rollback rule needs is
survival, not order, and survival is what the round-trip test asserts. Deviation 4.

**16. Vendored text assets are stored CRLF, and the drift check is Windows-only.** The asset
strategy above says `assets/` is copied byte-identically from the Go repo and that `sync-assets`
catches drift. Verification found that the assets were first committed under `core.autocrlf=true`
before `.gitattributes` arrived, leaving 42 of 65 tracked files with LF-only blobs while the
working tree, the Go checkout and the installed production copy were all CRLF. Commit `7a86211`
renormalised once so that Git, the disk, the manifest, the Go checkout and production all agree on
CRLF, which is what production serves. The consequence is that
`cargo run -p xtask -- sync-assets --from ../wire-pod --check` is meaningful only against a
Windows checkout of the Go repo, because a Linux checkout would report every text file as drifted.
P9 chooses a cross-platform normalisation. Decision D11 and deviation 5.

**17. The route counts: `/api-sdk/*` is 45, and `/api/*` is 22, not 25.** Phase 4 above says "all
45 `/api-sdk/*` + 25 `/api/*`". The 45 is right, recounted arm by arm at `origin/main`, and the
one addition since the plan was written is `net_probe`. The 25 is wrong: the web server has 22
route arms, and the plan counted the three `level` cases inside `handleGetLogsJSON` at
`webserver.go:292-296` as if they were routes. The corrected enumerations are the tables in
`docs/phases/P4-sdk-app/sdkapp-routes.md`, which were verified as exact bijections against the Go
source.

**18. Decision D7's stated reason for taking `tokio-util` with default features was inaccurate.**
The slice plan said the `rt` feature would pull `futures-util` into `Cargo.lock`. `futures-util`
was already resolved in the lock, so that consequence was wrong. The choice itself stands and is
unchanged: `CancellationToken` lives in an ungated module, so the `rt` feature buys nothing and
would still mutate tokio-util's dependency array in the lock, which the slice's lock-stability
gate reviews by eye. The `serde_json` `raw_value` feature, taken so that `rttMs` can reach the
wire through a `RawValue` built from `go_json_f64`, adds no packages and is unaffected.

**19. `stopEventStream` deletes unconditionally.** The slice plan described the event-stream stop
as the mirror of the release. It is not. `releaseEventStream` is generation-checked, but
`stopEventStream` deletes the registry entry at `robot.go:256` with no generation check, then
clears the flag and zeroes stim in the same critical section and cancels after dropping the lock,
which is what lets a `begin` immediately after a `stop` be admitted. `EventOwner::stop`
reproduces the unconditional delete and `EventOwner::release` reproduces the generation check, and
the difference is pinned by separate tests.

**20. `EventOwner::claim` and `stop` hand the token back to the caller.** The first implementation
kept the cancellation token inside the owner and cancelled from within the critical section. The
C5 adversarial review found that this lets a stopper cancel a token a later claim had already
installed. `claim(token) -> Option<Generation>` now takes the token, and
`#[must_use] stop() -> Option<CancellationToken>` returns the token that was actually owned, so
the stopper cancels only its own and cancels after dropping the lock, matching Go at
`robot.go:226` and `robot.go:254-262`. The same review found that two ported tests never parked
the receiver before stopping it, so the bug shape they were meant to catch survived them.
`FakeReceiver` gained a readiness oneshot fired on the first poll of `next()`, and the stops are
gated on it. Committed in `05e902c`, deviations 13 and 14.

**21. `CamGuard` is drop-safe, and the disconnect re-checks ownership under the operation lock.**
Go's `camStreamHandler` runs in a goroutine, which always runs to completion, so its
`defer finishCamStream` at `server.go:737` always runs. An axum handler is a future and is dropped
outright when the client aborts, so a guard constructed after the settle and the enable cannot
give back a claim taken before them. `CamGuard` is therefore built immediately after the claim,
owns the session, the camera handle, the timings and its generation, and does the same
generation-checked release from `Drop`, spawning a best-effort disable that takes the camera
operation lock and re-reads ownership before issuing anything. `RobotRegistry::disconnect` does
the analogous thing after its three second settle, issuing one bounded
`enable_image_streaming(false)` under the operation lock only when the owner it stopped still
holds the feed, so a `/cam-stream` request that claims during the settle keeps its camera. Commits
`6668aff`, `045d72e` and `dc15e04`, deviations 15 and 16.

**22. The idle rule is strictly greater than 300 seconds.** `connTimer` at `robot.go:429-452`
zeroes the counter and then loops on sleep, test `>= 300`, increment, so the test first reads 300
at 301 seconds after the reset. Go therefore keeps a robot idle for exactly 300 seconds and
removes it a second later, which is why `idle_candidates` compares with `>` rather than `>=`. The
earlier `>=` was a misreading of the check-then-increment order and was found by the C8
adversarial review. The port does not reproduce Go's accumulating drift from
`time.Sleep(time.Second)`, so under a per-second sweeper it removes at the early end of the window
Go removes in. Nothing drives eviction yet, because the sweeper is Tier C. Recorded under
"Additional recorded differences" in `deviations.md`.

**23. `rttMs` is `float64(rtt.Microseconds())/1000`.** The probe body's round-trip figure is not a
free-form duration. Go computes it at `server.go:117` from the integer microsecond count divided
by a thousand, so it carries at most three decimal places, and its exact digits then follow from
Go's `encoding/json` float writer. The Rust handler reproduces the same computation and pushes the
result through `go_json_f64_raw` as a `serde_json::value::RawValue`, because serde_json's own f64
writer always emits a decimal point and cannot produce Go's `0`. Thirteen live `rttMs` values read
from the running Go server were reproduced exactly by the formatter. Committed in `fa4083e`.

**24. `serde_json` does not escape `<`, `>` and `&`.** Go's `encoding/json` escapes those three
characters inside strings unless a caller turns it off, which nothing in wire-pod does, so both
`json.Marshal` at `server.go:190` and `json.NewEncoder(w).Encode` at `webserver.go:308` escape
them. `serde_json` has no such option and emits them literally. It is accepted because nothing
that reaches either body can contain them: the fields are a serial, an IPv4 address, a base64
GUID, a fixed status word, an integer and a boolean. Any later route that serialises free text, a
robot name or an LLM reply will differ, and the fix then is a custom `serde_json::ser::Formatter`,
not a post-hoc string replace. Deviation 17.

**25. Request paths are canonicalised in a layer in front of the route table.** The first router
matched and dispatched on the escaped path. Go's `findHandler` canonicalises first
(`net/http/server.go:2660-2699`): it cleans the escaped path, answers a 301 when the clean changed
it, and matches with each segment unescaped, because the routing tree's `firstSegment` calls
`pathUnescape` (`net/http/routing_tree.go:205-215`). The new `mux` module reproduces both steps,
and `router::canonicalise` runs them as a layer ahead of the match, rewriting the request URI once
so that the route table, the `/ok:80` fallback literal and both dispatch switches all see the
decoded path. It was live-verified against fourteen Go responses. Two residuals are accepted and
are named in deviation 18: a trailing-slash redirect target built from the cleaned escaped path,
and a segment whose decoded form cannot be written back into a URI path staying escaped. Committed
in `1f8ae8f`.

**26. `sdkapp` has its own caching middleware, distinct from the web root's.** The slice plan
treated cache and sniffing headers as one behaviour. There are two. `webserver.go:415-421` sets
four headers on the web-root surface, and `server.go:791-798` in `sdkapp` sets three of its own,
with `Cache-Control: no-cache, no-store, must-revalidate;` carrying a trailing semicolon and no
`Expires`, and it wraps only `/sdk-app`. Neither wraps `/api-sdk/*`, which sets no CORS and no
cache headers at all, while every `/api/*` response sets `Access-Control-Allow-Origin: *` and
`Access-Control-Allow-Headers: *`. The three sets are pinned separately in the server tests.

**27. The slice plan's inline float-formatting table was wrong, and `expected.txt` is
authoritative.** That table listed `0.53249997` as the `%v` rendering of `float32(0.5325)` and
`1e17` as the `encoding/json` rendering of `float64(1e17)`. A Go probe program run under Go 1.24.4
prints `0.5325` and `100000000000000000`. The Rust tests are driven from
`docs/phases/P4-sdk-app/gofmt-probe/expected.txt`, which is that probe's committed stdout, read
through `include_str!`, so the table cannot drift from Go again. A separate adversarial finding
fixed the tie rule: when a value sits exactly halfway between two shortest digit strings, Go's
`strconv` rounds half to even (`strconv/ftoaryu.go`, `ryuDigits32`) while Rust's shortest
formatter rounds half up, which produced 37,249 mismatches in a 963,000-case corpus.
`go_format_f32` and `go_json_f64` now detect the exact tie and reproduce Go's choice, verified by
a 969,004-case differential run with zero mismatches. Committed in `1d26c46`, with seven tie cases
added to the probe.

**28. The handler contract table in the slice plan stands as written apart from these
corrections.** It is not rewritten, so that the original text stays readable next to what replaced
it. The corrections that touch it are amendments 17, 23, 26 and 27. Everything else in it was
verified against the Go source and against live responses from the running Go server and needed no
change.

**29. The toolchain pins its components, and the CI-hardening item was dropped.** Every push
before `e5d4ee3` failed at `cargo fmt --check`, because the workflow installs stable through
`dtolnay/rust-toolchain@stable` while rustup then auto-installs the pinned 1.92.0 without rustfmt
or clippy. The fix is `components = ["rustfmt", "clippy", "rust-analyzer"]` in
`rust-toolchain.toml`, and a pre-existing `clippy::needless_update` at
`crates/wirepod-proto/tests/roundtrip.rs:24` that blocked the `--all-targets` gate was fixed in
`723d185`. A broader CI-hardening commit was dropped during plan review as unrequested scope that
could not be verified without a push. Recommended for later: `timeout-minutes`, `clippy
--all-targets` in the workflow, and pinning `components` alongside the toolchain action. The local
gate list grew to include `cargo clippy --all-targets --all-features -- -D warnings`.

**30. The Ubuntu CI leg is green through the C3 commit and unverified for the rest of the slice.**
The first run green on both `windows-latest` and `ubuntu-latest` was on `5f7170f` (run
34305117622, 2026-09-09), which closed the cross-platform risk for the workspace and the asset
manifest. Every commit of the slice after it has been gated only on Windows, with the four CI
commands plus the two extra clippy invocations run locally after each one. The Linux leg stays
unverified for the slice until the work is pushed and a run is observed. This is open question 7
in the "Not yet verifiable" section of `deviations.md`.

**31. Four files exist that the slice plan's module layout block does not name.** They are
recorded in `early-slice-design.md` rather than by editing the block.
`crates/wirepod-server/src/mux.rs` holds the path canonicalisation of amendment 25.
`crates/wirepod-server/src/sdkapp/disconnect.rs` is a module rather than an inline arm because it
carries an ordering, that the body is written only after `RobotRegistry::disconnect` returns so
the request pays the settle first, and that the answer is `done` whatever happened.
`crates/wirepod-server/tests/lifecycle.rs` is a sixth test binary, because the idle-timer
asymmetry it pins spans the preamble, all ten slice routes, an unknown path under the prefix and
`/cam-stream`, and so belongs to no single route. `crates/wirepod-server/src/test_support.rs` is
new, behind the same `test-util` feature the other two crates use, because four test binaries
needed the same three helpers and an integration-test binary cannot import another. Two further
differences from that block: `literals.rs` sits at the crate root rather than under `sdkapp/`,
because its constants cover all three surfaces, and there is no
`crates/wirepod-server/src/state.rs`, because the axum state is `Arc<wirepod_core::AppState>`
directly and a newtype would hold nothing until P1.
