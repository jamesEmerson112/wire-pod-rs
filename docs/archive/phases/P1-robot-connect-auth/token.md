# P1: the token service

This is the specification commit C17 implements the token service from. It describes what the Go
server does, what the robot does with the answer, what commit C11 already landed in
`wirepod-core`, and what is left for C17 to compose and to decide.

Citations are `path:line`. A bare `token.go` or `hashing.go` means
`chipper/pkg/servers/token/` in the Go checkout at `E:/GitHub/wire-pod`, and `jdocs/server.go`
means `chipper/pkg/servers/jdocs/server.go`. Paths beginning `vector-cloud/` are the robot's own
cloud process in the same checkout. Paths beginning `crates/` are this repository. Every line
number here was read from the file at the time of writing rather than copied from a commit
message.

## What the service is

`tokenpb.Token` is one of the three gRPC services the robot dials on port 443, registered beside
the chipper and jdocs services at `initwirepod/startserver.go:78`. It hands the robot two things
in one `TokenBundle` (`crates/wirepod-proto/proto/token/token.proto:86-91`): a JWT in `token`,
which the robot stores and presents as bearer metadata on every later cloud call, and a client
token in `client_token`, which is the GUID the SDK and the wire-pod dashboard authenticate with.
The `sts_token` field is never set.

The whole service is one file of 301 lines. `TokenServer` embeds `tokenpb.UnimplementedTokenServer`
(`token.go:24-26`), so every RPC the file does not define answers `Unimplemented`.

| RPC | Go | `CreateJWT` arguments | What the bundle carries | Side effects |
|---|---|---|---|---|
| `AssociatePrimaryUser` | `token.go:272-282` | `(ctx, false, true)` at `:280` | `token` always; `client_token` from a fresh GUID, or from the secondary store on a match | Appends the request's certificate bytes and the peer address paired with the certificate's issuer common name to the two session slices (`:276-278`). Because `isPrimary` is true it always takes the else arm at `:231-239`, so it writes no file and leaves one entry in `TokenHashStore` |
| `AssociateSecondaryClient` | `token.go:284-289` | `(ctx, false, false)` at `:287` | `token` always; `client_token` from a fresh GUID, from the secondary store on a match, or the global constant when no GUID was minted | Three file writes when the peer matches a stored robot (`:226-228`), one in-memory append when it does not (`:236`) |
| `RefreshToken` | `token.go:291-296` | `(ctx, false, false)` at `:294` | Same as the row above | Same as the row above |
| `ReassociatePrimaryUser` | not defined | none | none | Answers `Unimplemented` |
| `DisassociatePrimaryUser` | not defined | none | none | Answers `Unimplemented` |
| `ListRevokedTokens` | not defined | none | none | Answers `Unimplemented` |
| `RevokeFactoryCertificate` | not defined | none | none | Answers `Unimplemented` |
| `RevokeTokens` | not defined | none | none | Answers `Unimplemented` |

Two things are worth naming before the detail. `AssociatePrimaryUser` dereferences `pem.Decode`'s
result without checking it (`token.go:274-275`), and the Go server installs no recovery
interceptor, so a request whose `session_certificate` is empty or is not PEM takes the whole
process down. And no request field is read anywhere except that certificate: the
`expiration_minutes` field all three request messages carry
(`proto/token/token.proto:109`, `:117`, `:149`) is never looked at, the package's own
`ExpirationTime` constant (`token.go:30`) is never read either, and `RefreshTokenRequest`'s
`refresh_jwt_tokens` and `refresh_sts_tokens` flags make no difference to the answer.

## `CreateJWT`, step by step

`CreateJWT(ctx, skipGuid, isPrimary)` is `token.go:185-270`. It is the whole of the service's
behaviour; the three handlers are one log line each and a call into it.

**The defaults.** `requestorId` starts at the fixed serial at `token.go:187`, `clientToken` starts
at the package's `GlobalGUID` constant (`token.go:188`, declared at `token.go:32`), and the bundle
starts empty (`:189`).

**Two clock reads, not one.** `currentTime` comes from `time.Now()` at `token.go:195` and
`expiresAt` from a second `time.Now()` at `token.go:196`, on which `AddDate(0, 1, 0)` is called.
Both are formatted with `TimeFormat`, which is `time.RFC3339Nano` in local time (`token.go:29`).
The two calls are a few hundred nanoseconds apart, so the `iat` and `expires` claims carry
different sub-second fractions, and because they are a calendar month apart they can carry
different UTC offsets across a daylight saving transition. `AddDate` normalises an overflowing day
of the month forward rather than clamping it, so 31 January becomes 3 March in a common year and 2
March in a leap one. Both values are logged (`:197-198`).

