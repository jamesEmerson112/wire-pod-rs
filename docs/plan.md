This file is the approved master plan for the Rust port of the wire-pod `chipper` server. It was written and approved in a planning session and originally lived at `~/.claude/plans/breezy-knitting-cray.md`, outside the repository. It has been copied here unchanged so that project knowledge lives with the project. This copy is now the authoritative one. The home-directory original is kept only as a historical artifact; when the two differ, this file wins.

The plan below is the roadmap at the level of phases, crates, and locked decisions. Detail for each individual phase lives in `docs/phases/`, one document per phase, with an index at `docs/phases/README.md`. Nothing in the plan text below has been edited. As decisions change during implementation, they are recorded in the `## Amendments` section appended at the very end of this file rather than by rewriting the plan, so the original reasoning stays readable next to what replaced it.

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
- Token hashing must match Go byte-for-byte (existing jdocs hold Go-produced hashes): GUID = b64(16 rand bytes); hash = b64(SHA256(token‖salt)‖salt), sizes 16/16/32; constant-time compare. JWT: RS512, throwaway RSA key per call (robot never verifies sig but requires all 7 claims, RFC3339Nano-ish timestamps).
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

Key crate choices: tokio, tonic 0.12 + prost + **protox**, axum 0.8, **hyper-util `auto` builder** (= the cmux replacement: sniffs h2 preface when ALPN absent), tokio-rustls (+ non-default `tls-native` escape hatch), **mdns-sd**, arc-swap (whole-struct config replace), `ogg` + `opus` (vendored libopus — same lib as Go ⇒ bit-identical decode; drops shipped DLLs), `webrtc-vad` (same C source as Go), `vosk` (reuse shipped libvosk.dll), whisper-rs, pv_leopard (official), reqwest(rustls), **mlua lua51+vendored** (gopher-lua is 5.1; Luau rejected — would break user scripts), **extism** (WASM plugins; PDKs let Go plugin authors recompile not rewrite), russh (pure-Rust SSH), btleplug (deferred feature), jsonwebtoken, rsa+rcgen (**generate 2048-bit certs going forward**; ring rejects <2048; preflight ERROR for legacy keys), rust-ini, image/jpeg-encoder, tracing (+custom ring layer), uuid/base64/sha2/subtle/chrono, tray-icon + windows crate (Phase 9). Weak-on-Windows flags: btleplug (deferred per D3), coqui libstt (Linux-only, feature-off, best-effort — project discontinued).

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
