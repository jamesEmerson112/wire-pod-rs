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
| M4 | LLM and knowledge graph. NOT TRANSLATED, by the user's decision on 2026-09-20. Weather, which shared the milestone, is done. | 1,535 not translated |
| M5 | Lua scripting, certificates and SSH setup. Done. The rest of the milestone is cut or deferred: the five other speech engines and the Go plugin loader are cut, Bluetooth onboarding is deferred. | 823 translated |
| M6 | the Rust server replaces the Go one on this PC: the log fix, robot sessions, the findings triage, the Windows tray, a deploy script, a 24-hour soak and the cutover. Planned on 2026-09-26, see "M6 plan" below. | 976 in the WirePod repo, most of it new, and 40 in `pkg/logger` |

## File table

Status is one of: done, partial, or the milestone that will translate it.

| Go file | Lines | Rust module | Status |
|---|---|---|---|
| `cmd/vosk/main.go` | 10 | `wirepod-app/src/serve.rs` | done, with no voice processor until M3 |
| `cmd/coqui/main.go`, `cmd/leopard/main.go`, `cmd/experimental/{houndify,whisper,whisper.cpp}/main.go` | 49 | none | cut, by decision |
| `pkg/initwirepod/startserver.go` | 231 | `wirepod-server/src/startserver.rs` | done |
| `pkg/initwirepod/web.go` | 56 | `wirepod-server/src/initweb.rs` | done |
| `pkg/logger/logger.go` | 248 | `wirepod-core/src/logger.rs` | done |
| `pkg/logger/msg-and.go`, `msg-winmac.go` | 40 | `wirepod-core/src/msg.rs` | done |
| `pkg/mdnshandler/mdns.go` | 90 | `wirepod-server/src/mdns.rs` | done |
| `pkg/scripting/scripting.go` | 317 | `wirepod-plugins/src/scripting.rs` | done, apart from the `gopher-lua-libs` preload |
| `pkg/scripting/bcontrol.go` | 92 | `wirepod-plugins/src/bcontrol.rs` | done |
| `pkg/scripting/display.go` | 36 | `wirepod-plugins/src/display.rs` | done |
| `pkg/servers/chipper/*.go` (seven files) | 296 | `wirepod-server/src/chipper/*.rs` | done |
| `pkg/servers/jdocs/server.go` | 200 | `wirepod-server/src/jdocs/server.rs` | done |
| `pkg/servers/jdocs/botInfoStorer.go` | 153 | `wirepod-core/src/store/bot_info.rs` | done |
| `pkg/servers/token/hashing.go` | 136 | `wirepod-core/src/token/hash.rs` | done |
| `pkg/servers/token/token.go` | 300 | `wirepod-core/src/token/{jwt,stores}.rs` and `wirepod-server/src/token.rs` | done |
| `pkg/vars/config.go` | 158 | `wirepod-core/src/config.rs` | done |
| `pkg/vars/vars.go` | 465 | `wirepod-core/src/{paths,state,intents}.rs` and `store/*.rs` | done; `RememberedChats` comes with M4 |
| `pkg/vtt/*.go` (three files) | 79 | `wirepod-server/src/vtt.rs` | done |
| `pkg/wirepod/config-ws/webserver.go` | 545 | `wirepod-server/src/api/*.rs` | done, all 22 routes |
| `pkg/wirepod/localization/localization.go` | 259 | `wirepod-intent/src/localization.rs` | done apart from `ReloadVosk` |
| `pkg/wirepod/localization/download.go` | 192 | `wirepod-intent/src/download.rs` | done |
| `pkg/wirepod/preqs/server.go` | 76 | `wirepod-ttr/src/preqs/server.rs` | done |
| `pkg/wirepod/preqs/intent.go` | 63 | `wirepod-ttr/src/preqs/intent.rs` | done |
| `pkg/wirepod/preqs/intent_graph.go` | 95 | `wirepod-ttr/src/preqs/intent_graph.rs` | done |
| `pkg/wirepod/preqs/knowledgegraph.go` | 159 | `wirepod-ttr/src/preqs/knowledgegraph.rs` | done |
| `pkg/wirepod/preqs/stream_houndify.go` | 61 | none | cut, by decision |
| `pkg/wirepod/sdkapp/robot.go` | 515 | `wirepod-core/src/robot/*.rs` and `wirepod-vector` | done |
| `pkg/wirepod/sdkapp/server.go` | 886 | `wirepod-server/src/sdkapp/*.rs` | done, all 45 routes |
| `pkg/wirepod/sdkapp/jdocspinger.go` | 269 | `wirepod-server/src/jdocspinger.rs` | done |
| `pkg/wirepod/sdkapp/batterywatchdog.go` | 290 | `wirepod-server/src/sdkapp/batterywatchdog.rs` | done |
| `pkg/wirepod/sdkapp/bcassume.go` | 91 | `wirepod-server/src/sdkapp/bcassume.rs` | done |
| `pkg/wirepod/sdkapp/urlreqs.go` | 67 | `wirepod-vector/src/urlreqs.rs` | done |
| `pkg/wirepod/setup/certs.go` | 124 | `wirepod-setup/src/certs.rs` | done |
| `pkg/wirepod/setup/ssh.go` | 254 | `wirepod-setup/src/ssh.rs` | done; `russh` has never spoken to the robot |
| `pkg/wirepod/setup/ble.go`, `ble_other.go` | 530 | `wirepod-setup/src/ble.rs` | deferred; the user wants it upgraded rather than translated |
| `pkg/wirepod/speechrequest/speechrequest.go` | 365 | `wirepod-audio/src/speechrequest.rs` | done |
| `pkg/wirepod/stt/vosk/Vosk.go` | 223 | `wirepod-stt/src/vosk.rs` | done, behind the `stt-vosk` feature |
| `pkg/wirepod/stt/vosk/context.go` | 79 | `wirepod-stt/src/vosk_context.rs` | done |
| `pkg/wirepod/stt/{coqui,houndify,leopard,whisper,whisper.cpp}` | 525 | none | cut, by decision |
| `pkg/wirepod/ttr/intentparam.go` | 719 | `wirepod-intent/src/intentparam.rs` | done |
| `pkg/wirepod/ttr/matchIntentSend.go` | 337 | `wirepod-intent/src/match_intent_send.rs` | done |
| `pkg/wirepod/ttr/words2num.go` | 169 | `wirepod-intent/src/words2num.rs` | done |
| `pkg/wirepod/ttr/convert.go` | 92 | `wirepod-ttr/src/convert.rs` | done |
| `pkg/wirepod/ttr/bcontrol.go` | 141 | `wirepod-ttr/src/bcontrol.rs` | done |
| `pkg/wirepod/ttr/kgsim.go` | 713 | none | not translated, by decision |
| `pkg/wirepod/ttr/kgsim_cmds.go` | 730 | none | not translated, by decision |
| `pkg/wirepod/ttr/kgsim_interrupt.go` | 92 | none | not translated, by decision |
| `pkg/wirepod/ttr/weather.go` | 432 | `wirepod-ttr/src/weather.rs` | done |
| `pkg/wirepod/ttr/plugins.go` | 81 | none | cut; Rust cannot load Go `.so` plugins at all |