**The peer, and the IPv6 quirk.** `peer.FromContext` gives the peer address and `token.go:202`
takes `strings.Split(addr, ":")[0]` and trims it. That is the host for an IPv4 address with a port.
For an IPv6 peer the first colon is inside the address, so the host comes out as the single
character `[`, and every IPv6 caller is keyed under that one string. `p` is used without a nil
check, which is one of the panics reserved deviation 31 covers.

**The bot-info lookup, on every call.** `GetEsnFromTarget` (`token.go:58-74`) reads
`vars.BotInfoPath` from disk on every single call, unmarshals it, and walks the robots comparing
`strings.TrimSpace(target)` against `strings.TrimSpace(robot.IPAddress)` for exact string equality.
It does not use the in-memory `vars.BotInfo` the rest of the package mutates, it does not fold
case, and it does not normalise the address. A miss returns the error `bot not found`.

**The secondary scan.** If the lookup hit, `token.go:206-217` walks `SecondaryTokenStore` looking
for an entry whose first slot equals the ESN. On a match it sets `skipGuid` to true, remembers the
entry's GUID and hash, removes the entry (`:213`) and breaks. That assignment at `:209` is the only
way `skipGuid` can be true, because all three handlers pass it as false.

**The two arms.** `token.go:221-230` is taken when the lookup hit and `isPrimary` is false. It logs
that the target matched, sets `requestorId` to `vic:` plus the serial exactly as the bot-info file
spells it (`:223`), and then, unless `skipGuid` was set by the secondary scan, mints a GUID and its
hash with `CreateTokenAndHashedToken` (`hashing.go:47-71`) and performs three writes:
`WriteTokenHash` rewrites the jdocs file (`:226`), `SetBotGUID` rewrites the bot-info file
(`:227`, `token.go:76-97`), and `ChangeGUIDInIni` rewrites `sdk_config.ini` (`:228`,
`token.go:148-178`). The GUID becomes the bundle's client token.

`token.go:231-239` is the other arm, taken when the lookup missed or when the call came from
`AssociatePrimaryUser`. It logs that the ESN was not found, and unless `skipGuid` is set it mints a
GUID and hash and appends one entry `{peer host, GUID, hash}` to the in-memory `TokenHashStore`
(`:236`). It writes no file. The jdocs server's fallback is what later turns such an entry into the
same three writes, when the robot's first `ReadDocs` for `vic.AppTokens` arrives from the same
address (`jdocs/server.go:94-109`).

**The client token.** `token.go:240-242` copies `clientToken` into the bundle unless `skipGuid` is
set, so an ordinary call always carries one, either the fresh GUID or the global constant when no
GUID was minted. A secondary match instead takes `token.go:244-248`, which calls `SetBotGUID` with
the stored pair, puts the stored GUID into the bundle, and logs it.

**The claims and the key.** `GenerateUUID` (`token.go:180-183`) draws a version 4 UUID for
`token_id` at `:250`. `jwt.NewWithClaims(jwt.SigningMethodRS512, ...)` at `token.go:254-265` builds
the seven-claim map. `rsa.GenerateKey(rand.Reader, 1024)` at `:266` makes a key that lives only for
the length of the call, `SignedString` signs with it at `:267`, and both errors are discarded into
`_`. The signed string becomes `bundle.Token` and the bundle is returned (`:268-269`).

**The stored hash.** `WriteTokenHash` (`token.go:99-128`) looks the `vic.AppTokens` document up
under the **bare** serial (`:101`) and stores it under `vic:` plus the serial (`:125`). Nothing
anywhere writes a jdoc under a bare serial, so the lookup never hits, `token.go:103-107` always
runs, and the decode at `:108` only ever sees the empty string, whose error is discarded. A third
`time.Now()` at `:110` stamps the client token's `issued_at`. The marshal error at `:115-119` is
logged and then the nil result is stored anyway, which would leave an empty `json_doc`. Four fields
are copied one at a time into a fresh `AJdoc` (`:120-124`), so anything else the looked-up document
carried is dropped, and then `AddJdoc` and `WriteJdocs` both write the file (`:125-126`).

