# RUNBOOK: run the Rust server against the real robot

Goal: put Vector (ESN 00303f28, 192.168.8.203) on the Rust server instead of the Go one for a while, then put him back. This is the procedure that was used on 2026-09-19, when he first completed a TLS handshake with the Rust listener and read his jdocs from it.

The Go server and the Rust server cannot run at the same time in this mode, because both want ports 80, 443, 8080 and 8084 and the mDNS name `escapepod`. Voice commands need the build described under "Voice commands" below.

## Switch to Rust

1. Quit WirePod from its tray icon. The icon and the Go server are one process, `chipper.exe`, so this stops the server, and quitting this way also clears its `LastRunningPID` in the registry. Check in Task Manager that `chipper.exe` is gone.
2. Once only, in an elevated PowerShell, allow the Rust binary through Windows Firewall. Windows allows inbound connections per program, and the installed rule covers only the Go binary.
   `netsh advfirewall firewall add rule name="wirepod-rs-test" dir=in action=allow program="E:\GitHub\wire-pod-rs\target\debug\chipper.exe" enable=yes`
3. Make a working copy of the state directory, so the live state stays untouched. From Git Bash:
   `cp -r "$APPDATA/wire-pod/." <scratch>/data/`
4. Build and start the server from the repo root:
   `cargo build -p wirepod-app`
   `./target/debug/chipper.exe serve --data-dir <scratch>/data --asset-dir assets --sdk-ini-dir <scratch>/ini`
   With `--packaged` instead of `--data-dir` it uses the live `%APPDATA%\wire-pod`, which is what a real cutover will do.

   Set `STT_SERVICE` and `STT_LANGUAGE` to whatever the tray app uses before pointing the server at the live data directory. Go overwrites the config's STT provider from `STT_SERVICE` on every read, and so does this port, so a shell run without it blanks that setting in `apiConfig.json`.

   Stop the server by its process id, never by its image name. The installed Go server is also called `chipper.exe`, so `taskkill /F /IM chipper.exe` takes both down.

## Voice commands

The speech engine links against libvosk, whose import library is not in this repository, so it is behind a Cargo feature that is off by default. A plain build therefore answers the three streaming voice RPCs as unimplemented. To build it in, from Git Bash:

```bash
export VOSK_LIB_DIR=E:/GitHub/wire-pod-rs/spikes/s2-vosk/vendor/vosk-win64-0.3.45
export PATH="$PATH:/e/GitHub/wire-pod-rs/spikes/s2-vosk/vendor/vosk-win64-0.3.45"
cargo build -p wirepod-app --features stt-vosk
```

`VOSK_LIB_DIR` is needed at link time and the same directory has to be on `PATH` at run time for `libvosk.dll`. The engine also needs a model under `<data dir>/vosk/models/<language>/model`, which the copied data directory already carries.

## What a healthy start looks like

Within a few seconds the log shows the four listeners, `Registering escapepod.local on network`, then from the robot:

```
tls connection from 192.168.8.203:<port>
192.168.8.203:<port> POST /jdocspb.Jdocs/ReadDocs
pulled jdocs bot=00303f28
```

`netstat -ano | findstr 192.168.8.203` shows his heartbeat connection to port 80 and the server's own connection to his port 443. `ping escapepod.local` from this PC answers with this machine's address.

The build output is locked while the server runs. To build or test in the meantime, use another output folder: `CARGO_TARGET_DIR=E:/GitHub/wire-pod-rs-target-gate cargo test`.

## Switch back to Go

1. Stop the Rust server with Ctrl-C, or by its process id (`netstat -ano | findstr :8080` names it). Never by the image name: the installed Go server is also `chipper.exe`.
2. Only then launch WirePod from the Start Menu, wait about thirty seconds, and check `curl http://localhost:8080/api/is_running` answers `true`. If WirePod starts while the Rust server still holds the ports, the Go server binds only the IPv6 side of each one and Vector stays on the Rust server until it stops; stopping it then hands everything to Go, as on 2026-09-27.
3. The firewall rule can stay for next time. To remove it: `netsh advfirewall firewall delete rule name="wirepod-rs-test"`.

## Web UI only, beside the Go server

To look at the Rust web UI without taking Vector off the Go server, serve HTTP only on other ports:

`./target/debug/chipper.exe serve --web-only --bind 127.0.0.1 --web-port 18080 --http-port 18082 --data-dir <scratch>/data --asset-dir assets --sdk-ini-dir <scratch>/ini`