The Windows shell comes from a second Go repo, `E:/GitHub/WirePod`, which is kercre123/WirePod at `9b5ff6e`. It builds the installed `chipper.exe` by importing the `chipper` module. It is read-only here, like the Go checkout. Only its Windows files are listed. The `android`, `debian`, `macos` and `cross/mac` trees are not used on this machine.

| Go file in the WirePod repo | Lines | Rust module | Status |
|---|---|---|---|
| `windows/cmd/main.go` | 10 | `wirepod-app/src/main.rs` | done, behind the `tray` feature |
| `cross/all/all.go` | 43 | `wirepod-app/src/tray/all.rs` | done |
| `cross/win/funcs.go` | 130 | `wirepod-app/src/tray/win/funcs.rs` | done |
| `cross/win/registry.go` | 194 | `wirepod-app/src/tray/win/registry.rs` | done |
| `cross/win/syscallstuff.go` | 29 | `wirepod-app/src/tray/win/syscallstuff.rs` | done |
| `cross/podapp/main.go` | 249 | `wirepod-app/src/tray/podapp.rs` | done; has not run on the desktop yet |
| `cross/podapp/initwirepod.go` | 265 | `wirepod-app/src/tray/initwirepod.rs` | done; only what differs from `startserver.go` |
| `cross/podapp/web.go` | 56 | `wirepod-server/src/initweb.rs` | done; identical to `pkg/initwirepod/web.go` |
| `windows/installer/*.go`, `windows/uninstall/main.go` | 834 | none | kept as Go; the installed copies stay |

## Progress

| Date | Go lines translated | Server can |
|---|---|---|
| 2026-09-19 | about 2,300 of 12,130 (19%) | nothing the robot can use yet; the SDK dashboard slice runs beside Go |
| 2026-09-19, after M1 | about 4,800 of 12,130 (40%) | M1 translated: `chipper serve` starts the TLS listener with the chipper, jdocs and token services, mDNS, the `/ok` side effects and `/api-chipper/`; proven on loopback, not yet run against the robot |
| 2026-09-20, M3 | about 9,100 of 12,130 (75%) | the voice pipeline is translated: audio decode and VAD, the Vosk engine behind a feature, intent matching and parameter extraction, behaviour control, and the three request processors wired into `chipper serve`. Not yet run against the robot. |
| 2026-09-19, M2 | about 6,800 of 12,130 (56%) | the web UI runs on the Rust server: every page, all 22 `/api` routes and all 45 `/api-sdk` routes, the static mounts, the camera route, the battery watchdog and the idle sweeper. Checked against the Go server side by side. |
| 2026-09-20, M5 | 9,309 of 12,130 (77%), which is every line the port is going to translate | the Lua host runs a script on the robot and `/api-lua/run_script` answers, a custom intent runs the script attached to it, the web UI can generate the certificate pair and the server config, and `/api-ssh/setup` can push a bot through onboarding. Nothing in M5 has met the robot. |
| 2026-09-19, robot session | unchanged | M1 confirmed on the real robot: Vector completed TLS with the Rust listener, called `Jdocs/ReadDocs`, held his heartbeat on port 80, and the server pulled his jdocs. No token or voice request arrived during the session |

