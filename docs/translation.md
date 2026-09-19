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

| Milestone | What it gives | Go lines |
|---|---|---|
| M1 | the robot can connect: listeners, the three gRPC services, mDNS, the `/ok` side effects, a `serve` subcommand | about 1,200 |
| M2 | voice commands: audio, Vosk, intent matching, the request processors | about 3,100 |
| M3 | LLM, knowledge graph, weather | about 2,000 |
| M4 | the web UI's API and the rest of the SDK app | about 2,000 |
| M5 | Lua scripting, certificates and SSH setup, then the parts the Windows install does not use (five other speech engines, BLE, the Go plugin loader) | about 1,900 |
| M6 | debug against the robot, the optimization list, tray shell, packaging, cutover | none |

## File table

Status is one of: done, partial, M1 in progress, or the milestone that will translate it.

| Go file | Lines | Rust module | Status |
|---|---|---|---|
| `cmd/vosk/main.go` | 10 | `wirepod-app/src/serve.rs` | M1 |
| `cmd/coqui/main.go`, `cmd/leopard/main.go`, `cmd/experimental/{houndify,whisper,whisper.cpp}/main.go` | 49 | `wirepod-app` engine selection | M5 |
| `pkg/initwirepod/startserver.go` | 231 | `wirepod-server/src/startserver.rs` | M1 in progress |
| `pkg/initwirepod/web.go` | 56 | `wirepod-server/src/initweb.rs` | M1 in progress |
| `pkg/logger/logger.go` | 248 | `wirepod-core/src/logger.rs` | done |
| `pkg/logger/msg-and.go`, `msg-winmac.go` | 40 | `wirepod-app` | M6 |
| `pkg/mdnshandler/mdns.go` | 90 | `wirepod-server/src/mdns.rs` | M1 in progress |
| `pkg/scripting/scripting.go` | 317 | `wirepod-plugins/src/scripting.rs` | M5 |
| `pkg/scripting/bcontrol.go` | 92 | `wirepod-plugins/src/bcontrol.rs` | M5 |
| `pkg/scripting/display.go` | 36 | `wirepod-plugins/src/display.rs` | M5 |
| `pkg/servers/chipper/*.go` (seven files) | 296 | `wirepod-server/src/chipper/*.rs` | M1 in progress |
| `pkg/servers/jdocs/server.go` | 200 | `wirepod-server/src/jdocs/server.rs` | M1 in progress |
| `pkg/servers/jdocs/botInfoStorer.go` | 153 | `wirepod-core/src/store/bot_info.rs` | M1 in progress |
| `pkg/servers/token/hashing.go` | 136 | `wirepod-core/src/token/hash.rs` | done |
| `pkg/servers/token/token.go` | 300 | `wirepod-core/src/token/{jwt,stores}.rs` and `wirepod-server/src/token.rs` | partial; the handlers are M1 in progress |
| `pkg/vars/config.go` | 158 | `wirepod-core/src/config.rs` | done |
| `pkg/vars/vars.go` | 465 | `wirepod-core/src/{paths,state}.rs` and `store/*.rs` | done apart from the loaders later milestones need |
| `pkg/vtt/*.go` (three files) | 79 | `wirepod-server/src/vtt.rs` | M1 in progress |
| `pkg/wirepod/config-ws/webserver.go` | 545 | `wirepod-server/src/api/*.rs` | partial, 1 of 22 routes; M4 |
| `pkg/wirepod/localization/localization.go` | 259 | `wirepod-intent/src/localization.rs` | M2 |
| `pkg/wirepod/localization/download.go` | 192 | `wirepod-intent/src/download.rs` | M2 |
| `pkg/wirepod/preqs/server.go` | 76 | `wirepod-ttr/src/preqs/server.rs` | M2 |
| `pkg/wirepod/preqs/intent.go` | 63 | `wirepod-ttr/src/preqs/intent.rs` | M2 |
| `pkg/wirepod/preqs/intent_graph.go` | 95 | `wirepod-ttr/src/preqs/intent_graph.rs` | M2 |
| `pkg/wirepod/preqs/knowledgegraph.go` | 159 | `wirepod-ttr/src/preqs/knowledgegraph.rs` | M2 |
| `pkg/wirepod/preqs/stream_houndify.go` | 61 | `wirepod-ttr/src/preqs/stream_houndify.rs` | M5 |
| `pkg/wirepod/sdkapp/robot.go` | 515 | `wirepod-core/src/robot/*.rs` and `wirepod-vector` | mostly done; the remainder is M4 |
| `pkg/wirepod/sdkapp/server.go` | 886 | `wirepod-server/src/sdkapp/*.rs` | partial, 10 of 45 routes; M4 |
| `pkg/wirepod/sdkapp/jdocspinger.go` | 269 | `wirepod-server/src/jdocspinger.rs` | M1 in progress |
| `pkg/wirepod/sdkapp/batterywatchdog.go` | 290 | `wirepod-server/src/sdkapp/batterywatchdog.rs` | M4 |
| `pkg/wirepod/sdkapp/bcassume.go` | 91 | `wirepod-server/src/sdkapp/bcassume.rs` | M4 |
| `pkg/wirepod/sdkapp/urlreqs.go` | 67 | `wirepod-vector/src/urlreqs.rs` | M4 |
| `pkg/wirepod/setup/certs.go` | 124 | `wirepod-setup/src/certs.rs` | M5 |
| `pkg/wirepod/setup/ssh.go` | 254 | `wirepod-setup/src/ssh.rs` | M5 |
| `pkg/wirepod/setup/ble.go`, `ble_other.go` | 530 | `wirepod-setup/src/ble.rs` | M5, last; the user decides then |
| `pkg/wirepod/speechrequest/speechrequest.go` | 365 | `wirepod-audio/src/speechrequest.rs` | M2 |
| `pkg/wirepod/stt/vosk/Vosk.go` | 223 | `wirepod-stt/src/vosk.rs` | M2 |
| `pkg/wirepod/stt/vosk/context.go` | 79 | `wirepod-stt/src/vosk_context.rs` | M2 |
| `pkg/wirepod/stt/{coqui,houndify,leopard,whisper,whisper.cpp}` | 525 | `wirepod-stt/src/<engine>.rs` behind features | M5, last; the user decides then |
| `pkg/wirepod/ttr/intentparam.go` | 719 | `wirepod-intent/src/intentparam.rs` | M2 |
| `pkg/wirepod/ttr/matchIntentSend.go` | 337 | `wirepod-intent/src/match_intent_send.rs` | M2 |
| `pkg/wirepod/ttr/words2num.go` | 169 | `wirepod-intent/src/words2num.rs` | M2 |
| `pkg/wirepod/ttr/convert.go` | 92 | `wirepod-ttr/src/convert.rs` | M2 |
| `pkg/wirepod/ttr/bcontrol.go` | 141 | `wirepod-ttr/src/bcontrol.rs` | M2 |
| `pkg/wirepod/ttr/kgsim.go` | 713 | `wirepod-ttr/src/kgsim.rs` | M3 |
| `pkg/wirepod/ttr/kgsim_cmds.go` | 730 | `wirepod-ttr/src/kgsim_cmds.rs` | M3 |
| `pkg/wirepod/ttr/kgsim_interrupt.go` | 92 | `wirepod-ttr/src/kgsim_interrupt.rs` | M3 |
| `pkg/wirepod/ttr/weather.go` | 432 | `wirepod-ttr/src/weather.rs` | M3 |
| `pkg/wirepod/ttr/plugins.go` | 81 | `wirepod-ttr/src/plugins.rs` | M5, last; the user decides then |

