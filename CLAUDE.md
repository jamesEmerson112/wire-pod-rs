# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this repo is

A Rust translation of the Go wire-pod `chipper` server, the voice backend for Anki/DDL Vector robots. The Go server works, so the job is to translate it, not to redesign it. The Go source lives in the sibling checkout `E:/GitHub/wire-pod` under `chipper/pkg/` and `chipper/cmd/`. That checkout is read-only: never edit, build, run, stash or check out anything there. It is also the production server on this machine until cutover.

The goal is a drop-in replacement: the same gRPC, HTTP and mDNS contract and the same state files under `%APPDATA%\wire-pod`, so switching servers is "stop one, start the other".

The plan, the Go-file to Rust-module table and the progress figure are in `docs/translation.md`. That file is the only plan. Everything under `docs/archive/` is the older approach, kept for reference, and none of its rules apply.

## Translation rules

1. One Go file becomes one Rust module: same functions, same order, same control flow, Rust naming.
2. Comments only where the Rust differs from the Go. No line citations, no essays in doc comments.
3. One or two tests per Go file, covering what the robot, the web UI or the state files can observe.
4. A Go crash (nil dereference, index out of range) becomes a returned error or a log line. Nothing else about Go's behaviour is changed or improved.
5. Translation goes to 100% before anything is debugged in depth, optimized, refactored or hardened. Things noticed on the way go on the list at the bottom of `docs/translation.md`.
6. No reviewer agents, no fixer agents, no Go recording programs, no deviations ledger. A translation is checked by reading it against the Go file.
7. Code already on master stays and gets built on. Do not rework it and do not extend its byte-exactness.
8. Where the Go calls a package that is not translated yet, leave a one-line `// TODO(Mn): <Go call>` and carry on. Never `todo!()` or `unimplemented!()`.
9. Subagents are Opus with the model set explicitly, at most two at a time, each briefed only to translate named Go files into named Rust modules.

## Commands

The toolchain is pinned to Rust 1.92.0 in `rust-toolchain.toml`, edition 2024. The gate before every commit is what CI runs on Windows and Ubuntu, and nothing more:

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo build
cargo test
```

Run one test file or one test:

```bash
cargo test -p wirepod-core --test jdocs
cargo test -p wirepod-proto --test roundtrip jdoc_roundtrip
```

Default workspace members are `crates/*` and `xtask`. Spikes build only when named (`-p s1-tls-listener`). `.cargo/config.toml` sets `CMAKE_POLICY_VERSION_MINIMUM=3.5` so the vendored libopus builds under CMake 4.x; keep it.

Asset sync, through the `xtask` alias in `.cargo/config.toml`:

```bash
cargo xtask sync-assets --from ../wire-pod --check   # report drift, exit 1 if any
cargo xtask sync-assets --from ../wire-pod           # copy drifted files, rewrite assets/MANIFEST.sha256
```

The SDK-app trial serves the router on `127.0.0.1:18080` beside the production Go server; `RUNBOOK-SDK-TRIAL.md` is the procedure:

```bash
cargo run -p wirepod-app -- sdk-trial
bash scripts/sdk-trial-diff.sh
```

## Where Go packages go

| Go package under `chipper/pkg/` | Crate |
|---|---|
| `vars`, `logger`, `servers/jdocs/botInfoStorer.go`, the logic of `servers/token` | `wirepod-core` |
| `initwirepod`, `servers/chipper`, `servers/jdocs`, `servers/token` handlers, `vtt`, `mdnshandler`, `wirepod/config-ws`, `wirepod/sdkapp` | `wirepod-server` |
| the outbound robot client (`vector-go-sdk` usage) | `wirepod-vector` |
| `wirepod/speechrequest` | `wirepod-audio` |
| `wirepod/stt/*` | `wirepod-stt` (engines other than Vosk behind Cargo features) |
| `wirepod/localization`, intent matching in `wirepod/ttr` | `wirepod-intent` |
| `wirepod/preqs`, the rest of `wirepod/ttr` | `wirepod-ttr`, with the LLM provider code in `wirepod-llm` |
| `scripting` | `wirepod-plugins` (mlua, Lua 5.1) |
| `wirepod/setup` | `wirepod-setup` |
| `cmd/*/main.go` | `wirepod-app` |

`wirepod-proto` holds the generated tonic and prost code, compiled at build time from protos vendored byte-identically from the Go module cache. Its permitted edits are listed in `crates/wirepod-proto/DEVIATIONS.md`.

Conventions that the existing code depends on:

- Crate direction is `proto <- vector` and `core <- vector <- server`. `wirepod-core` depends on neither tonic nor `wirepod-proto`; it takes plain types.
- axum is 0.7, because tonic 0.12 depends on it and `Routes::into_axum_router()` returns an axum 0.7 `Router`.
- JSON state files are written through `go_marshal` and read through the decoder in `crates/wirepod-core/src/gojson.rs`.
- Shared state is `wirepod_core::AppState`. It replaces Go's package globals; add fields to it rather than creating new globals.
- Test helpers needed by both `src` and integration tests live in `src/test_support.rs` behind the `test-util` feature, which the crate turns on through a dev-dependency on itself.
- `gen` is a reserved word in edition 2024, so Go's `gen` is `generation` here.
- One TLS listener replaces Go's cmux: rustls advertising ALPN `h2` and `http/1.1`, served by hyper-util's auto builder, routing the three gRPC services plus `/ok` and `/ok:80`. The `/ok:80` path has a literal colon and is matched in the router fallback. `spikes/s1-tls-listener` is the reference.

## Assets are a contract

`assets/` is vendored byte-identically from the Go repo: `webroot/`, `intent-data/`, `epod/`, `weather-map.json`, `stttest.pcm`, `pod-bot-install.sh`. `assets/MANIFEST.sha256` is maintained by xtask. Never hand-edit under `assets/`; change the Go repo and re-sync. Text assets are stored with CRLF, as the Windows Go checkout has them, and `.gitattributes` sets `* -text` so Git converts no line endings anywhere in this repo.

## Spikes

`spikes/*` are early experiments with hardcoded paths for this machine. Lift working code out of them (`s1-tls-listener` for the listener and mDNS, `s3-audio` for the filters, `s2-vosk` for Vosk) and do not grow them.

## Live environment

- The Go `chipper.exe`, supervised by the WirePod tray app, is the production server here. It listens on 80, 443, 8080 and 8084 with state in `%APPDATA%\wire-pod`. Health probe: `curl http://localhost:8080/api/is_running` returns `true`.
- The robot is ESN 00303f28 at 192.168.8.203 in escape-pod mode, and it finds the server through the mDNS name `escapepod`.
- Outside a session with the user present: nothing binds 80, 443, 8080 or 8084, nothing registers or browses mDNS, nothing contacts the robot, and nothing reads or writes `%APPDATA%\wire-pod` or `~/.anki_vector`. Tests bind `127.0.0.1:0` and use temporary directories.
- Never write a token, GUID, key or hash value from the live state into any file, commit message or report.
- `.env` is gitignored and holds a key for local experiments. The server reads its key from `apiConfig.json` in the data dir.

## Conventions

- Never add Claude attribution to commits: no `Co-Authored-By`, no `Claude-Session`, no "Generated with" footer.
- Commit messages follow the existing style: `server: translate servers/chipper into the chipper module`.
- Vendored protos and assets stay byte-identical to upstream.