## Where the translation ended

M5 finished on 2026-09-20 and the translation is done, in the sense that every
Go line the user wants in Rust is in Rust. The file table adds up like this:

| | Go lines |
|---|---|
| translated | 9,309 |
| cut: the LLM and knowledge graph (M4) | 1,535 |
| cut: the five speech engines other than Vosk, with Houndify streaming | 635 |
| cut: the Go `.so` plugin loader | 81 |
| deferred: Bluetooth onboarding | 530 |
| left for M6: the tray notification helper, which needs the tray shell first | 40 |
| total | 12,130 |

What is left is M6: the robot debugging, the list below, the tray shell,
packaging and cutover, planned under "M6 plan" below. The 12,130 counts the
`chipper` module only. The Windows tray M6 translates lives in a second repo and
is counted in its own table. The one thing owed from M3 is still owed: the
voice pipeline has never met real audio, and the test is ten minutes with the
user present, running `chipper serve --features stt-vosk` against a copy of the
data directory and saying "Hey Vector, what time is it".

## What is deliberately not being translated

Three decisions the user made on 2026-09-20, all reversible, all recorded so nobody rediscovers them by accident.

**The five speech engines other than Vosk are cut**, 635 lines counting their entry points and the Houndify streaming glue: Coqui, Leopard, Whisper over the network, whisper.cpp, and Houndify. The user runs Vosk and wants none of the others. Houndify is the one worth a second line, because it is not a transcriber: it answers a spoken question outright, and it was wire-pod's original route for the knowledge graph. Cutting it follows from cutting M4 rather than being a separate judgement.

**The Go plugin loader is cut**, 81 lines. It opens `.so` files built by the Go toolchain, which Rust cannot do at all, so a translation would be a loader that finds nothing. The Lua host is the extensibility path this port has.

**Bluetooth onboarding is deferred**, 530 lines. The user wants it upgraded rather than translated. It is worth knowing that the Go server on their Windows machine does not have this feature either: the code sits behind a build tag that the Windows build does not set, so the seventeen Bluetooth routes on the web UI are stubs today. Translating it would add something rather than reach parity, and the Rust crate for it is weakest on Windows.

## M4 is not being translated

On 2026-09-20 the user decided not to translate the LLM and knowledge-graph work: `kgsim.go`, `kgsim_cmds.go` and `kgsim_interrupt.go`, 1,535 Go lines. They are not interested in the feature. The `wirepod-llm` crate stays a stub and the prep commit for the milestone was reverted, so the tree carries no scaffolding for it.

What this costs, so that the decision is reversible with open eyes. A voice command the intent list does not match reaches `intent_system_unmatched` instead of an answer, which is where the port already stood. The dashboard's talk panel and the knowledge-graph RPC have nothing behind them. `preqs` carries three `TODO(M4)` markers at the exact call sites, so picking the work up later means filling those three holes rather than finding them. Weather shared the milestone and is translated.

## M6 plan

Planned on 2026-09-26. M6 ends when the Rust server has replaced the Go one on this PC and the Go binary has sat unused beside it for a month. Four decisions the user made that day shape it:

- The Windows tray is translated from the Go wrapper, not dropped.
- Packaging covers this PC only.
- A 24-hour soak comes before the cutover.
- The findings list is triaged, not cleared.

Out of scope, by the same decisions and the earlier ones:

- a redistributable installer
- the Linux, Jetson and Docker builds from the old P9
- Bluetooth onboarding, which stays deferred
- everything cut in M4 and M5

### What the planning found

**The shipped debug lines are dropped before the log ring.**

- `serve.rs` installs its `EnvFilter` as a global filter, ahead of the `LogLayer`. The default filter raises only the three crate targets to debug.
- So a `debug` event on the component targets `sdkapp`, `stt`, `voice` and `conn` never reaches the ring or the console.
- That covers every motion, state and map line from the 2026-09-25 work, and the speech engine's debug lines.
- It was checked with a scratch program against the same `tracing-subscriber` 0.3.23.
- Go's ring keeps every level, and `DEBUG_LOGGING` gates only its stdout copy. `logger.rs` already says the filter belongs on the formatting layer.
- The runbook's advice to choose `debug` on the log page cannot help, because the lines are gone before the page asks.

