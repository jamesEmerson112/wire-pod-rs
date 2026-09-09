# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this repo is

A Rust port of the Go wire-pod `chipper` server, the voice backend for Anki/DDL Vector robots. The goal is a drop-in replacement: the same gRPC, HTTP, and mDNS contract and the same on-disk state files under `%APPDATA%\wire-pod`, so cutover and rollback are "stop one server, start the other". The Go server in the sibling checkout `../wire-pod` (`C:/Users/voan2/Documents/GitHub/wire-pod`) remains the production server until cutover and is the reference implementation for every porting decision. Its `CLAUDE.md` documents the Go architecture and package layout, and `chipper/pkg/` is the source to read when porting a behavior.

Current state: the port runs in eleven phases, P0 through P10. P0 is done, and most crates under `crates/` are still one-line stubs whose `//!` doc comment states the crate's intended responsibility. Three are not. `wirepod-core`, `wirepod-vector` and `wirepod-server` carry the P4 SDK-app early slice, which was built ahead of P1 through P3 because it ports work done in the Go fork in September and because it is unit-testable without a robot. `wirepod-core` holds robot identity, the injectable timing constants, Go-compatible number formatting, the robot seam expressed in domain types, the per-robot camera and event-stream ownership state machines, the never-pruned camera meters, the connection registry, the bot-info and pinger stores, and `AppState`. `wirepod-vector` holds the tonic implementation of that seam: the client that attaches the SDK bearer metadata to every RPC, the stream adapters, the `tonic::Status` conversion, the TLS connector that dials port 443 with the SDK's accept-all certificate verifier, the connection factory, and a loopback fake robot for tests in both plaintext and TLS. `wirepod-server` holds the axum router and the slice's handlers under `/api-sdk/*`, `/api/*` and `/ok`, and binds no ports.

The approved plan (phase list, crate map, locked architecture decisions, parity test strategy, risks, magic constants) is copied into the repository at `docs/plan.md`, which is the authoritative copy and carries an Amendments section for decisions that supersede it. Per-phase detail lives in `docs/phases/`, one document per phase, indexed by `docs/phases/README.md`; the `P4-sdk-app/` subfolder holds the SDK-app parity specification. Read those before starting phase work; this file only summarizes them. The plan files under `~/.claude/plans/` are working notes, not the record.

## Commands

The toolchain is pinned to Rust 1.92.0 in `rust-toolchain.toml`, edition 2024. CI (`.github/workflows/ci.yml`, Windows and Ubuntu) runs exactly four commands: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo build` and `cargo test`. Locally the gate before every commit that touches code or Cargo configuration is those four plus two wider clippy runs, and all six have to be green:

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --all-features -- -D warnings
cargo build
cargo test
```

The two extra clippy runs are what lint the test binaries and the feature-gated `test_support` modules, which the plain clippy command never compiles. `Cargo.lock` is a gate as well: the SDK-app slice resolves no new packages, so the set of `(name, version, checksum)` triples must not move, and this must print nothing.

```bash
diff <(git show origin/master:Cargo.lock | grep -E '^(name|version|checksum) = ') <(grep -E '^(name|version|checksum) = ' Cargo.lock)
```

Default workspace members are `crates/*` and `xtask`. Spikes are only built when named (`-p s1-tls-listener`) or with `--workspace`.

Run one test file or one test:

```bash
cargo test -p wirepod-proto --test roundtrip
cargo test -p wirepod-proto --test roundtrip jdoc_roundtrip
```

Asset sync. `.cargo/config.toml` defines the `xtask` alias, so the short form works:

```bash
cargo xtask sync-assets --from ../wire-pod --check   # report drift, exit 1 if any
cargo xtask sync-assets --from ../wire-pod           # copy the drifted files, report the count, rewrite assets/MANIFEST.sha256
```

