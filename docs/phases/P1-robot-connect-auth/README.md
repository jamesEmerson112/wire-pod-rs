# P1: robot connects and authenticates

This folder is the Phase 1 spec folder. Today it holds the two Go probe
programs whose recorded stdout the Rust tests read through `include_str!`, and
`fixtures/`, which holds two state files the same tests read the same way.
Commit C23 adds the rest of the phase spec beside them (`routes.md`,
`state.md`, `startup-and-restart.md`) and extends this file.

`.gitignore` carries `docs/*` plus `!docs/phases/`, so everything here is
tracked normally with `git add`. Nothing under `docs/` outside `docs/phases/`
is.

## The fixtures

`fixtures/apiConfig.json` and `fixtures/jdocs.json` are this machine's own live
state files, copied byte for byte out of `%APPDATA%\wire-pod` and then redacted
in place. Each is `include_str!`'d by a test that asserts the parse and the
rewrite reproduce every byte, so the byte count is part of what they pin:
`apiConfig.json` is 743 bytes and `jdocs.json` is 5336, both without a trailing
newline.

Redaction replaced whole leaf values, named by key, with ASCII placeholders of
exactly the length the original had:

| File | Keys replaced |
|---|---|
| `apiConfig.json` | `knowledge.key`, `knowledge.openai_prompt` |
| `jdocs.json` | every `hash` inside the `vic.AppTokens` document's `json_doc`, and the `vic.RobotSettings` `default_location` |

Every other byte is the live one, the serials, versions and timestamps
included, which is the point: the fixture has the real file's shape, key order
and length and none of its secrets. The scripts that did the substitutions
worked on the raw bytes, printed neither original, and asserted that the output
was the same length as the input and still re-marshalled to itself through Go's
own structs. They are deliberately not committed, because a committed
redactor invites regenerating a fixture by running it over a fresh copy, and
the thing that has to be checked at that point is the output, not the script.

A regenerated fixture therefore keeps every byte outside the table above and
replaces every value in it with a same-length placeholder. Never hand-edit
one, for the same reason an `expected.txt` is never hand-edited: the file is a
recording, and an edit that makes a test pass has changed the recording rather
than the port.

## The probes

A probe is a small Go program that prints what Go does, so a parity rule is
recorded from the Go toolchain and the exact library versions the Go server
pins rather than guessed at inside a Rust test. Each probe directory holds
`main.go`, its own `go.mod` and `go.sum` so it joins neither this Rust
workspace nor the Go server's module, and `expected.txt`, which is that
program's stdout.

```
cargo run -p xtask -- go-probe           # rewrite every expected.txt
cargo run -p xtask -- go-probe --check   # verify them, naming the first differing line
```

`--check` exits 1 on drift and prints the recorded and produced lines side by
side. With no `go` on the PATH both forms print a SKIP line and exit 0, because
the recordings are committed artifacts and only regenerating or auditing them
needs a Go toolchain.

Never hand-edit an `expected.txt`. Change `main.go` and regenerate.

## The line format, and how to read one

Both recordings use the same format. Lines whose first character is `#` are
comments; there are no blank lines. Every other line is one case, three
tab-separated fields:

```
<section>\t<input>\t<output>
```

Field 1 is the section name. Field 2 describes the input as space-separated
`key=value` pairs whose first pair is always `kind=`, naming what the line
records. Keys match `[a-z_]+`, and no value contains a space or a tab, so
splitting field 2 on `' '` and then each piece on its first `'='` recovers the
inputs. A value may be empty, as in `comp=`.

Field 3 is **always** a Go `%q` quoted string literal, even when the value is a
short number with nothing to escape, so one unquoting rule covers every line.
The only escapes that occur in practice are `\"`, `\\`, `\n` and `\r`, all of
which Rust spells the same way, and every byte in both files is ASCII.

Three conventions are worth knowing before writing a test against these files:

- `"ok"` is this recording's spelling of a nil error. An empty output field
  means Go really produced an empty string.
