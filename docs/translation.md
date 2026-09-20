# The translation plan

This is the only plan for wire-pod-rs. It was adopted on 2026-09-19 and replaces everything under `docs/archive/`.

The Go wire-pod server works. This repository translates it to Rust, file by file, until every Go file has a Rust module. Debugging against the real robot, optimization and packaging come after that, not during it.

The Go source is `E:/GitHub/wire-pod/chipper`, 12,130 non-test lines in 67 files. It has not changed since the commit this port started from.

## Rules

1. One Go file becomes one Rust module: same functions, same order, same control flow, Rust naming. Comments only where the Rust differs from the Go.
2. No reviewer or fixer agents, no Go recording programs, no deviations ledger, no line citations. A translation is checked by reading it against the Go file.
3. One or two tests per file, for what the robot, the web UI or the state files can observe. The gate is CI's four commands: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo build`, `cargo test`.
4. Translation reaches 100% before anything is debugged in depth, optimized, refactored or hardened. Findings go on the list at the bottom.
5. A Go crash becomes a returned error or a log line. Nothing else about Go's behaviour changes.
6. The older plan's redesigns wait until after translation: `kgsim.go` and `kgsim_cmds.go` stay two files, there is no WASM plugin host, and restart does what `RestartServer` does.
7. Code already on master stays and gets built on.
8. Dependencies are added when a file needs them.
9. Real-robot testing never blocks translation. Tests are loopback only. When the user has ten minutes, whatever exists is run against the robot and the findings go on the list.
10. Commits go to master, one per Go file or small group, and each milestone ends with a push.

## Milestones

The user reordered these on 2026-09-19, after M1: the web UI moved ahead of voice, and the LLM work follows voice.

| Milestone | What it gives | Go lines |
|---|---|---|
| M1 | the robot can connect: listeners, the three gRPC services, mDNS, the `/ok` side effects, a `serve` subcommand | about 1,200 |
| M2 | the web UI's API and the rest of the SDK app | about 2,000 |
| M3 | voice commands: audio, Vosk, intent matching, the request processors | about 3,100 |
| M4 | LLM, knowledge graph, weather | about 2,000 |
| M5 | Lua scripting, certificates and SSH setup, then the parts the Windows install does not use (five other speech engines, BLE, the Go plugin loader) | about 1,900 |
| M6 | debug against the robot, the optimization list, tray shell, packaging, cutover | none |

## File table

Status is one of: done, partial, or the milestone that will translate it.