Real-robot trial of the SDK-app slice. It serves the router on `127.0.0.1:18080` and dials the robot over TLS while the production Go server keeps running on 8080; `RUNBOOK-SDK-TRIAL.md` is the procedure and `scripts/sdk-trial-diff.sh` compares the two servers request by request:

```bash
cargo run -p wirepod-app -- sdk-trial
bash scripts/sdk-trial-diff.sh
```

Spikes:

```bash
cargo run -p s1-tls-listener -- --selftest      # in-process probes; no robot, no elevation needed
cargo run -p s1-tls-listener -- --tls-port 443 --http-port 80 --mdns --alpn on   # live robot test; follow RUNBOOK-S1.md
cargo run -p s2-vosk            # needs spikes/s2-vosk/vendor/vosk-win64-0.3.45/ (see its README) and libvosk.dll on PATH
cargo run -p s3-audio
cargo run -p s4-script-hosts    # the extism stage reports SKIP unless vendor/count_vowels.wasm exists
```

`.cargo/config.toml` sets `CMAKE_POLICY_VERSION_MINIMUM=3.5` so the vendored libopus in `audiopus_sys` builds under CMake 4.x. Keep it.

## Architecture

### Crate map

Each crate replaces specific Go packages under `wire-pod/chipper/pkg/`. Keep this mapping when deciding where ported code goes.

| Crate | Replaces in Go | Responsibility |
|---|---|---|
| `wirepod-proto` | protos from `digital-dream-labs/api` and `fforchino/vector-go-sdk` | Generated tonic/prost code, compiled from the vendored protos at build time. |
| `wirepod-core` | `vars`, `logger` | `AppState` replacing the ~30 unsynchronized globals, config, path resolution, logger ring, jdocs/botinfo/session-cert stores, pinger. The P4 slice landed identity, timings, `gofmt`, the robot seam, ownership, meters, the registry, the bot-info and pinger stores and `AppState`; config, paths, the logger ring and the jdocs and session-cert stores are still to come. |
| `wirepod-audio` | `wirepod/speechrequest` | Ogg/Opus decode, high-pass + gain filter chain, VAD, `SpeechRequest`. Pure and golden-tested. |
| `wirepod-stt` | `wirepod/stt/<engine>` | `SttEngine` trait with engines behind Cargo features (`stt-vosk` default), selected at runtime. Replaces Go's six per-engine binaries. |
| `wirepod-intent` | intent matching in `wirepod/ttr`, `wirepod/localization` | Keyphrase matching over the 14 locale files, parameter extraction, words2num. Pure and table-tested. |
| `wirepod-llm` | provider side of `ttr/kgsim*.go` | Provider layer, reasoning-model params, streaming sentence splitter. |
| `wirepod-vector` | `vector-go-sdk` usage | Outbound robot client: ExternalInterface RPCs, `update_settings` REST, port 8889 consolevar. The P4 slice landed the tonic client, the stream adapters, the error conversion, the connection factory, the loopback fake and the rustls TLS dialler that reproduces the SDK's `WithInsecureSkipVerify`; the REST and consolevar calls are still to come. |
| `wirepod-plugin-api`, `wirepod-plugins` | `ttr/plugins.go` (Go `.so`), `scripting` (gopher-lua) | Extism WASM plugin host and mlua Lua 5.1 host. |
| `wirepod-ttr` | `wirepod/ttr` | `ConversationTask` (unifies StreamingKGSim and DoGetImage), weather, battery watchdog. |
| `wirepod-server` | `initwirepod`, `servers/*`, `mdnshandler`, `wirepod/config-ws`, `wirepod/sdkapp` | TLS listener, tonic services, axum router, mDNS, restart supervisor. The P4 slice landed the router, Go's path canonicalization and 404, the `FormValue` merge, the byte-exact reply literals and the SDK-app handlers it covers; nothing in the crate binds a port yet. |
| `wirepod-setup` | `wirepod/setup` | Cert generation, SSH onboarding, BLE behind feature `ble`. |
| `wirepod-app` | `cmd/<engine>/main.go` | The `chipper` binary: CLI and wiring. Phase 9 adds the Windows tray shell. |

