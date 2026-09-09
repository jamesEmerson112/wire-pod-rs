# Phase documents

The port runs in eleven phases, P0 through P10. This index is the map; each row links to the document that holds the goal, scope, exit criteria, and dependencies for that phase.

| Phase | Title | Status | Document |
| --- | --- | --- | --- |
| P0 | Foundations + de-risk spikes | done (spikes landed) | [P0-foundations.md](P0-foundations.md) |
| P1 | Robot connects & authenticates | not started | [P1-robot-connect-auth.md](P1-robot-connect-auth.md) |
| P2 | Voice commands | not started | [P2-voice-commands.md](P2-voice-commands.md) |
| P3 | LLM/KG/weather/commands | not started | [P3-llm-kg-weather-commands.md](P3-llm-kg-weather-commands.md) |
| P4 | SDK app + web UI + camera | spec written; early slice landed on master through C12 (core, vector, the ten `/api-sdk` slice routes, `/api/get_bot_status`, `/ok`; 207 tests); P4 proper not started | [P4-sdk-app.md](P4-sdk-app.md) |
| P5 | Watchdog, logger polish, Lua | not started | [P5-watchdog-logger-lua.md](P5-watchdog-logger-lua.md) |
| P6 | WASM plugins | not started | [P6-wasm-plugins.md](P6-wasm-plugins.md) |
| P7 | Setup: certs, SSH, BLE | not started | [P7-setup-certs-ssh-ble.md](P7-setup-certs-ssh-ble.md) |
| P8 | Remaining STT engines | not started | [P8-stt-engines.md](P8-stt-engines.md) |
| P9 | Packaging | not started | [P9-packaging.md](P9-packaging.md) |
| P10 | Cutover + soak | not started | [P10-cutover.md](P10-cutover.md) |

This folder is where the project's own knowledge lives, phase after phase, instead of in home-directory notes. There is one document per phase, named for its number and a short title, and each one is written before the phase starts and updated as it runs. The master plan those documents were seeded from is copied into the repository at [../plan.md](../plan.md), which is the authoritative copy and carries an Amendments section for decisions that supersede it. The `P4-sdk-app/` subfolder holds the parity specification for the SDK app, which is large enough to need several documents of its own: the route table, the ownership state machines, the camera stream contract, the dashboard client contract, the mapping from the Go tests, the recorded deviations, and the design of the early implementation slice. The file [pending-upstream.md](pending-upstream.md) tracks commits on the upstream `kercre123/wire-pod` repository that the fork has not merged, so that a deferred decision stays visible instead of being forgotten.