**There is no supervisor.**

- The installed `chipper.exe` is the tray icon and the server in one process.
- It is built from the WirePod repo, listed under the file table. There, `cross/podapp` starts the tray and then runs its own copy of `startserver.go`.
- So the Rust binary has to become the tray, and replacing the Go server means replacing one file.

**The install matches this port's assets.**

- The installed build is the user's fork, version `v1.2.18-custom`.
- Its `webroot` is byte-identical to `assets/webroot`, and its `intent-data` differs only in line endings, so the Rust binary can run from the install folder as it is.
- Several things name the exact path `C:\Program Files\wire-pod\chipper\chipper.exe`: the per-program firewall rule, the `Run` key's `chipper.exe -d`, the uninstaller's registry entry and the shortcuts. They all carry over to whatever binary sits there.
- `libvosk.dll` and the MinGW runtime DLLs it needs are already beside it.

**Three gaps.**

- CI never builds the `stt-vosk` feature, because the libvosk import library is not in the repository. That feature is in the build that ships.
- Nothing in the port reads `WEBSERVER_PORT`. The tray sets it from the registry when the web port is not 8080, and Go reads it in `vars.go`.
- The jdocs pinger's mDNS browse is switched off by `set_mdns_enabled`, which no boot path calls. Go runs that browse when a conn check arrives from an address it does not know, which is how it follows a robot whose IP address has changed. Left off, an unattended Rust server loses him on the first address change.

### Stage 0: before any robot session

No robot is needed, and each item is its own commit.

1. **Fix the log filter.** Put the `EnvFilter` on the console layer only, as `logger.rs` describes, and let the `LogLayer` admit every level of wire-pod's own targets. A test checks that a debug event on `sdkapp` reaches the ring under the default filter. More lines then land in the 500-slot ring. That is Go's behaviour, and the motion window was designed with that budget in mind. In the same commit, `serve` opens `LOG_FILE` as Go's `logger.Init` does. `LogRing::with_log_file` already exists, but `serve` built the ring without it, and the soak needs the file. Done.
2. **Turn the mDNS browse on.** The jdocs pinger's browse is switched on in the full `serve` boot path, and never under `--web-only` or in tests. Done.
3. **Fix the CI flake.** `concurrent_writers_never_leave_a_torn_file` fails intermittently on Windows and can fail a CI run. Done: a process outside ours, most likely a scanner, held the new file open, and the rename retry now waits about a second.
4. **Stop printing keys.** `ApiConfig` and `BotInfo` must not print keys through their derived `Debug`. Done, for `Env` and the two on-disk bot-info types as well.
5. **Add a local packaged gate, since CI cannot run one.** It runs clippy and a release build with `--features stt-vosk`, with `VOSK_LIB_DIR` set as in `RUNBOOK-SERVE.md`. Run it before any push that touches the voice path, and always before a deploy. Done: `scripts/gate-packaged.sh`.
6. **Correct the docs.** Fix where `CLAUDE.md` and `RUNBOOK-SERVE.md` said the tray app supervises `chipper.exe`, name the WirePod checkout as read-only, and put the tray's registry keys out of bounds outside a session. This was done with this plan.

### Stage 1: robot sessions, with the user present

Every session follows `RUNBOOK-SERVE.md`:

- Quit WirePod from the tray.
- Run the Rust `stt-vosk` build against a copy of the data directory.
- End with the Go server back and `is_running` answering `true`.

Findings go on the list. The four sessions fit in one or two sittings of about an hour.

1. **Voice**, the test owed since M3. Ask "Hey Vector, what time is it", ask a weather question, and trigger a custom intent that has a Lua script attached. It passes when he answers each one, and the `stt` and `voice` lines show the transcription and the matched intent.
2. **Web UI and dashboard on the live robot.** Try the dashboard's controls, a settings change, the battery watchdog and the jdocs pinger. Measure the camera frame rate against the Go figure on the list, 3.55 frames per second.
3. **Motion and the map, on the floor.** Use the checklist from the 2026-09-25 plan:
   - Drive him from the dashboard, and from a script at priority 20.
   - Watch three things line up: the call, the state change or its absence, and the map.
   - Pick him up once and see the map reset twice.
   - Leave him idle and confirm the log stays quiet.

   Also check the robot-dependent items on the list from that work: cliff flicker, the state stream while he sleeps, and `BAD_TAG` after a server restart.
4. **Setup, optional.** Certificate and server-config generation run against the data copy only. SSH onboarding re-onboards him, so it runs only if the user asks for it on the day.

### Stage 2: triage the findings list

What the sessions find joins the list at the bottom. Each item goes into one of three groups.

**Fix.** Anything that breaks this PC's use, CI or key safety. Stage 0 already takes the known ones.

**Settle on the robot.** Items the sessions or the soak can decide:

