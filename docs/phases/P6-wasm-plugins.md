# P6: WASM plugins

## Goal

Replace Go's plugin system, which loaded native `.so` files and therefore never worked on Windows, with a WebAssembly host. The phase is sized M in the master plan. The point is not only parity but a capability the Go server never had on this platform, and Extism's guest development kits mean a plugin author who wrote in Go can recompile rather than rewrite.

## Scope

- The Extism host inside `wirepod-plugins`, loading `*.wasm` from `<data_dir>/plugins/`.
- The guest ABI in `wirepod-plugin-api`, mirroring Go's `Utterances`, `Name`, and `Action`: a `wirepod_manifest()` export, a `wirepod_action(...)` export returning an intent and a speech string, and opt-in host functions for logging, HTTP, and robot access.
- Both sample plugins from the Go repository, `whatdate` and `sdkTest`, ported as guests.
- The `/api/reload_plugins` route.
- A `PLUGINS.md` describing how to write and build a plugin.

## Exit criteria

- Both sample plugins work on Windows and on Linux.
- A plugin that panics is isolated and does not take the server down.

## Dependencies

P0, whose S4 spike proved extism builds on the MSVC toolchain, and P2, because plugins are reached from the intent path.

## Status

Not started.