### Slice conventions

These hold across the three crates that carry P4 slice code, and code added to them should keep them.

- Crate direction is `proto <- vector` and `core <- vector <- server`, with `wirepod-server` also depending on `wirepod-core` directly. `wirepod-core` depends on neither `wirepod-proto` nor tonic: the robot seam is a set of traits in domain types, `wirepod-vector` implements them over the generated gRPC client, and `wirepod-server` consumes them. That is what keeps `AppState` at the bottom of the graph.
- axum is 0.7, not the 0.8 the older plan text names. tonic 0.12 depends on axum 0.7 and `Routes::into_axum_router()` hands back an axum 0.7 `Router`, so the two versions move together.
- Test helpers that both `src` and an integration-test binary need live in `src/test_support.rs` behind a Cargo feature named `test-util`, and the crate lists itself as its own dev-dependency with that feature on. Items behind `#[cfg(test)]` are invisible from an integration-test binary, and the self dev-dependency is what turns the feature on for the test build only, so plain `cargo test` works while `cargo build` compiles none of it.
- `wirepod-core` and `wirepod-server` carry `#![deny(clippy::await_holding_lock)]`. The ownership state machines guard their state with `std::sync::Mutex` and no guard ever crosses an `.await`, which is what lets them be driven from plain `#[test]` functions with no runtime. The one lock that is genuinely held across an await, the per-robot camera op lock, is a `tokio::sync::Mutex`.
- Every duration the slice waits on is a field of `Timings`, injected through `AppState` rather than read from a constant at the call site. Tests never use tokio's `start_paused`: they run zero settles and millisecond deadlines against the real clock, with a generous ceiling around the assertion so a regression times out instead of hanging.
- Go's `gen` counter is `Generation` here and its fields are named `generation`, because `gen` is a reserved keyword in edition 2024.
- Every deliberate difference from the Go server is numbered in `docs/phases/P4-sdk-app/deviations.md`, currently 1 through 23, with what differs, why, and where it is tested. A difference that is not listed there is a bug, not a decision.

### wirepod-proto

`build.rs` compiles five root proto files with protox (pure Rust, no protoc install) and tonic-build, with include paths `proto` and `proto/vector`. The generated modules are `chippergrpc2` (the inbound voice service the robot dials), `jdocspb` and `tokenpb` (inbound), and `anki::vector::external_interface` (the outbound SDK surface). The protos are vendored byte-identical from the Go module cache. The only permitted edits are listed in `crates/wirepod-proto/DEVIATIONS.md`; record any new deviation there. `tests/api_surface.rs` is a compile-time assertion that the load-bearing services and types exist, and `tests/roundtrip.rs` pins field numbers through prost encode/decode.

### Voice request flow being ported

The robot streams audio over gRPC to the chipper streaming handlers in the server crate. The audio crate sniffs the first byte (`0x4F` means Ogg-Opus, anything else is raw 16 kHz s16le PCM), decodes, runs the filter chain, and runs VAD to find end of speech. The STT crate produces text, or a ready-made intent for engines such as Houndify. The intent crate matches text against locale keyphrases and custom intents, or the ttr crate handles it as an LLM or knowledge-graph conversation. The intent result goes back to the robot over gRPC, and any spoken response is driven separately through the vector crate.

### Listener design (proven by spike S1)

One rustls TLS listener on 443 advertises ALPN `h2` and `http/1.1` and is served by hyper-util's auto builder, which sniffs the HTTP/2 preface when ALPN is absent. This replaces Go's cmux. It routes the three gRPC services (chipper, jdocs, token) plus the `/ok` and `/ok:80` HTTP conn-check endpoints. The `/ok:80` path has a literal colon and is the robot's liveness heartbeat, so test it against axum's matcher explicitly. A second plain-HTTP listener on 80 serves the robot's step-1 conn-check. In Go, ports 80 and 8080 serve the same mux with every route on both. mDNS registers `escapepod` on `_app-proto._tcp` port 8084 via mdns-sd. A supervisor built on a CancellationToken and `serve_with_shutdown` replaces Go's `RestartServer()` and must await the old task before rebinding.