- the retry on the cached connection in `pingJdocs`
- the battery watchdog holding an evicted connection
- the stim stream a closed tab leaves running
- the nav map channel's missing HTTP/2 keep-alive

**Keep as Go.** Items where the port matches Go on purpose, or where the difference is invisible:

- the camera hanging while he sleeps
- the missing 301 redirects
- the `get_ota` 500
- the unbounded token store
- the `gopher-lua-libs` preload
- the 404 for an unrouted gRPC path

The camera frame rate and the wake-before-camera fix are improvements, not parity, so they wait until after the cutover.

### Stage 3: the tray, translated

Stage 3 needs no robot, so it can run beside stages 1 and 2. The WirePod files in the table below become modules under `crates/wirepod-app/src/tray/`, following rule 1. Two of those Go files are copies of code already translated:

- `cross/podapp/web.go` is identical to `pkg/initwirepod/web.go`.
- `cross/podapp/initwirepod.go` is `startserver.go` with dialogs and tooltips added. Only those additions are translated, and the rest calls `startserver.rs`.

**What the translation must reproduce:**

- the tray menu: Quit, Web Interface, Config Folder, Run On Startup and About
- the tooltip changes
- the single-instance check against `LastRunningPID` under `HKCU\Software\wire-pod`
- the `NeedsRestart` check
- the crash dump to `%APPDATA%\wire-pod\dump.txt`
- the `-d` flag that suppresses the start-up message box
- the environment Go sets before starting the server: `STT_SERVICE=vosk`, and `WEBSERVER_PORT` when the registry's `WebPort` is not 8080. Edition 2024 makes `set_var` unsafe, so the tray does not set these variables. It writes both values into the `Env` that `Env::from_process` builds, and `WEBSERVER_PORT` gains a field there. Go also sets `DEBUG_LOGGING=true`. It has no counterpart here, because `logger.rs` sends the stdout copy through the formatting layer instead.

**Go's `vars.Packaged` becomes real.** It becomes a field on `AppState`, set only by the tray. Its consumers are:

- the two `TODO(M5)` markers in `ssh.rs`
- the port-bind failures in `webserver.go` and `server.go`, which show a message box in a packaged build

`pkg/logger/msg-winmac.go` and `msg-and.go` supply that message box. The box is Win32 on Windows and a log line elsewhere, as on Android.

**Build choices:**

- **Win32 calls.** The tray, the message boxes, the registry and the process check call Win32 directly through `windows-sys`, which `Cargo.lock` already carries. A tray crate with its own event loop would be a redesign of `getlantern/systray`, not a translation of the calls it makes.
- **The Open browser button.** zenity's extra "Open browser" button needs a task dialog, and a task dialog needs a Common Controls 6 manifest.
- **Resources.** `windows/cmd/rc/app.rc` embeds only the five `.ico` files. The manifest is an addition with no Go counterpart, and it goes on the list of deliberate differences.
- **The icons.** They are copied byte-identically from the WirePod repo into `crates/wirepod-app/resources/`, because `cargo xtask sync-assets` covers only the Go checkout.
- **The resource compiler.** Embedding needs `rc.exe` from the Windows SDK. It is installed here, under `10.0.26100.0`, but it is not on `PATH`.
- **The window subsystem.** A `tray` Cargo feature sets the Windows GUI subsystem, as Go's `-H=windowsgui` does. With it, a bare `chipper` or `chipper -d` starts the tray, which is how the Start menu and the `Run` key launch it. Without it, the binary stays the console program `serve` and `sdk-trial` need.
- **Threads.** The Win32 message loop owns the main thread, and the server runs on a Tokio runtime built beside it.

**Tests:**

- The `OSFuncs` interface is Go's own seam, so `podapp` is tested against a fake of it.
- Registry tests use a scratch key under `HKCU\Software\wire-pod-rs-test` and delete it afterwards. They are `cfg(windows)`, because the Ubuntu CI leg runs the same suite.
- No test reads or writes `HKCU\Software\wire-pod` or the `wire-pod` value under the `Run` key, because the running Go tray reads them.

Translated on 2026-09-26 by four lanes. The Win32 code for the tray icon, the menu and the dialogs has compiled and passed its tests but has not yet run on the desktop.

### Stage 4: packaging for this PC

The build is `cargo build --release -p wirepod-app --features stt-vosk,tray`.

`scripts/deploy-windows.ps1` replaces the Go binary in place. It is written, and it refuses any build that is not a tray build. It is modelled on the fork's `scripts/build-windows.ps1 -Deploy`, with one difference: it never stops a process by name. Run from an elevated PowerShell, it does the following:

1. Read `LastRunningPID` from the registry. Check that the PID belongs to the `chipper.exe` in the install folder, then stop it by that PID.
2. The first time only, rename the Go binary to `chipper-go.exe` beside it, and never overwrite that copy.
3. Copy the Rust binary in as `chipper.exe`.
4. Suffix the install's `version` file with `-rs`.
5. Launch it as the `Run` key does, with `-d` and the `chipper` folder as the working directory.
6. Wait up to thirty seconds for `is_running` to answer `true`.

