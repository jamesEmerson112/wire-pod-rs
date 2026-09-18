# P1: robot connects and authenticates

This folder is the Phase 1 spec folder. Today it holds the three Go probe
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
program's stdout. A probe that needs only the standard library, which
`store-probe` is, resolves offline and carries an empty `go.sum`; the file is
still there because every probe directory has the same four.

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
| `f32json` | `pkg/vars/config.go:38-39`, Go's `encoding/json/encode.go:538-577` | How a `float32` struct field marshals, including both format cutoffs, the two-digit negative exponent cleanup, and the pair of near-identical inputs that separate an exact decimal tie, which strconv breaks to even, from a value that merely starts its tail with a 5 |
| `claims` | `pkg/servers/token/token.go:254-265`, `github.com/golang-jwt/jwt` v3.2.2 | The seven-key JWT payload, its key order, the header, both base64url segments and the two hard-coded literals |
| `legacystamp` | `pkg/logger/logger.go:20-31`, `:102-111`, `:113-122` | The `2006.01.02 15:04:05` stamp and both whole log-line layouts with the component and bot fields present and absent |
| `claims_matrix` | `pkg/servers/token/token.go:254-265`, `github.com/golang-jwt/jwt` v3.2.2 | The same claim build over fourteen instants, five requestor ids and three token ids, in `America/Los_Angeles` and in UTC: the whole payload, the `expires` string with its instant and offset, and the signing input |
| `jws` | `pkg/servers/token/token.go:266-267` | The shape around a signature and no byte of one: the segment count, the signature's byte and character lengths, that `SignedString` leaves the signing string untouched, that no standard-alphabet byte reaches a token, both header values, and that only a fresh key makes two signings differ |
| `robot_parse` | `vector-cloud/internal/token/identity/identity.go:158`, `.../token.go:96-161`, `github.com/golang-jwt/jwt` `parser.go:96-149` | The robot's own reader over thirty-one crafted tokens: the six required claims and the optional one, the `time.RFC3339` shapes it takes and refuses, the segment count, the alg lookup, and that the signature segment is never decoded |
| `uuid` | `github.com/google/uuid` v1.6.0 `version4.go:13`, `:47`, used at `pkg/servers/token/token.go:180-183` | `NewRandomFromReader` over five fixed draws: the version nibble, the two variant bits, the lowercase hex and the 8-4-4-4-12 layout |

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

## `store-probe`

This one records behaviour rather than formatting: what Go's four transient
token stores hold after each walk the token server and the jdocs server run
over them. Its `main.go` copies the four package-level slices, the three
removal helpers and the three walks verbatim, with `logger.Debug` captured into
a slice and each panicking case wrapped in a `recover`, so a case can be read
back at the instant Go's process would have died.

| Section | Go source | What it pins |
|---|---|---|
| `split` | `jdocs/server.go:70`, `:82`, `:112` | `strings.Split(addr, ":")[0]` on a host and port, a bare host, the empty string, a leading colon, and the three IPv6 addresses whose first colon is inside the address |
| `remove` | `token.go:130-146` | The three removal helpers by index: the survivors and their order, both columns of the log line, and the three out-of-range calls that take Go's process down |
| `primary_walk` | `jdocs/server.go:92-109` | `ReadDocs`'s walk over the primary store: the entries the loop body ran on, the survivors, `matched`, `botGUID`, the removal lines and the panic, over the skip, the three shapes that overrun, and the five that do neither |
| `session` | `jdocs/server.go:111-130`, `:81-86` | The session lookup's `EqualFold` over the split stored address, its break at the first match, the certificate it takes, and the presence check twelve lines earlier that compares the same two values with `==` |
| `secondary` | `token.go:207-217` | `CreateJWT`'s scan: `==` on the serial, the break, and the entry removed out of a four-entry store whose match is at index 1 |

The store encoding is the probe's own: entries joined with `;`, slots joined
with `/`, in the slot order the Go declaration gives. An empty store is the
empty string. That encoding is what makes the survivors' *order* an assertion
rather than a set comparison, which is the whole point of the `remove` section:
Go's `append(s[:i], s[i+1:]...)` keeps the order of every later element, and a
removal that swapped the last element into the hole would leave the same
entries and fail here.

`crates/wirepod-core/tests/token_stores.rs` replays every case through five
drivers and pins the count of each section, so a case the probe gains and
nothing replays fails a test.

## Counts

As of the commit that last regenerated these files:

| File | Total lines | Comment lines | Cases |
|---|---|---|---|
| `go-probe/expected.txt` | 445 | 7 | 438 |
| `ini-probe/expected.txt` | 38 | 7 | 31 |
| `store-probe/expected.txt` | 159 | 7 | 152 |

`store-probe` writes several lines per case, one per observable, so its 152
lines are 32 cases: 7 in `split`, 8 in `remove`, 9 in `primary_walk`, 6 in
`session` and 2 in `secondary`.

Do not assert a count from this table without rechecking it; adding a case is
the normal way these files change. Recompute with:

```bash
for f in docs/phases/P1-robot-connect-auth/*/expected.txt; do
  echo "$f total=$(wc -l < "$f") comments=$(grep -c '^#' "$f")"
  grep -v '^#' "$f" | cut -f1 | sort | uniq -c
done
```
