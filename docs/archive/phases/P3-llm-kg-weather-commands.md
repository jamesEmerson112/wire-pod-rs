# P3: LLM/KG/weather/commands

## Goal

Port the conversational half of the server: the language-model provider layer, the streaming conversation loop that drives the robot's speech, the weather lookup, and the command tags a model can emit. This is the largest body of work in the plan, sized XL, and it is where the fork's own fixes are most concentrated, so the phase is organised around not losing them. It runs in four sub-steps, 3a through 3d.

## Scope

- 3a, the provider layer: OpenAI, Together, and custom endpoints; reasoning-model parameters, where `gpt-5` and o-series models take `max_completion_tokens` and a reasoning effort instead of `max_tokens` and sampling parameters; the default model `gpt-5.6-luna`; and `RememberedChats`.
- 3b, the streaming sentence splitter, checked against recorded server-sent-event transcripts.
- 3c, `ConversationTask`, which unifies Go's `StreamingKGSim` and `DoGetImage`. These were near-duplicates in Go and the fork's fixes were applied to only the first. The Rust version is one `select!` loop over an mpsc sentence stream, with a cancellation token for touch and wake-word interrupts from the event stream and a first-token deadline. Behavior control is released on every path by an RAII guard. The dual response path is preserved: the gRPC intent acknowledgement goes back over gRPC while the spoken response is driven separately through the SDK.
- 3d, weather, the language-model command tags, and `DoSayText` and text-to-speech routing across the OpenAI voice path and the robot's native voice.

## Exit criteria

- 3b is a hard internal gate: the sentence splitter must be green against the recorded transcripts before 3c starts.
- Named tests pin the fork behaviors: the end-of-file flush of an unpunctuated tail, the stream-error unhang, the early `intent_greeting_hello` acknowledgement for slow reasoning models, and the custom prompt going last.
- A ten-question live A/B against the Go server.
- A behavior-control leak test: killing the language-model endpoint mid-stream must leave the robot autonomous again within five seconds.
- The early acknowledgement fires with `reasoning_effort` set to medium.
- A golden test over the weather map.

## Dependencies

P0 through P2. The conversation loop is entered from the intent path built in P2, and it needs P1's configuration and logging. The spoken response also needs the outbound robot client, which is the reason the plan places the full SDK client in P4 and this phase before it; the early P4 slice supplies part of that client ahead of schedule.

## Status

Not started. The fork's reasoning-model work already supersedes two upstream commits that would otherwise land here, `fc57af9` and `55275b6`; both are recorded as deferred in [pending-upstream.md](pending-upstream.md) and the Rust port follows the fork, keeping `gpt-5.6-luna` as the fallback model.
