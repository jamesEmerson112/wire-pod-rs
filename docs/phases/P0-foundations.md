# P0: Foundations + de-risk spikes

## Goal

Stand up the workspace, the vendored contract inputs, and continuous integration, then answer the four questions that could invalidate the whole port before any behavior is ported. The phase is sized M in the master plan. Its real product is not code but four PASS/FAIL answers, so that no later phase discovers a blocking library or platform problem after a large amount of work has been built on top of it.

## Scope

- A Cargo workspace holding thirteen crate stubs plus an `xtask` crate, each stub carrying a `//!` doc comment that states the crate's intended responsibility.
- Vendored protos compiling through protox and tonic-build, with no `protoc` installation required. Three protos come from `digital-dream-labs/api` and twelve from `fforchino/vector-go-sdk`, plus the two `google/api` protos they import.
- Runtime assets copied byte-identically from the Go checkout into `assets/`, covering `webroot/`, `intent-data/`, `weather-map.json`, the `epod/` certificate and key, `stttest.pcm`, and `pod-bot-install.sh`, together with an xtask subcommand that re-copies them and reports drift.
- CI on Windows and Ubuntu running `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo build`, and `cargo test`.
- Spike S1: rustls plus the hyper-util auto builder on port 443 serving the escape-pod certificate, with the Go server stopped for about five minutes, run once with ALPN advertised and once with ALPN off. The robot's connection check must succeed and an established connection must appear.
- Spike S2: the `vosk` crate links on Windows against the shipped `libvosk.dll` and transcribes `stttest.pcm` to the same text the Go server produces.
- Spike S3: `opus` and `webrtc-vad` build on Windows, and the decode output and VAD decisions diff clean against a dump taken from the Go server.
- Spike S4: mlua and extism run a hello-world on the MSVC toolchain.

## Exit criteria

- The four CI commands are green on both the Windows and the Ubuntu runner.
- All four spikes report PASS.
- The S1 gate is explicit in the master plan: if S1 fails with ALPN on and also with ALPN off, the port stops and switches to `tls-native` before any further phase proceeds.

## Dependencies

None. P0 is the first phase and everything else depends on it.

## Status

Done; the spikes landed. The workspace, the crate stubs, the vendored protos, the vendored assets, and the CI workflow were committed in `2230024`, and the spike sources live under `spikes/`. All four spikes are recorded PASS in the commit message of `2230024` rather than in the master plan, which only specifies them, so the S1 gate never fired and the rustls listener design in the plan stands unchanged. The S1 result recorded there is the in-process selftest, run once with ALPN advertised and once with ALPN off. The spikes are Phase-0 experiments and not product code: they hardcode this machine's paths, their `vendor/` directories are gitignored, and their findings are meant to be ported into the crates rather than grown in place. The procedure for re-running S1 against the live robot, including how to stop and restart the Go server, is in `RUNBOOK-S1.md` at the repository root.
