# wire-pod-rs

A Rust port of the [wire-pod](https://github.com/kercre123/wire-pod) server for
Anki/DDL Vector robots. Goal: a drop-in, protocol-compatible replacement for the
Go `chipper` server — same gRPC/HTTP/mDNS contract, same on-disk state formats,
so an existing installation can switch between the Go and Rust servers by
stopping one and starting the other. Runtime assets under `assets/` are vendored
byte-identically from the Go project (`cargo xtask sync-assets` re-syncs them).
Licensing follows upstream wire-pod (MIT).