| Go file | Lines | Rust module | Status |
|---|---|---|---|
| `cmd/vosk/main.go` | 10 | `wirepod-app/src/serve.rs` | done, with no voice processor until M3 |
| `cmd/coqui/main.go`, `cmd/leopard/main.go`, `cmd/experimental/{houndify,whisper,whisper.cpp}/main.go` | 49 | `wirepod-app` engine selection | M5 |
| `pkg/initwirepod/startserver.go` | 231 | `wirepod-server/src/startserver.rs` | done |
| `pkg/initwirepod/web.go` | 56 | `wirepod-server/src/initweb.rs` | done |
| `pkg/logger/logger.go` | 248 | `wirepod-core/src/logger.rs` | done |
| `pkg/logger/msg-and.go`, `msg-winmac.go` | 40 | `wirepod-app` | M6 |
| `pkg/mdnshandler/mdns.go` | 90 | `wirepod-server/src/mdns.rs` | done |
| `pkg/scripting/scripting.go` | 317 | `wirepod-plugins/src/scripting.rs` | M5 |
| `pkg/scripting/bcontrol.go` | 92 | `wirepod-plugins/src/bcontrol.rs` | M5 |
| `pkg/scripting/display.go` | 36 | `wirepod-plugins/src/display.rs` | M5 |
| `pkg/servers/chipper/*.go` (seven files) | 296 | `wirepod-server/src/chipper/*.rs` | done |
| `pkg/servers/jdocs/server.go` | 200 | `wirepod-server/src/jdocs/server.rs` | done |
| `pkg/servers/jdocs/botInfoStorer.go` | 153 | `wirepod-core/src/store/bot_info.rs` | done |
| `pkg/servers/token/hashing.go` | 136 | `wirepod-core/src/token/hash.rs` | done |
| `pkg/servers/token/token.go` | 300 | `wirepod-core/src/token/{jwt,stores}.rs` and `wirepod-server/src/token.rs` | done |
| `pkg/vars/config.go` | 158 | `wirepod-core/src/config.rs` | done |
| `pkg/vars/vars.go` | 465 | `wirepod-core/src/{paths,state,intents}.rs` and `store/*.rs` | done; `RememberedChats` comes with M4 |
| `pkg/vtt/*.go` (three files) | 79 | `wirepod-server/src/vtt.rs` | done |
| `pkg/wirepod/config-ws/webserver.go` | 545 | `wirepod-server/src/api/*.rs` | partial, 1 of 22 routes; M2 |
| `pkg/wirepod/localization/localization.go` | 259 | `wirepod-intent/src/localization.rs` | done apart from `ReloadVosk` |
| `pkg/wirepod/localization/download.go` | 192 | `wirepod-intent/src/download.rs` | done |
| `pkg/wirepod/preqs/server.go` | 76 | `wirepod-ttr/src/preqs/server.rs` | M3 |
| `pkg/wirepod/preqs/intent.go` | 63 | `wirepod-ttr/src/preqs/intent.rs` | M3 |
| `pkg/wirepod/preqs/intent_graph.go` | 95 | `wirepod-ttr/src/preqs/intent_graph.rs` | M3 |
| `pkg/wirepod/preqs/knowledgegraph.go` | 159 | `wirepod-ttr/src/preqs/knowledgegraph.rs` | M3 |
| `pkg/wirepod/preqs/stream_houndify.go` | 61 | `wirepod-ttr/src/preqs/stream_houndify.rs` | M5 |
| `pkg/wirepod/sdkapp/robot.go` | 515 | `wirepod-core/src/robot/*.rs` and `wirepod-vector` | mostly done; the remainder is M2 |
| `pkg/wirepod/sdkapp/server.go` | 886 | `wirepod-server/src/sdkapp/*.rs` | partial, 10 of 45 routes; M2 |
| `pkg/wirepod/sdkapp/jdocspinger.go` | 269 | `wirepod-server/src/jdocspinger.rs` | done |
| `pkg/wirepod/sdkapp/batterywatchdog.go` | 290 | `wirepod-server/src/sdkapp/batterywatchdog.rs` | M2 |
| `pkg/wirepod/sdkapp/bcassume.go` | 91 | `wirepod-server/src/sdkapp/bcassume.rs` | M2 |
| `pkg/wirepod/sdkapp/urlreqs.go` | 67 | `wirepod-vector/src/urlreqs.rs` | M2 |
| `pkg/wirepod/setup/certs.go` | 124 | `wirepod-setup/src/certs.rs` | M5 |
| `pkg/wirepod/setup/ssh.go` | 254 | `wirepod-setup/src/ssh.rs` | M5 |
| `pkg/wirepod/setup/ble.go`, `ble_other.go` | 530 | `wirepod-setup/src/ble.rs` | M5, last; the user decides then |
| `pkg/wirepod/speechrequest/speechrequest.go` | 365 | `wirepod-audio/src/speechrequest.rs` | M3 |
| `pkg/wirepod/stt/vosk/Vosk.go` | 223 | `wirepod-stt/src/vosk.rs` | M3 |
| `pkg/wirepod/stt/vosk/context.go` | 79 | `wirepod-stt/src/vosk_context.rs` | M3 |
| `pkg/wirepod/stt/{coqui,houndify,leopard,whisper,whisper.cpp}` | 525 | `wirepod-stt/src/<engine>.rs` behind features | M5, last; the user decides then |
| `pkg/wirepod/ttr/intentparam.go` | 719 | `wirepod-intent/src/intentparam.rs` | M3 |
| `pkg/wirepod/ttr/matchIntentSend.go` | 337 | `wirepod-intent/src/match_intent_send.rs` | M3 |
| `pkg/wirepod/ttr/words2num.go` | 169 | `wirepod-intent/src/words2num.rs` | done |
| `pkg/wirepod/ttr/convert.go` | 92 | `wirepod-ttr/src/convert.rs` | done |
| `pkg/wirepod/ttr/bcontrol.go` | 141 | `wirepod-ttr/src/bcontrol.rs` | M3 |
| `pkg/wirepod/ttr/kgsim.go` | 713 | `wirepod-ttr/src/kgsim.rs` | M4 |
| `pkg/wirepod/ttr/kgsim_cmds.go` | 730 | `wirepod-ttr/src/kgsim_cmds.rs` | M4 |
| `pkg/wirepod/ttr/kgsim_interrupt.go` | 92 | `wirepod-ttr/src/kgsim_interrupt.rs` | M4 |
| `pkg/wirepod/ttr/weather.go` | 432 | `wirepod-ttr/src/weather.rs` | done |
| `pkg/wirepod/ttr/plugins.go` | 81 | `wirepod-ttr/src/plugins.rs` | M5, last; the user decides then |

## Progress