`--web-only` starts no TLS listener, does not bind 8084 and registers nothing on mDNS.

## The camera

The dashboard's camera view and `/cam-stream?serial=00303f28` share his one camera feed. As on the Go server, the newest viewer takes it and the earlier one freezes. The dashboard's view re-dials after 12 seconds without a frame, which takes the feed back, so two viewers should alternate every 12 to 20 seconds. That alternation is read from the code and has not yet been watched on a robot. While he sleeps on his charger the camera sends nothing, on either server.

## The energy estimate

The battery voltage he reports does not change while he is off the charger, so the server keeps its own estimate of his remaining energy. `docs/translation.md` describes the model under "Added on purpose, beyond the Go server", and section 3.8 of `docs/robot-api.md` explains the voltage.

**Reading it.** Open `http://localhost:8080/api-energy?serial=00303f28`. It answers JSON. `energy_percent` is 100 at a full charge and 0 when his low-battery flag rises, which still leaves him about four minutes before he shuts down. `minutes_left` is the time until that flag. `runtime_minutes` and `charge_minutes` are the model's current figures, and `runtime_learned` and `charge_learned` say whether each one has been learned from him yet. These four figures are rounded to one decimal. `on_charger` is his charger state, `guess` is true while the estimate is still the first guess, and `since` is in Unix seconds. `known` is true once a battery poll has confirmed the estimate since the server started. A serial the server has never seen answers only `serial` and `known: false`. After a restart, the estimate `energy.json` held answers in full with `known: false` until the first poll. The route only reads the estimate: it never dials him and never resets the idle timer. The battery watchdog is what feeds the estimate, and it does not run in the web-only mode above, so on port 18080 the route only shows what `energy.json` held when that server started.

**The log lines.** These are at info level or above, so the web UI's log page shows them without choosing `debug`. Each trip writes a line when he leaves the charger and another when he comes back, and the second one shows what the trip taught the model:

```
energy: left the charger at ~92%, about 18 min before his low-battery flag (runtime 20 min)
energy: back on the charger after 19 min off; estimate ~5%, his docking reading 3.62V gives ~0%; runtime 20 -> 20 min
```

`runtime 20 -> 20 min` is the old runtime and the new one. A trip that ends with his low-battery flag writes `energy: low-battery flag after 21 min off the charger; runtime 20 -> 21 min`, and a full charge writes `energy: charged full after 48 min on the charger; charge time 60 -> 54 min`. When the estimate reaches `gohome_percent`, the watchdog writes `battery watchdog: energy low (~24% <= 25%, about 5 min left), sending robot to charger` and drives him home as it does for a low voltage.

**The first estimate is a guess.** The first time the server sees a robot, it starts from his reported voltage and says so: `energy: first sight of him on the charger; starting from a guess of ~48% from his reported 3.87V`. That voltage is stale if he is off the charger at the time. The estimate stays a guess until he first docks or leaves the charger. After a restart the server resumes from the file and writes `energy: resumed from energy.json at ~60%, on the charger`. If his charger state changed while the server was down, it assumes the change happened at the restart: `energy: he left the charger while the server was down; assuming it happened now, at ~80%`.

**After a battery change.** The model is kept per robot in `energy.json` at the root of the data directory. That is `%APPDATA%\wire-pod` for the installed server and under `--packaged`, and the `--data-dir` folder otherwise. Go never reads the file. To make a robot start again from the seeds, stop the server from its tray icon or by its process id, never by its image name. Then delete his entry from `energy.json` and start the server again.

## Watching what he does: the motion log and the nav map

**Safety first.** Behaviour control at priority 10 (`OVERRIDE_BEHAVIORS`) turns off his cliff reaction until it is released. wire-pod takes that priority for voice-triggered speech, for the battery watchdog's drive home, and for the dashboard's control request when it asks for `high`; a Lua script takes whatever it passes to `assumeBehaviorControl`. Under it he neither stops at a drop nor records it in his map. Drive him only on the floor. A script that needs control to drive him should ask for priority 20 instead, `assumeBehaviorControl(20)`, which gives full control while keeping the cliff reaction on (`SDKDefault.json` sets `disableCliffDetection` false; `SDKOverrideAll.json`, priority 10, sets it true).