## Parity rules

- The contract is bug-for-bug wherever the robot, the web UI, or on-disk state can observe a difference. `spikes/s3-audio/src/dsp.rs` shows the standard: Go's low-pass alpha inside a high-pass recurrence, with filter state reset on every chunk, is preserved and commented as a deliberate quirk.
- Token and GUID hashing, JWT claim shape, `server_config.json`, the mDNS record, and the magic constants must match Go byte-for-byte. Existing jdocs on disk hold Go-produced hashes, so hash parity is tested against a live Go value.
- Every persisted state struct takes `#[serde(default)]` plus a `#[serde(flatten)]` extra map so unknown and fork-only fields survive a round trip. This is what makes rollback to the Go server safe.
- A short list of Go bugs may be fixed (unknown LLM provider nil client, `||`-less LLM command panic, discarded DoSayText sanitize, action-enum collision, unchecked jdocs `Items[0]`). The `/api/get_kg_api` plaintext key stays because the web UI depends on it. The full list is item 8 of the core architecture decisions in `docs/plan.md`.

## Assets are a contract

`assets/` is vendored byte-identically from the Go repo: `webroot/` (the web UI, no build step), `intent-data/` (14 locales), `epod/` cert and key, `weather-map.json`, `stttest.pcm`, and `pod-bot-install.sh`. The vendored copy is current as of the Go fork's `81fa3b3`. `assets/MANIFEST.sha256` is the SHA-256 manifest that xtask maintains, and `ASSET_MAP` in `xtask/src/main.rs` is the source-to-destination mapping. Never hand-edit under `assets/`; change the Go repo and re-sync.

Line endings are policy, recorded as D11 and as deviation 5 in `docs/phases/P4-sdk-app/deviations.md`. Text assets are stored with the CRLF bytes of the Windows Go checkout, which is also what the installed production copy serves, and `.gitattributes` sets `* -text` so Git never converts line endings anywhere in this repo. The consequence is that `sync-assets --check` is meaningful only against a Windows checkout of the Go repo with `core.autocrlf=true`; a Linux checkout would report every text file as drifted. P9 decides whether to normalize across platforms.

## Spikes

`spikes/*` are Phase-0 PASS/FAIL experiments, not product code. They hardcode this machine's paths: the Go checkout for the epod cert, `C:\Program Files\wire-pod\chipper\stttest.pcm`, and the Vosk model under `%APPDATA%\wire-pod\vosk\models`. Their `vendor/` directories are gitignored. Do not grow them; port their findings into the crates instead.

## Live environment

- The Go `chipper.exe`, supervised by the WirePod tray app, is the production server on this machine. It listens on 80, 443, 8080, and 8084 with state in `%APPDATA%\wire-pod`. Anything that binds those ports needs it stopped first. `RUNBOOK-S1.md` has the exact stop, firewall, and restart procedure. Side-by-side testing uses alternate ports (the plan names 18080, 1880, and 1443).
- The robot is ESN 00303f28 at 192.168.8.203 in escape-pod mode. Health probe for the Go server: `curl http://localhost:8080/api/is_running` returns `true`.
- `.env` (gitignored) holds an `OPENAI_API` key for local experiments. The server itself reads its key from the existing `apiConfig.json` in the data dir, not from `.env`.

## Conventions

- Never add Claude attribution to commits: no `Co-Authored-By`, no `Claude-Session`, no "Generated with" footer. This is a locked project rule.
- Vendored things (protos, assets) stay byte-identical to upstream. Deviations are documented, never silent.