## What the robot does with each claim

The robot parses with `new(jwt.Parser).ParseUnverified`
(`vector-cloud/internal/token/identity/identity.go:158`) and then reads the claims with
`FromJwtToken` (`vector-cloud/internal/token/identity/token.go:96-161`). `ParseUnverified`
(`golang-jwt/jwt@v3.2.2/parser.go:96-148`) splits on `.`, refuses anything that is not exactly
three parts (`:97-99`), decodes and reads parts zero and one, looks the signing method up by the
header's `alg` and refuses an unknown or absent one (`:140-142`, `:144`), and never refers to part
two. The claim names are constants at `identity/token.go:11-19`.

| Claim | Required | Type the robot demands | Read where | What it is used for |
|---|---|---|---|---|
| `token_id` | yes | JSON string | assertion at `identity/token.go:101`, refusal two lines below | Stored on `TokenInfo` and never read again |
| `token_type` | yes | JSON string | `:106` | Stored and never read again |
| `user_id` | yes | JSON string | `:111` | Answered to the engine as the account id (`vector-cloud/internal/jdocs/client.go:123-129`) and used by the log collector. An empty value costs the robot its token at the next boot |
| `requestor_id` | yes | JSON string | `:116` | Stored and never read again |
| `iat` | yes | JSON string, then `time.ParseInLocation(time.RFC3339, s, UTC)` at `:126` | `:121` | The refresher stalls while the robot's clock is behind it |
| `expires` | yes | JSON string, then the same parse at `:135` | `:131` | Minus three hours is the refresh time |
| `permissions` | no | object, or the claim is ignored | `:153-156` | Stored when it is an object. A `null`, an array or a number is silently dropped |

A missing or wrongly typed required claim answers `missing claim <name>` (`:167-169`). A timestamp
the RFC 3339 parse refuses is a fatal error for the whole token, because the errors at `:128` and
`:137` are returned rather than defaulted. `JwtToken` (`:81-91`), which would write the claims back
out, has no caller in the robot tree, and `IsExpired` (`:75-77`) has none either: expiry is never
checked, only refreshed against.

A token that parses is written raw to `token.jwt` in the robot's JWT directory, through a temporary
file and a rename (`identity/identity.go:169-179`). At boot the robot reads that file back and
parses it again; a parse failure deletes the file (`:133-137`), and a successful parse whose
`user_id` is empty also deletes it (`:141-145`). So an empty `user_id` costs the robot its
association at the next restart even though the parse itself answers `ok`.

## Refresh timing, and the `iat` stall

`RefreshTime` is `expires` minus three hours (`identity/identity.go:191-193`). The refresh routine
(`vector-cloud/internal/token/refresher.go:18-73`) loops as follows.

It first waits for a token to exist at all, sleeping five minutes between attempts (`:20`, `:25-34`).
Then, while the robot's own clock reads earlier than the token's `iat`, it sleeps in twenty second
steps (`:21`, `:38-42`). That loop is the reason `iat` matters: a robot whose clock has not yet been
set by NTP parks there until it has, and a server that issued an `iat` in the robot's future would
park it for that long. Finally it computes `RefreshTime().Sub(time.Now()) + 10*time.Second`
(`:46`); if that is at or below zero it refreshes immediately, otherwise it sleeps exactly that
long and loops (`:47-65`). So a refresh fires about three hours minus ten seconds before `expires`,
and after a refresh it sleeps five minutes before reconsidering.

With Go's one calendar month expiry, the robot therefore comes back roughly once a month, three
hours before the token it holds runs out. A token whose `expires` is less than three hours away
when it arrives is refreshed at once, which would turn into a five minute polling loop rather than
a tight one, because the refresh branch sleeps five minutes before looping.

## The robot's request and its metadata

The refresh goes through `handleJwtRequest` (`vector-cloud/internal/token/queue.go:82-107`). It
refreshes when `time.Now()` is after `RefreshTime()` or when the caller forced it (`:91`), opens a
connection carrying the old token as metadata, and calls `refreshJwtToken`
(`:97`, `vector-cloud/internal/token/client.go:81-83`), which sends
`pb.RefreshTokenRequest{RefreshJwtTokens: true}`. `RefreshStsTokens` is left false and
`ExpirationMinutes` is left zero. Only `bundle.Token` is used from the answer (`:101`); the
`client_token` the Go server put in the bundle is discarded on a refresh.