`-Rollback` does the same in reverse and restores `chipper-go.exe` and the version file.

Nothing else in the install is touched. The firewall rule, the `Run` key, the uninstaller, the shortcuts, the DLLs and the assets all stay. Re-running the WirePod installer wipes the folder and brings back upstream Go, so it is not run during M6.

### Stage 5: soak and cutover

**Before the soak.** Make timestamped backups of `%APPDATA%\wire-pod` and `~/.anki_vector`. Then deploy the Rust binary with the stage 4 script.

**The soak is a standing session.** It runs the Rust server on the production ports with the real robot for 24 hours, mostly unattended. `CLAUDE.md` forbids that outside a session with the user present, so it needs the user's explicit go-ahead as one session, with a start and an end.

**The hourly check.** The soak runs with `LOG_FILE` set, because the 500-slot ring can wrap within an hour on an active robot. A script checks the server over localhost only and reads that file. It appends one line per check to a file in the scratch area, and it never copies a jdoc or anything else from the live state. Each line records:

- `is_running`
- the server's memory, read by its PID
- whether `escapepod.local` resolves
- whether the robot answers through the SDK routes
- his jdoc version numbers, and nothing else from those documents
- the count of behaviour-control grants against releases, from the log file now that stage 0 lets those lines in

The user talks to him as usual through the day.

**It passes when:**

- no control grant goes unreleased
- memory shows no upward trend
- mDNS answers all day
- jdoc versions only ever increase
- he stays connected
- voice works whenever it is used

**Rehearse the rollback.** Afterwards, run `-Rollback`. Let the Go server run for an hour on the state the Rust server wrote, and check the web UI and one voice command. This proves both directions are schema-safe.

**The cutover.** Deploy the Rust binary again. That is the cutover. Then:

- Keep `chipper-go.exe` and the backups for a month.
- Change the "Live environment" section of `CLAUDE.md` to name the Rust server as production.
- Close M6 in the progress table.

## Added on purpose, beyond the Go server

Translation stopped at 100% of what was in scope, and these were added after it.
Each one is a difference the Go server does not have, listed so that a reader who
diffs the two does not take it for a porting mistake. The feature they belong to
is the motion and map logging planned on 2026-09-20.

**Motion responses are logged rather than discarded.** Go throws away the answer
to every motion RPC, at all seven call sites. Those calls now pass through
`logged` in `crates/wirepod-vector/src/motionlog.rs`, which times the round trip,
decodes the response *body* and writes one debug line. The body is what matters:
the transport status is success whether or not the robot moved, so only
`ResponseStatus.code`, `PlayAnimationResponse.result` and `ActionResult.code` can
tell a robot that obeyed from one that ignored us. Behaviour control grants and
releases get a line for the same reason, since a missing control lock is the
usual cause. Debug level only, so the web UI's default log still matches Go's.

**Go's connect-time event stream is back, reading `robot_state`.** Go opens an
event stream the moment it connects to a robot (`robot.go:372-384`) and never
reads it; this port had left it out. It now opens with whitelist
`["robot_state"]` and the same empty connection id, so the robot's gateway treats
it exactly as it treats Go's, and it lives as long as the connection; the map
page reopens it if the robot ends it early. It writes a line only for an urgent
change (delocalization, localization, being picked up or held, a fall, a cliff)
or inside a three-second window after a motion call, where it says whether he
moved. The stim
stream is unchanged and still asks for `stimulation_info` alone. The code is
`crates/wirepod-core/src/robot/state_stream.rs` and `robotstate.rs`.

**A nav map feed, a page and a snapshot route.** `/navmap` serves a page
compiled into the binary, and `/api-navmap/snapshot` answers the robot's latest
map with every quad's position reconstructed, plus his latest state. The feed is
a `NavMapFeed` stream Go never opens, and it runs only while the page keeps
polling. The code is `crates/wirepod-core/src/robot/navmap.rs`, `navmap_feed.rs`
and `crates/wirepod-server/src/navmap/`.

**Two Lua globals.** `goToPose` and `lookAroundInPlace` have no Go counterpart.
Unlike the translated globals they return the robot's decoded answer, and a
timed-out `goToPose` cancels its queued action.

The engine facts all of this rests on are in section 6 of `docs/robot-api.md`.

## The robot's own API

`docs/robot-api.md` is the reference for what the robot exposes and expects, read out of the WireOS sources at `E:/GitHub/wire-os-victor`. Read it before translating anything that talks to the robot. Two findings from it change how the port should behave, and both are on the list below: taking behaviour control is the only SDK message that wakes a sleeping robot, and the robot's own `settings.proto` numbers its fields differently from the copy this port vendors.

