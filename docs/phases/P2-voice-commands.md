# P2: Voice commands

## Goal

Make the robot's voice commands work end to end through the Rust server: take the audio the robot streams over gRPC, decode and filter it, decide when speech ended, transcribe it, match it to an intent, and answer. Together with P0 and P1 this completes what the master plan calls the make-or-break sequence. The phase is sized L.

## Scope

- `SpeechRequest`: sniff the first byte, where `0x4F` means Ogg-Opus and anything else means raw 16 kHz signed 16-bit little-endian PCM, then decode.
- The filter chain: a 300 Hz single-pole high-pass followed by gain 5 then 1.5, applied per chunk with the filter state reset on every chunk. The reset is a Go quirk that is audible, so it is preserved deliberately rather than fixed.
- Voice activity detection: WebRTC VAD mode 2 over 320-byte, 10 ms frames, ending speech when the inactive frame count reaches 23 and the active frame count is above 18.
- The Vosk speech-to-text engine behind the default `stt-vosk` feature, reached through the `SttEngine` trait.
- Intent matching over the fourteen locale files, plus custom intents, `words2num`, and parameter extraction.
- The chipper gRPC streaming handlers that carry all of this.

## Exit criteria

- A golden diff of the post-filter PCM and of the VAD decisions against the Go server. The goldens are harvested from a throwaway `parity-capture` branch of the Go repository that dumps under `WIREPOD_DUMP_DIR` and is never merged.
- Table tests covering the keyphrases of all fourteen locales.
- Live commands work on the real robot, for example setting a timer for ten seconds.

## Dependencies

P0 for the audio and Vosk library spikes, and P1 for the server, configuration, and logging the streaming handlers run inside.

## Status

Not started. One deferred upstream commit lands in this phase: `bf85055` adds `requiresexact` to seven de-DE intents, which would require a re-sync of `assets/intent-data/de-DE.json` and a de-DE test case. A second, `97271da`, changes the rule that resets the speech-to-text language to `en-US`, which lives in this phase's entry chain. Both are recorded in [pending-upstream.md](pending-upstream.md) as deferred and not merged.