| Date | Go lines translated | Server can |
|---|---|---|
| 2026-09-19 | about 2,300 of 12,130 (19%) | nothing the robot can use yet; the SDK dashboard slice runs beside Go |
| 2026-09-19, after M1 | about 4,800 of 12,130 (40%) | M1 translated: `chipper serve` starts the TLS listener with the chipper, jdocs and token services, mDNS, the `/ok` side effects and `/api-chipper/`; proven on loopback, not yet run against the robot |
| 2026-09-19, M2 | about 6,800 of 12,130 (56%) | the web UI runs on the Rust server: every page, all 22 `/api` routes and all 45 `/api-sdk` routes, the static mounts, the camera route, the battery watchdog and the idle sweeper. Checked against the Go server side by side. |
| 2026-09-19, robot session | unchanged | M1 confirmed on the real robot: Vector completed TLS with the Rust listener, called `Jdocs/ReadDocs`, held his heartbeat on port 80, and the server pulled his jdocs. No token or voice request arrived during the session |

## Facts worth keeping from the old plan

- Ports 80 and 8080 serve the same mux in Go, with every route on both. There is no method checking. `/api/` responses carry CORS `*`.
- The robot never verifies the JWT signature. It requires six claims as JSON strings: `token_id`, `token_type`, `user_id`, `requestor_id`, `iat`, `expires`.
- Audio: a first byte of `0x4F` means Ogg-Opus, anything else is raw 16 kHz s16le PCM. The high-pass filter resets its state on every chunk, and that is kept. VAD is WebRTC mode 2 on 320-byte frames, and speech ends at 23 inactive frames after more than 18 active ones.
- mDNS: instance `escapepod`, service `_app-proto._tcp`, port 8084.
- The magic constants (the app key, the global GUID and its hash document, the BLE auth token) are copied verbatim from `pkg/vars/vars.go` and the Go files that use them.
- The route list for the SDK app and the web API is `docs/archive/phases/P4-sdk-app/sdkapp-routes.md`. It is the checklist for M2.
- `docs/pending-upstream.md` tracks upstream wire-pod commits the Go fork has not merged.

## Debug and optimization list

Nothing here is acted on until translation is 100%.

From the browser session of 2026-09-19, with the robot attached to the Rust server:

- `/cam-stream` produces no frames. The robot lets `EnableImageStreaming` time out after five seconds and then `CameraFeed` never sends headers, so the handler hangs until the client gives up. Every other SDK call works on the same connection, including `SayText`, `BatteryState`, `PullJdocs` and `ProtocolVersion`. Not yet compared against the Go server, which is the test that would say whether this is the robot's state or a difference in the port.
- Go's `get_ota` indexes a path segment that the only matching route cannot have, so the handler panics on every call. The port answers Go's own `failed to parse URL` 500 instead, and the proxy below it is unreachable in both.
- `print_robot_info` prints the robot's GUID in Go. The port leaves it out.
- The `sdkapp` log target is not in the default filter, so lines logged to it at debug never appear. Either add it to `DEFAULT_FILTER` or move those lines to a crate target.

Checked against the running Go server, web-only on 18080 beside it:

- Every static path matches byte for byte with the same headers: `/`, `/index.html`, the CSS and JS, `/sdkapp/*`, the 404 and `/ok`.
- Of the twelve read-only `/api` routes, nine match in status, headers and body shape. The three that differ do so because of the environment, not the code. The STT provider is blank because both servers overwrite it from `STT_SERVICE`, which the tray app sets and a shell run does not. The log routes differ only by uptime, and the entry keys and types match. `get_version_info` reports from source because `assets/` carries no `version` file where the install does.
- `/cam-stream` produces nothing on the Go server either, with the same robot in the same state, so the port matches Go here.
- Running `chipper serve` from a shell with `STT_SERVICE` unset will blank the STT provider in `apiConfig.json`. Set `STT_SERVICE` and `STT_LANGUAGE` to match the tray app before pointing the server at the live data directory.
- `assets/` does not vendor the `version` file the install has, so `get_version_info` always reports from source.

- A JSON key with a malformed Unicode escape makes the Rust decoder drop the whole file where Go loads it (`gojson::merge_object`).
- The in-memory token store grows without bound, as in Go.
- Session-certificate writes build a fresh write gate per call.
- `ApiConfig` and `BotInfo` derive a full `Debug` and could leak keys into a log line.
- `mdns_sd` logs `failed to send response of shutdown` every 32 seconds, each time the registration loop re-registers. The name still resolves.
- An unrouted gRPC path on the TLS listener answers the router's 404 rather than tonic's `Unimplemented`, because `/ok:80` is matched in the fallback.
- `pingJdocs` retries on the cached robot connection where Go dials a second one.
- The mDNS browse in `jdocspinger.rs` sits behind `set_mdns_enabled`, which no boot path turns on yet.
- Log lines with ANSI colour codes print the escape as text under the `tracing` formatter.
- `/api-chipper` without the trailing slash has no redirect to `/api-chipper/`.
- `download.rs` skips zip entries that would leave the destination, and ignores file modes.