## Progress

| Date | Go lines translated | Server can |
|---|---|---|
| 2026-09-19 | about 2,300 of 12,130 (19%) | nothing the robot can use yet; the SDK dashboard slice runs beside Go |

## Facts worth keeping from the old plan

- Ports 80 and 8080 serve the same mux in Go, with every route on both. There is no method checking. `/api/` responses carry CORS `*`.
- The robot never verifies the JWT signature. It requires six claims as JSON strings: `token_id`, `token_type`, `user_id`, `requestor_id`, `iat`, `expires`.
- Audio: a first byte of `0x4F` means Ogg-Opus, anything else is raw 16 kHz s16le PCM. The high-pass filter resets its state on every chunk, and that is kept. VAD is WebRTC mode 2 on 320-byte frames, and speech ends at 23 inactive frames after more than 18 active ones.
- mDNS: instance `escapepod`, service `_app-proto._tcp`, port 8084.
- The magic constants (the app key, the global GUID and its hash document, the BLE auth token) are copied verbatim from `pkg/vars/vars.go` and the Go files that use them.
- The route list for the SDK app and the web API is `docs/archive/phases/P4-sdk-app/sdkapp-routes.md`. It is the checklist for M4.
- `docs/pending-upstream.md` tracks upstream wire-pod commits the Go fork has not merged.

## Debug and optimization list

Nothing here is acted on until translation is 100%.

- A JSON key with a malformed Unicode escape makes the Rust decoder drop the whole file where Go loads it (`gojson::merge_object`).
- The in-memory token store grows without bound, as in Go.
- Session-certificate writes build a fresh write gate per call.
- `ApiConfig` and `BotInfo` derive a full `Debug` and could leak keys into a log line.
- The live listener test against the robot has never been confirmed. The first robot session starts there.
