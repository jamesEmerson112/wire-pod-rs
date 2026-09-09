# Pending upstream work

The Rust port is a port of the fork, `jamesEmerson112/wire-pod`, not of upstream `kercre123/wire-pod`. The fork has diverged, and upstream has moved on since the divergence, so this file records what upstream has that the fork does not. It exists so that a deferred decision stays visible instead of being forgotten when a phase reaches the code the commit touches.

The merge base of the two is `11e7b22` ("optimize some gifs"). The fork's `origin/main` is 35 commits ahead of that base and sits at `81fa3b3`. Upstream's `main` is 8 commits ahead and sits at `347c45f`. Four of those eight are merge commits for pull requests 521, 523, 524, and 525, all of which upstream merged on 2026-08-27. The other four carry the actual content and are listed below. The whole divergence touches five files and amounts to 64 insertions and 23 deletions.

## The four commits

| Commit | Date | Author | Subject | Rust phase affected | Decision |
| --- | --- | --- | --- | --- | --- |
| `fc57af9` | 2026-08-24 | Tomas Kafka | Use MaxCompletionTokens so GPT-5 era models work | P3, LLM provider layer | deferred, not merged |
| `55275b6` | 2026-08-27 | Marvin Hackfort | Use the configured knowledge graph model for the OpenAI provider | P3, LLM provider layer | deferred, not merged |
| `bf85055` | 2026-08-27 | Marvin Hackfort | de-DE: require exact match for short interjection intents | P2, intent matching and the vendored locale assets | deferred, not merged |
| `97271da` | 2026-08-27 | Marvin Hackfort | whisper (OpenAI API): keep configured language, send language and prompt | P2 for the language rule, P8 for the request | deferred, not merged |

### `fc57af9`, MaxCompletionTokens for GPT-5 era models

It changes `chipper/pkg/wirepod/ttr/kgsim.go` and `chipper/pkg/wirepod/ttr/kgsim_cmds.go`, swapping `MaxTokens: 2048` for `MaxCompletionTokens: 2048` at both call sites while leaving `Temperature: 1` and `TopP: 1` in place. The fork solved the same problem earlier and more thoroughly in `846dc3c`, which branches on the model family through `isReasoningModel` and `setAIReqParams`, clears both parameter groups before setting either, and adds a configurable reasoning effort. Upstream's blanket change still sends `temperature` and `top_p`, which reasoning models reject, so the fork's version is a strict improvement. The Rust port should implement the fork's semantics and the plan already specifies them, so this commit adds nothing.

### `55275b6`, honour the configured model for the OpenAI provider

It adds three lines to `chipper/pkg/wirepod/ttr/kgsim.go` so that the model field in the web UI is no longer ignored, falling back to `openai.GPT4oMini`. The fork already does this, again from `846dc3c`, at both call sites. The only semantic difference is the fallback constant: the fork uses `gpt-5.6-luna` and upstream uses `gpt-4o-mini`. The plan's P3a already names `gpt-5.6-luna`, so the Rust port keeps the fork's default.

### `bf85055`, exact match for short German interjections

It adds `"requiresexact": true` to seven intents in `chipper/intent-data/de-DE.json`, covering the affirmative, negative, praise, abuse, greeting, and two blackjack intents whose keyphrases are short enough to substring-match inside longer German words. This is a data-only change and needs no code on either side: the `requiresexact` field already exists in the `JsonIntent` structs in both `chipper/pkg/vars/vars.go` and `chipper/pkg/wirepod/preqs/server.go`, and `en-US.json` already uses it. It is the only upstream change that touches a vendored asset. If the fork ever takes it, `assets/intent-data/de-DE.json` has to be re-synced and P2 gains a German test case; the Rust intent matcher already has to honour `requiresexact` for `en-US`, so no new capability is needed.

### `97271da`, whisper language and vocabulary prompt