## Facts worth keeping from the old plan

- Ports 80 and 8080 serve the same mux in Go, with every route on both. There is no method checking. `/api/` responses carry CORS `*`.
- The robot never verifies the JWT signature. It requires six claims as JSON strings: `token_id`, `token_type`, `user_id`, `requestor_id`, `iat`, `expires`.
- Audio: a first byte of `0x4F` means Ogg-Opus, anything else is raw 16 kHz s16le PCM. The high-pass filter resets its state on every chunk, and that is kept. VAD is WebRTC mode 2 on 320-byte frames, and speech ends at 23 inactive frames after more than 18 active ones.
- mDNS: instance `escapepod`, service `_app-proto._tcp`, port 8084.
- The magic constants (the app key, the global GUID and its hash document, the BLE auth token) are copied verbatim from `pkg/vars/vars.go` and the Go files that use them.
- The route list for the SDK app and the web API is `docs/archive/phases/P4-sdk-app/sdkapp-routes.md`. It is the checklist for M2.
- `docs/pending-upstream.md` tracks upstream wire-pod commits the Go fork has not merged.

## Debug and optimization list

Translation reached 100% of its scope on 2026-09-20. Stage 2 of the M6 plan triages this list.

From the motion and map logging work of 2026-09-25, to check against the robot:

- The state stream writes every flip of an urgent flag. If his cliff sensor
  flickers at a table edge, each flip takes a log-ring slot. Measure it before
  rate-limiting anything.
- A map page left open against a robot that refuses the nav map feed restarts
  the feed on every poll, one RPC a second.
- How the robot's gateway treats the state stream while he sleeps on the charger
  is unknown.
- The battery watchdog caches its own `Arc<RobotEntry>`
  (`crates/wirepod-server/src/sdkapp/batterywatchdog.rs`), so it can hold an
  evicted connection open. Closing a dashboard tab while Stim is showing never
  sends a stop, so that stream runs until the idle sweep.
- `goToPose` answers `code=...` for any action result `motionlog.rs` does not
  name; widen the list if the robot turns out to use others.
- Found by the pre-push audit of the same work, to check on the robot:
  - The nav map channel sets no HTTP/2 keep-alive, so a robot that drops off
    the network without closing the connection may leave the feed waiting on a
    dead stream, reported as waiting for a map, until another call on the
    channel fails.
  - He can flip between localized and dead reckoning when the charger is
    marked dirty and then seen again (`blockWorld.cpp:1622`), and each flip is
    an urgent line, so a roaming robot is not as quiet in the log as a still
    one.
  - The robot's gateway hands each action response to every waiting listener,
    so the `CANCELLED` result of a timed-out `goToPose` can answer a
    `goToPose` sent just after it.
  - The action tag counter starts again at 2000001 on every server start, and
    the Python SDK uses the same window, so a tag still pending on the robot
    makes the next `goToPose` answer `BAD_TAG`.
  - `to_number` differs from gopher-lua's `ToNumber` for strings with a leading
    zero, which gopher-lua reads as octal.

- `MakeLuaState` preloads `gopher-lua-libs` in Go, about thirty Go-written modules a script can `require`: json, http, strings, time, filepath and the rest. The Rust host gives a script mlua's own standard library instead, so a script that requires one of those modules fails where Go's would not. Nothing in the vendored web UI ships such a script, so this shows up only for a script the user writes.
- The SSH client has never spoken to the robot. `russh` negotiates key exchange and ciphers differently from Go's `golang.org/x/crypto/ssh`, and Vector runs dropbear, so the first real onboarding attempt is the test. `/api-ssh/setup` is the route to watch.
- Go's mux answers a 301 for a subtree prefix requested without its trailing slash. Only `/api-sdk` and `/api` do that here; `/api-chipper`, `/api-ssh`, `/api-lua` and `/session-certs` fall through to the 404 instead. Nothing the web UI or the robot sends uses the bare form, so this is invisible today.

- Fixed on 2026-09-26: `concurrent_writers_never_leave_a_torn_file` in `crates/wirepod-core/tests/persist.rs` failed intermittently on Windows with `Access is denied`. The conflicting handle was not another writer: instrumented runs of the concurrent writers never had a single rename refused, because std falls back to a POSIX-semantics rename when `MoveFileExW` finds the target open. It was a process outside the test, most likely the on-access scanner or the search indexer, holding the freshly written target open without delete sharing for longer than the rename retry's budget of four attempts over 14 ms. A stand-in that holds each new version of the target for 30 ms reproduces the failure, and the production code can hit it too, because Go's `os.WriteFile` never renames and the port does. The budget is now ten attempts over about a second, close to the two seconds Go's own toolchain retries renames for, and a directory in the target's place, which answers the same error, is no longer waited on. `concurrent_writers_outlast_a_scanner_holding_the_target` pins it.

From the browser session of 2026-09-19, with the robot attached to the Rust server:

- `/cam-stream` gives nothing while the robot is asleep on its charger, on the Go server as well as on this one: `EnableImageStreaming` times out after five seconds and `CameraFeed` never sends headers, so the request hangs until the client gives up. Awake, the Go server streams normally. Measured on the Go server on 2026-09-20 with the robot awake: first frame 0.98 s after the request, then 71 frames in 20.0 s, which is 3.55 frames per second or about 282 ms between frames, 7.7 kB per frame, 547 kB in total, against a robot round trip of 12 to 22 ms. That frame rate is well under what the camera can do and is worth investigating. The same measurement has not yet been taken against the Rust server.
- The camera hang has a real fix rather than a deadline: requesting behaviour control is the only SDK message registered as a wake reason, so taking control first wakes a sleeping robot and the camera calls then answer. Go papers over it with a five-second client deadline instead.
- `UpdateSettings` over binary gRPC would write the wrong fields. The robot's `settings.proto` carries `custom_eye_color = 3` and shifts every field after it by one, and both the vendored proto and the SDK wire-pod links have the unshifted numbering. The port is safe only because, like Go, it sends settings as JSON over the REST mirror, where the gateway matches on field names. Do not switch those routes to gRPC.
- Every action RPC hangs forever without behaviour control, because the robot only produces the completion response while the SDK behaviour is active. Every such call needs a deadline.
- Refusals are usually invisible: the robot's gateway overwrites the engine's `FORBIDDEN` with `RESPONSE_RECEIVED` on the vision toggles, `SetEyeColor` has no response path at all, and the four direct motor calls report success while doing nothing.
- Neither server reads the timestamp the robot sends on each camera frame, so glass-to-glass latency cannot be measured. Reading it would be an addition rather than a translation.
- Go's `get_ota` indexes a path segment that the only matching route cannot have, so the handler panics on every call. The port answers Go's own `failed to parse URL` 500 instead, and the proxy below it is unreachable in both.
- `print_robot_info` prints the robot's GUID in Go. The port leaves it out.
- The `sdkapp` log target is not in the default filter, so lines logged to it at debug never appear. The same holds for `stt`, `voice` and `conn`, and the cause was that `serve` filtered globally, ahead of the ring. Fixed in M6 stage 0: the filter is on the console layer only.

Checked against the running Go server, web-only on 18080 beside it:

- Every static path matches byte for byte with the same headers: `/`, `/index.html`, the CSS and JS, `/sdkapp/*`, the 404 and `/ok`.
- Of the twelve read-only `/api` routes, nine match in status, headers and body shape. The three that differ do so because of the environment, not the code. The STT provider is blank because both servers overwrite it from `STT_SERVICE`, which the tray app sets and a shell run does not. The log routes differ only by uptime, and the entry keys and types match. `get_version_info` reports from source because `assets/` carries no `version` file where the install does.
- `/cam-stream` produces nothing on the Go server either, with the same robot in the same state, so the port matches Go here.
- Running `chipper serve` from a shell with `STT_SERVICE` unset will blank the STT provider in `apiConfig.json`. Set `STT_SERVICE` and `STT_LANGUAGE` to match the tray app before pointing the server at the live data directory.
- `assets/` does not vendor the `version` file the install has, so `get_version_info` always reports from source.

- A JSON key with a malformed Unicode escape makes the Rust decoder drop the whole file where Go loads it (`gojson::merge_object`).
- The in-memory token store grows without bound, as in Go.
- Session-certificate writes build a fresh write gate per call.
- `botSdkInfo.json` is written through a fresh write gate on every call too, in `store/bot_info.rs` and in `jdocspinger.rs`, so nothing orders two writers of that file in production. The M6 flake work measured that this was not the flake's cause.
- `a_hold_that_clears_does_not_lose_the_write` in `tests/persist.rs` depends on timing: with 48 copies of the test binary running at once, the hold often clears before the first rename, and its `attempts > 1` assertion fails. Normal CI load does not come close.
- `ApiConfig` and `BotInfo` derive a full `Debug` and could leak keys into a log line.
- `mdns_sd` logs `failed to send response of shutdown` every 32 seconds, each time the registration loop re-registers. The name still resolves.
- An unrouted gRPC path on the TLS listener answers the router's 404 rather than tonic's `Unimplemented`, because `/ok:80` is matched in the fallback.
- `pingJdocs` retries on the cached robot connection where Go dials a second one.
- The mDNS browse in `jdocspinger.rs` sits behind `set_mdns_enabled`, which no boot path turns on yet. Go uses it to follow a robot whose address changed. Fixed in M6 stage 0: the full `serve` boot path turns it on.
- Log lines with ANSI colour codes print the escape as text under the `tracing` formatter.
- `/api-chipper` without the trailing slash has no redirect to `/api-chipper/`.
- `download.rs` skips zip entries that would leave the destination, and ignores file modes.
