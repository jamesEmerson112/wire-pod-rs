# P8: Remaining STT engines

## Goal

Add the five speech-to-text engines that P2 did not, so the Rust server covers all six the Go server supports. The phase is sized M in the master plan. Go shipped one binary per engine, chosen at compile time; the Rust server has one binary with every compiled-in engine selectable at runtime, so this phase is also where that design is proven.

## Scope

- The engines in order: whisper.cpp, then OpenAI whisper, then Picovoice Leopard, then Houndify, then Coqui.
- Houndify is the odd one. It needs hand-rolled streaming and returns a ready-made intent rather than text, which is what the `SttEngine` trait's `Intent` outcome exists for; it bypasses local intent matching entirely.
- Coqui is best-effort. It is Linux-only foreign-function-interface work against a discontinued project, and the plan permits dropping it and documenting the drop if it turns out to be unworkable.
- Runtime selection through `STT_SERVICE` and the configuration file, plus a `--list-stt` flag, with each engine behind its own Cargo feature.

## Exit criteria

- Each engine transcribes `stttest.pcm` to the same text the Go server produces.
- Switching engines at runtime works without a rebuild.

## Dependencies

P0 and P2. P2 establishes the `SttEngine` trait, the audio pipeline that feeds it, and the intent path that consumes its output.

## Status

Not started. One deferred upstream commit lands here: `97271da` adds a `language` field and an 850-character vocabulary `prompt` to the OpenAI whisper request. If the fork never takes it, the Rust port matches the fork by sending neither, and that becomes a recorded deviation. See [pending-upstream.md](pending-upstream.md).