This is the only upstream commit carrying behavior the fork genuinely lacks. It is two independent fixes. The first, in `chipper/pkg/wirepod/preqs/server.go`, adds `whisper` to the exemption list on the guard that force-resets the speech-to-text language to `en-US`; without it, running with `STT_SERVICE=whisper` silently reset the language on every start, which also disabled the OpenAI voice path and left German text being read aloud by the English on-board voice. The second, in `chipper/pkg/wirepod/stt/whisper/Whisper.go`, sends two extra multipart fields on the transcription request: `language`, taken from the part of the configured language before the hyphen, and `prompt`, built by a new `buildVocabPrompt()` helper. That helper walks the intent list, takes the first keyphrase of each intent whose trimmed length is at least four characters, joins them with a period and a space, and returns as soon as the buffer would exceed 850 characters, so the prompt is a prefix of the intent list in its stored order rather than a sample of it. A byte-parity port would have to reproduce the 850-character cap, the length filter, the one-keyphrase-per-intent rule, and the separator exactly. The language rule belongs to P2, because it sits in the entry chain that phase ports, and the request construction belongs to P8. If the fork never takes this commit, the Rust port matches the fork by sending neither field and the difference is recorded as a deviation rather than a bug.

## What a merge would do

A read-only trial merge of `upstream/main` into `origin/main` conflicts in exactly two files, both in the language-model layer. `chipper/pkg/wirepod/ttr/kgsim.go` conflicts in two hunks, both inside `CreateAIReq`, and `chipper/pkg/wirepod/ttr/kgsim_cmds.go` conflicts in one hunk inside `DoGetImage`. All three resolve to keeping the fork, because `846dc3c` already subsumes both `fc57af9` and `55275b6`. The other three files merge cleanly: `de-DE.json` and `preqs/server.go` are untouched on the fork side since the merge base, and the fork's only change to `Whisper.go` is in a different function from upstream's. Upstream introduces no new `apiConfig.json` fields at all; the only schema divergence runs the other way, since `knowledge.reasoning_effort` and the whole `battery.gohome_percent` object are fork-only. That means the Rust configuration type should model the fork's schema and nothing more.

## Local branches and stash in the Go repo

The branch `feature/vector-brain-dashboard` at `2a5713a` was pull request #1 and was closed unmerged on 2026-09-07, and its remote branch has been deleted, so the local ref is the only thing keeping the commit reachable. It holds the only copy anywhere of `chipper/webroot/js/vectorbrain.js`, a 397-line dashboard that rendered into `#botStats` on the home page. That is a different program at a different path from the 1240-line `chipper/webroot/sdkapp/js/vectorbrain.js` that `origin/main` ships on the settings page; compared as a rename from the settings file the two differ by 374 insertions and 1217 deletions, which is a rewrite rather than an evolution. Merging the branch now would be destructive, because relative to `origin/main` it would delete the 496-line `sdkapp_test.go` and the settings-page dashboard. The branch must not be deleted or merged without the owner's decision on whether the home-page placement is still wanted.

The GitHub Desktop stash `719727d`, listed as `stash@{0}` on `feature/vector-brain-settings`, contains nothing that is not already committed or gitignored. Its `.gitignore` and `CLAUDE.md` hunks are already present in the fork's history, and its two added files, `Vector Brain.html` and `docs/jetson-second-brain-investigation.md`, are gitignored scratch files that also exist on disk with the same content. It is safe to leave in place and safe to drop, and it is not a source of pending work for the Rust port.

Local `main` was moved to `origin/main` at `81fa3b3` on 2026-09-08. It had been sitting at `685f853`, the July commit the Rust port's assets were originally vendored from, and that commit is a strict ancestor of `origin/main`, so the move was a fast-forward with nothing to lose.

The full set of local branches in the Go checkout, as `git -C ../wire-pod branch --list` reports it, is `feature/battery-gohome` at `846dc3c`, `feature/log-redesign` at `7957653`, `feature/vector-brain-dashboard` at `2a5713a`, `feature/vector-brain-ping` at `dd3783f`, which is the branch currently checked out, `feature/vector-brain-settings` at `083e6b0`, and `main` at `81fa3b3`.