- Where a case carries both `v=0x...` and `expr=...`, the hex bit pattern is
  the authoritative input and the expression is documentation. Parse the bits.
- The pair (field 1, field 2) is unique across each file, so it is safe to key
  a map on it. Sections appear in the order the section table below lists them,
  and cases in the order the program writes them; both are stable.

## `go-probe`

Paths in this table are relative to the Go checkout's `chipper/` directory
except where they name Go's own source tree or a module in the module cache.

| Section | Go source | What it pins |
|---|---|---|
| `hash` | `pkg/servers/token/hashing.go:16-26`, `:47-71`, `:89-114`, `:116-128`, `:130-136` | GUID and hash encoding, the sizes, the five error strings, the split of a 48-byte hash into 32 + 16, and what `CompareHashAndToken` answers for a matching, mismatching, short, long and undecodable token |
| `rfc3339` | `pkg/servers/token/token.go:29` | `time.RFC3339Nano` at every fraction length, every offset shape, and five edge instants |
| `addmonth` | `pkg/servers/token/token.go:196` | `AddDate(0, 1, 0)` month-end and leap-year normalisation, in fixed zones only |
| `addmonth_local` | `pkg/servers/token/token.go:195-196` | The same call in `America/Los_Angeles`, which is the zone shape `time.Now()` really hands it: the result carries the offset in effect at the target instant, and a target wall time that does not exist normalises backwards by the gap |
| `f32json` | `pkg/vars/config.go:38-39`, Go's `encoding/json/encode.go:538-577` | How a `float32` struct field marshals, including both format cutoffs, the two-digit negative exponent cleanup, and one exact decimal tie broken to even |
| `claims` | `pkg/servers/token/token.go:254-265`, `github.com/golang-jwt/jwt` v3.2.2 | The seven-key JWT payload, its key order, the header, both base64url segments and the two hard-coded literals |
| `legacystamp` | `pkg/logger/logger.go:20-31`, `:102-111`, `:113-122` | The `2006.01.02 15:04:05` stamp and both whole log-line layouts with the component and bot fields present and absent |

`addmonth` and `addmonth_local` are a pair and neither is sufficient alone. Every
`addmonth` case runs in a `time.FixedZone`, which has one offset for all time,
so it cannot show either rule that `addmonth_local` exists for. An
implementation that carries the input's offset straight through to the result
passes every `addmonth` case and writes a wrong `expires` claim for about two
months a year.

## `ini-probe`

| Section | Go source | What it pins |
|---|---|---|
| `setting` | `gopkg.in/ini.v1` v1.67.3, `pkg/servers/jdocs/botInfoStorer.go` | The library knobs that shape the output, the delimiter that reaches the file, and all four key orders the two writers use |
| `file` | `botInfoStorer.go:31-62` and `:66-128` | The complete bytes of one written file per case: both writers, both their paths, per-section alignment padding, the CRLF line break, the blank line between sections, unknown sections and keys surviving a rewrite, and each of the three value-quoting arms firing |

The two writers are not interchangeable. `WriteToIniPrimary` is reached from
`jdocs/server.go:124` and its update path sets cert, name, ip, guid.
`WriteToIniSecondary` is reached from `jdocs/server.go:140` and its update path
sets only guid and then ip. A writer that reuses the primary's order for the
secondary path appends two keys in the wrong order.

## Counts

As of the commit that last regenerated these files:

| File | Total lines | Comment lines | Cases |
|---|---|---|---|
| `go-probe/expected.txt` | 304 | 7 | 297 |
| `ini-probe/expected.txt` | 38 | 7 | 31 |

Do not assert a count from this table without rechecking it; adding a case is
the normal way these files change. Recompute with:

```bash
for f in docs/phases/P1-robot-connect-auth/*/expected.txt; do
  echo "$f total=$(wc -l < "$f") comments=$(grep -c '^#' "$f")"
  grep -v '^#' "$f" | cut -f1 | sort | uniq -c
done
```