**The map page.** With the server running, open `http://localhost:8080/navmap` (or `http://127.0.0.1:18080/navmap` in the web-only mode above). It is served on the plain HTTP ports only, like the rest of the web UI. It draws his nav map live, nose-up, in the colours his engine uses, with his pose on top. The feed runs only while the page is open and stops by itself about 15 seconds after the page stops polling. He only sends his map when it changes, so drive him a little before expecting one. Picking him up wipes the map twice, once when he leaves the ground and once when he is put down, and the page says so. `?demo=1` shows a built-in example without a robot.

**The log lines.** All of these are at debug level, so choose `debug` in the web UI's log page to see them.

- `motion DriveWheels(lw=50 rw=50) -> REQUEST_PROCESSING in 14ms` is a call we sent and the decoded body of his answer. The four direct-motor calls answer success whether or not he moved, so this line alone never proves he did.
- For three seconds after a motion call, the state stream watches only the flags that answer that call: `wheels_moving` and `pathing` for a drive or a `goToPose`, `head_in_pos` for a head move, `lift_in_pos` for a lift move, and the animation and motor flags for an animation or a look-around. A change there is written as `state +[wheels_moving] pose=(120.4, -33.1) heading=0.781rad origin=3`. His own idle animations flip other flags and write nothing, but the stream cannot tell our call's effect apart from his own motion on the same flag.
- `no movement after DriveWheels(lw=50 rw=50)` means none of those flags changed in the three seconds: he ignored the call. `no change after DriveWheels(lw=50 rw=50): [wheels_moving] throughout` means he was already moving and stayed so. When a newer call replaces one after at least half a second, the older call is judged then, naming both: `no movement after DriveWheels(lw=140 rw=140) in the 1.2 s before DriveWheels`. The dashboard sends a stop twice when a key is released, so an ignored drive from the dashboard writes that line and then a `no movement after DriveWheels(lw=0 rw=0)` for the stop.
- Urgent changes appear whenever they happen: `state delocalized: origin 3 -> 4`, `state localized to object 7`, `state on odometry alone: no longer localized to object 7`, and a `state` line when he is picked up, held, falling or at a cliff.
- `behavior control granted at OVERRIDE_BEHAVIORS; cliff detection is off until release` marks the hazard above.
- A nav map summary appears when the feed starts and stops, on every origin change, and at most once a minute otherwise.
- `the robot's gateway cut a nav map short at 510 quads (3 so far); kept the last whole map` appears at most once a minute. His gateway sometimes sends a map without its last batches of quads, and about one map in seven arrived that way in the first session. The page keeps showing the last whole map, and the stop line counts these separately from malformed maps.

**When state lines appear.** The state stream opens when the server connects to him: when the SDK dashboard or the map page is opened, when a Lua script runs, and whenever the battery watchdog or the jdocs pinger reaches him. The server never dials him just to watch, so during a voice session with nothing else open there may be no state lines at all; open the map page to get them. If he ends the stream while the connection lives, after a reboot of his gateway or a network blip, the map page reopens it on its next poll, and until then the page shows no pose rather than a stale one.

**Driving him from a script.** `goToPose(x_mm, y_mm, angle_rad)` and `lookAroundInPlace()` return his answer as two words, the status and the result, such as `RESPONSE_RECEIVED SUCCESS`. The robot sets the status to `RESPONSE_RECEIVED` for these whatever happened, so test the second word, for example `string.find(answer, "SUCCESS")`. Both answer only while the script holds behaviour control; without it they give up after 30 seconds and cancel what they asked for, `goToPose` by its tag and `lookAroundInPlace` with `CancelBehavior`. A first script to try, on the floor:

```lua
assumeBehaviorControl(20)
sayText(goToPose(200, 0, 0), false)
sayText(lookAroundInPlace(), false)
releaseBehaviorControl()
```

Run it by posting `{"esn":"00303f28","script":"..."}` to `/api-lua/run_script`, with the script JSON-escaped onto one line, or attach it to a custom intent. From Git Bash: `curl -X POST http://localhost:8080/api-lua/run_script -d '{"esn":"00303f28","script":"..."}'`. From PowerShell, where `curl` is a different command: `Invoke-RestMethod -Method Post -Uri http://localhost:8080/api-lua/run_script -Body '{"esn":"00303f28","script":"..."}'`. The coordinates are in whatever frame he is in when the call arrives. That frame's origin is where he was last put down, or where he started up if he has not been lifted since, facing the way he faced then. So `(200, 0)` means 200 mm straight ahead of that spot, not 200 mm ahead of wherever he is now; the map page shows both.
