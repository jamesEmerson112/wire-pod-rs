# Deviations from byte-identical vendored protos

The dd-l and vector-go-sdk `.proto` files are vendored byte-identical from the Go
module cache, with the following minimal exceptions. None of these affect the wire
format (imports and source-only options do not appear in encoded messages).

- `proto/vector/shared.proto`: added `import "onboarding.proto";`. The original
  references the `Onboarding` message without importing its file — tolerated by the
  toolchain that generated the Go SDK, rejected by protox's strict resolver.
- `proto/google/api/annotations.proto` and `http.proto`: fetched from canonical
  googleapis (not present in the Go module cache); byte-identity not required —
  they only satisfy source-level option imports.