The metadata is built by `tokenMetadata` (`vector-cloud/internal/token/accessor.go:62-65`): the
standard appkey metadata plus the key `anki-access-token` holding the raw old token. The Go server
reads neither, because `RefreshToken` (`token.go:291-296`) never looks at the request or at the
metadata: the only thing that identifies the caller is the peer address.

## Why the live comparison from 127.0.0.1 is safe

`crates/wirepod-vector/tests/live_token.rs` calls `RefreshToken` once against the production Go
server on this machine, and the whole of its safety argument is the bot-info lookup.
`GetEsnFromTarget` (`token.go:58-74`) compares the peer host against each stored `ip_address` for
exact string equality, and `botSdkInfo.json` stores the robot under its LAN address, so a call from
`127.0.0.1` misses. A miss takes `token.go:231-239`, the arm that writes no file, rather than
`token.go:221-230`, the arm that rewrites `jdocs.json`, `botSdkInfo.json` and `sdk_config.ini`.

The complete side effect of one run is one entry appended to `TokenHashStore` (`:236`), six
`logger.Debug` lines (`token.go:292`, `:197`, `:198`, `:232`, `:234`, `:251`) and one read of the
bot-info file (`:59`). The appended entry lives in process memory, a restart clears it, and its only
reader is the jdocs fallback (`jdocs/server.go:94-109`), which matches on the address a `ReadDocs`
call came from; no robot on this network calls from `127.0.0.1`, so the entry is never matched and
simply sits there. The literal IPv4 address is used rather than `localhost` on purpose, because
`localhost` can resolve to the IPv6 loopback and `token.go:202` would then key the entry under `[`.

The test asserts modification times either side of the call on the three files the writing arm
touches, which is an after-the-fact witness rather than a guard: the reason the write-free arm is
taken is the absent address, and the assertion is what would notice if that reasoning were wrong.
The test must never construct an `AssociatePrimaryUserRequest`, for the nil dereference at
`token.go:274-275`.

To run it, with the Go server up and the robot untouched:

```bash
WIREPOD_LIVE_GO=1 cargo test -p wirepod-vector --test live_token -- --ignored --nocapture
```

Check `curl http://localhost:8080/api/is_running` answers `true` before and after. The five other
tests in that file dial nothing and run in an ordinary `cargo test`.

## What C11 already provides

`crates/wirepod-core/src/token/jwt.rs` holds every byte-producing half of the service, as pure
functions over injected inputs:

- `Requestor`, with `Unknown` for the default serial at `token.go:187` and `Robot(String)` for
  `vic:` plus a serial at `token.go:223`, holding the serial as a raw string rather than an `Esn`
  so that its case is not changed.
- `Claims::new(&Requestor, token_id, &dyn WallClock)`, which reads the clock twice as
  `token.go:195-196` does and resolves each claim's UTC offset at its own instant.
- `generate_token_id`, Go's `uuid.New().String()` (`token.go:180-183`).
- `marshal_claims` and `signing_input`, which run the claim map through the crate's Go JSON encoder
  so that the payload bytes are `encoding/json`'s.
- `issue_token`, which assembles the three segments, with the third holding `SIGNATURE_LEN` drawn
  bytes. That is numbered deviation 28 in `docs/phases/P4-sdk-app/deviations.md`.
- `TokenBundle { token, client_token_guid }`, the two fields Go sets.
- `write_token_hash`, Go's `WriteTokenHash` including the dead existing-document arm, the bare
  serial lookup and the `vic:` store.
- `create_token_and_hashed_token` in `crates/wirepod-core/src/token/hash.rs`, returning a
  `TokenPair { guid, guid_hash }`.
- The three transient stores from C10 in `crates/wirepod-core/src/token/stores.rs`, with
  `add_primary`, `take_secondary_match`, `take_primary_matches`, `find_session_match` and the three
  index removals, all reproducing Go's walk order and its skip quirk.

What is not there is the service itself: nothing reads a peer address, nothing calls
`GetEsnFromTarget`, and nothing implements the generated `tokenpb` service trait. `wirepod-core`
depends on neither `wirepod-proto` nor tonic, so the conversion from `TokenBundle` to the generated
message belongs to the crate that owns the service.

## What C17 must write

One handler each for the three implemented RPCs, over one shared function standing in for
`CreateJWT`, composed from the pieces above in this order:

1. Read the peer address from the tonic request extensions and take the host with the same split
   Go takes, which is `host_of` in `crates/wirepod-core/src/token/stores.rs`. A missing peer must
   answer an error rather than panic.
2. Resolve that host against the bot-info file the way `GetEsnFromTarget` does: exact string
   equality against each stored `ip_address`, on a value read for this call rather than on a cached
   one, since Go re-reads the file every time.
3. Decide the `Requestor` from that lookup: `Requestor::Robot(serial)` when it hit and the call is
   not the primary association, `Requestor::Unknown` otherwise.
4. Scan the secondary store with `take_secondary_match(&esn)` when the lookup hit. A match supplies
   the bundle's client token and suppresses the mint, which is Go's `skipGuid`.
5. Draw the token id with `generate_token_id`, build the claims with `Claims::new`, and issue the
   token with `issue_token`.
6. On the minting path, call `create_token_and_hashed_token` and then, in Go's order,
   `write_token_hash` with the **bare** serial, the bot-info GUID write, and the `sdk_config.ini`
   rewrite. On the non-minting path, append one `PrimaryEntry` to the primary store and write
   nothing.
7. Put the GUID, the global constant or the secondary store's GUID into the bundle's
   `client_token_guid`, following `token.go:240-248`, and convert the bundle to
   `tokenpb.TokenBundle`.

`RandomError` from `generate_token_id`, `random_signature` or `create_token_and_hashed_token` is
fatal to the request and must answer a gRPC status rather than being discarded the way Go discards
`rsa.GenerateKey`'s error and the way `uuid.New` panics. That is already recorded as a candidate
deviation.

C17 also needs `AppState` to carry what these calls take. Today `AppState`
(`crates/wirepod-core/src/state.rs`) holds the bot-info file, the pinger, the registry, the timings
and the clock. The jdocs store, the token stores, the SDK ini path and a `WallClock` all have to
reach the handler, which is the `AppState` growth the crate map lists as remaining.

## Decisions C17 must take

**The `AssociatePrimaryUser` crash.** The port must answer an error where Go dereferences
`pem.Decode`'s nil result (`token.go:274-275`). This is the nil PEM block reserved deviation 31
already names; C17 chooses the status and the log line.

**Whether to bound `TokenHashStore`.** Go appends an entry for every request from an unmatched
address and prunes only through `RemoveFromPrimaryStore` (`token.go:135-138`) from the jdocs
fallback (`jdocs/server.go:107`), so any local process that can reach port 443 can grow the store
without limit, and each entry holds a GUID and its hash. C17 decides whether to bound or expire it
and records the answer either way.

**The unimplemented RPCs.** Five of the eight answer `Unimplemented` through Go's embedded
`UnimplementedTokenServer`. The tonic-generated trait requires every method, so C17 must write five
bodies that answer `Status::unimplemented` with the text tonic writes for an unimplemented method,
and a test should pin that the robot-visible behaviour is the same as grpc-go's.

**`expiration_minutes` stays ignored.** All three request messages carry the field and Go reads
none of them, and neither does `ExpirationTime` (`token.go:30`) reach the claims. Reading it would
change the `expires` claim and therefore the robot's refresh schedule, so it stays unread and the
decision is recorded rather than left implicit.

## The WireOS note

A robot running WireOS rather than stock 1.8 firmware reaches this service through the same code.
The study on branch `docs/wireos-study` of the Go fork compared the two cloud processes file by
file: the JWT parse is a byte-identical file apart from one added logging line, the claim reader is
an identical file, and the refresh timing is identical. So everything in this document holds for a
WireOS robot as written. The one WireOS difference that matters elsewhere in the port is that such
a robot always opens `StreamingIntentGraph`, which is P2's concern and not this service's.

## What pins this today

The `claims`, `claims_matrix`, `jws`, `robot_parse` and `uuid` sections of
`go-probe/expected.txt` record the Go side, and the section table in this folder's `README.md`
says what each one holds. `crates/wirepod-core/tests/jwt.rs`,
`crates/wirepod-core/tests/jwt_matrix.rs`, `crates/wirepod-core/tests/jwt_robot.rs` and
`crates/wirepod-core/tests/jwt_document.rs` drive the port from those recordings, and
`crates/wirepod-vector/tests/live_token.rs` compared one real bundle from the running Go server
against a locally issued one and matched on every point it checks.
