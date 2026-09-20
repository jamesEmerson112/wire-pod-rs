# The Vector robot API surface, as a server author sees it

This document describes the network interface of an Anki/DDL Vector robot running WireOS: the three
gRPC services the robot dials out to and therefore expects a server to implement, the
`ExternalInterface` service the robot itself serves on port 443, and the files and certificates that
decide which server the robot talks to at all. It is written for someone building
`E:/GitHub/wire-pod-rs`, a Rust port of the wire-pod `chipper` server. It does not cover how to build
WireOS, the on-robot behaviour tree, the animation system, the Bluetooth setup path, or the Python
SDK's own API. It assumes gRPC and protobuf are familiar.

It complements `wire-pod-wt-docs/docs/wireos-study.md`, which explains what WireOS is, how its cloud
client differs from stock, and what those differences mean for the server; this document is the
interface reference that study points at, organised by surface rather than by difference, and where
the two overlap the study is the authority on WireOS-versus-stock and this one is the authority on
message shapes and call ordering.

---

## Contents

1. [Orientation: who calls whom](#1-orientation-who-calls-whom)
2. [Inbound: the services a server must implement](#2-inbound-the-services-a-server-must-implement)
3. [Outbound: the SDK surface on the robot](#3-outbound-the-sdk-surface-on-the-robot)
4. [Setup and identity](#4-setup-and-identity)
5. [CLAD, briefly](#5-clad-briefly)
6. [Things that will cost you a day](#6-things-that-will-cost-you-a-day)
7. [What this document does not confirm](#7-what-this-document-does-not-confirm)

---

## 1. Orientation: who calls whom

There are two independent directions of traffic, and conflating them is the most common early
mistake.

| Direction | Who dials | Endpoint | Transport | Auth |
|---|---|---|---|---|
| Robot to server | `vic-cloud` on the robot | whatever `server_config.json` names, normally `escapepod.local:443` | TLS, HTTP/2, gRPC | `anki-access-token` and `anki-app-key` in per-RPC metadata, plus a client certificate on the token connection |
| Robot to server | `vic-cloud` on the robot | the `check` URL, normally `escapepod.local/ok` | plain HTTP and then HTTPS, `HEAD` | none |
| Server or SDK to robot | wire-pod, the web dashboard, the Python SDK | `<robot ip>:443` | TLS, HTTP/2, gRPC and grpc-gateway JSON | `Authorization: Bearer <client GUID>` |

The robot never dials the server's SDK surface, and the server never serves `ExternalInterface`. The
server implements `ChipperGrpc`, `Jdocs` and `Token`; the robot implements `ExternalInterface`.

The three inbound protos the port vendors under
`wire-pod-rs/crates/wirepod-proto/proto/{chipper,jdocs,token}/` are byte-identical to the copies in
`digital-dream-labs/api@v0.0.0-20210824232136-8cc90c1bb12c`, which is the exact module version the
WireOS cloud client compiles against (`wire-os-victor/cloud/go.mod`). Field numbers and service
names on the inbound side therefore need no further checking. The outbound side is not so tidy; see
[3.6](#36-the-vendored-sdk-proto-does-not-match-the-robots).

---

## 2. Inbound: the services a server must implement

All three services are served from one TLS listener on the port named in `server_config.json`.
wire-pod serves them on the configured port (443 by default) and, when escape-pod mode is on, on
8084 as well, because older escape-pod resource images point at `escapepod.local:8084`
(`wire-os-victor/resources/escapepod/config/server_config.json`,
`wire-pod/chipper/pkg/initwirepod/startserver.go`).

### 2.1 The connection check, which comes first

Before the robot opens a voice stream for a connection check it performs three steps in order, all
in `wire-os-victor/cloud/internal/voice/stream/connect.go`.

1. `HEAD http://<check>?emresn=<esn>&ankiversion=<ver>&victorversion=<ver>` with exactly one header,
   `User-Agent: Victor-CCHECK/<ankiversion>`, no body and no authentication. `emresn` is the ESN from
   `/bin/emr-cat e`, `ankiversion` is the property `ro.anki.version` and `victorversion` is
   `ro.anki.victor.version`; any of them can be empty, in which case the parameter is still present.
2. The same request over HTTPS, validated against the Mozilla root pool from `gwatts/rootcerts`.
3. A `ChipperGrpc.StreamingConnectionCheck` stream.

Because the robot prepends the scheme and uses the same `check` value for both, the host must answer
`HEAD` on port 80 and on port 443.

Only transport errors fail steps 1 and 2. The code closes the response body and moves on without
looking at the status code, so a 404 from the `check` URL passes the check exactly as a 200 does.
What must work is DNS, TCP, and, for step 2, a certificate chain the robot accepts. A failure at step
1 is reported to the engine as `ErrorType_Connectivity` and at step 2 as `ErrorType_TLS`, and either
aborts the check before any gRPC is attempted. There is a declared `HeadRequestTimeout` of 8 seconds
in that file, but it is never referenced; the requests inherit only the caller's context deadline.

The `check` value in `server_config.json` is a bare host plus path, not a URL, and the robot prefixes
the scheme itself. Historic escape-pod configs used the literal value `escapepod.local/ok:80`, which
produces the request path `/ok:80`, colon and all; the sample JSON still sitting in a comment above
`CreateServerConfig` in `wire-pod/chipper/pkg/wirepod/setup/certs.go` has that form, while the value
the function now writes is `escapepod.local/ok`. wire-pod registers both `/ok` and `/ok:80` on both
its listeners (`wire-pod/chipper/pkg/initwirepod/startserver.go`,
`wire-pod/chipper/pkg/wirepod/sdkapp/jdocspinger.go`), and any replacement server must too. Test the
colon form against your router explicitly rather than assuming it survives path parsing.

This `/ok` endpoint is also the robot's liveness heartbeat, and wire-pod hangs two side effects off
it: if the caller's IP is not in `botInfo.json` it kicks off an mDNS re-broadcast, and if the robot
has been quiet for more than fifteen seconds it calls `PullJdocs` back at the robot. That second one
exists for a reason worth knowing: the robot appends the escape-pod CA to its trust pool only when
it creates a jdocs connection, which does not happen on every boot, so the server pokes the robot to
force it.

### 2.2 ChipperGrpc, the voice service

Defined in `wire-pod-rs/crates/wirepod-proto/proto/chipper/chipperpb.proto`, package `chippergrpc2`.

| RPC | Request | Response | Robot uses it? |
|---|---|---|---|
| `TextIntent` | `TextRequest` | `IntentResponse` | No. wire-pod returns `Unimplemented`. |
| `StreamingIntent` | stream `StreamingIntentRequest` | stream `IntentResponse` | Not on WireOS. Keep it for stock firmware. |
| `StreamingKnowledgeGraph` | stream `StreamingKnowledgeGraphRequest` | stream `KnowledgeGraphResponse` | Yes, for "I have a question". |
| `StreamingIntentGraph` | stream `StreamingIntentGraphRequest` | stream `IntentGraphResponse` | Yes, for every ordinary voice request. Required. |
| `StreamingConnectionCheck` | stream `StreamingConnectionCheckRequest` | stream `ConnectionCheckResponse` | Yes. Required. |

A WireOS robot never opens a plain `StreamingIntent` stream. The version test that used to choose
between `StreamingIntent` and `StreamingIntentGraph` was deleted, and the plain stream was removed
from the dialer and from the response switch as well
(`wire-os-victor/cloud/internal/voice/process.go`,
`wire-os-victor/cloud/internal/voice/stream/context.go`). A server that answers an intent-graph
stream with an `IntentResponse` gets its answer dropped on the floor.

#### What the robot puts in the request

The first message of a stream carries the metadata and the first audio chunk; subsequent messages
carry audio only. Fields the robot actually sets, from
`wire-os-victor/cloud/internal/voice/process.go` and `.../stream/connect.go`:

| Field | What the robot puts there |
|---|---|
| `session` | The first 16 characters of a UUID. |
| `device_id` | The robot ESN, read from `/bin/emr-cat e`. |
| `input_audio` | 120 ms chunks of 16 kHz 16-bit mono, Opus-compressed at 66 kibibit with complexity 0 and a 60 ms frame size. |
| `language_code` | Mapped from the hotword event's locale, defaulting to `en-US`. |
| `firmware_version` | `robot.OSVersion()`, built from the `ro.build.fingerprint`, `ro.revision`, `ro.anki.os_build_comment` and `ro.build.version.release` properties. |
| `boot_id` | The contents of `/proc/sys/kernel/random/boot_id`. |
| `mode` | `VOICE_COMMAND` normally, `GAME` for blackjack. |
| `timezone` | Set on the knowledge-graph and intent-graph requests only. |
| `audio_encoding`, `skip_das`, `save_audio`, `app_key` | Set from the stream options. wire-pod reads none of them except the encoding. |

Three of those deserve a sentence. The server tells Opus from PCM by sniffing the first byte of
`input_audio`: `0x4F` means Ogg-Opus and anything else is raw 16 kHz s16le PCM. The language mapping
distinguishes only French, German, English UK and English AU, and every other language falls back to
`ENGLISH_US`. On WireOS the firmware version is a 3.0.1 string, so no server-side branch may assume a
`1.8.` or `2.0.` prefix.

The whole stream has a 9 second timeout on the robot side. If no response has arrived by then the
robot reports `ErrorType_Timeout` to the engine and gives up.

#### What the robot does with the answer

The robot waits for exactly one response and then closes the stream
(`wire-os-victor/cloud/internal/voice/stream/context.go`). For an `IntentGraphResponse` it asks the
`api-clients` helper `chipper.IsIntent` which half of the message to read. wire-pod sets
`response_type` to `INTENT` or `KNOWLEDGE_GRAPH` accordingly
(`wire-pod/chipper/pkg/wirepod/ttr/matchIntentSend.go`), and the port should do the same.

When the response is an intent, the robot forwards a CLAD `IntentResult` to the engine carrying
`intent_result.action` as the intent name and a JSON encoding of `intent_result.parameters` as the
parameters. When it is a knowledge-graph answer, the robot synthesises the intent name
`intent_knowledge_response_extend_bypass` (or `intent_knowledge_response_extend` for a genuine
`StreamingKnowledgeGraph` stream) and builds the parameter map itself from `spoken_text`,
`command_type`, `query_text` and `domains_used`.

The intent names the engine will accept are not open-ended. They are listed in
`wire-os-victor/resources/config/engine/behaviorComponent/user_intent_map.json`, which is the
authoritative list: 65 entries under `user_intent_map`, two more under `simple_voice_responses`, and
16 names under `unmatched_intent`. Five of those entries carry a `cloud_substitutions` map that
renames the parameter keys the server sends into the engine's internal names, so the server must use
the left-hand spelling. For example the timer intent expects a parameter named `timer_duration`, and
the weather intent expects `speakable_location_string`, `is_forecast`, `temperature_unit`,
`local_datetime` and `day_or_night`. An intent name that is not in that file reaches
`intentUnmatched`.

#### The connection-check stream specifically

The robot sends 6000 ms of random bytes in 120 ms chunks, which is 50 frames, and sets
`total_audio_ms = 6000` and `audio_per_request = 120`. Each frame is
`16000 * 120 / 1000 * 2` = 3840 bytes before Opus compression, so 192000 bytes in total over about
six seconds. The bytes are deliberately random, with a comment saying so, because random data
compresses badly and makes the test conservative.

The server counts frames and replies once with a `ConnectionCheckResponse`. Two fields are
load-bearing. `frames_received` is compared against `total_audio_ms / audio_per_request` on the
robot, and `status` must be the exact string `Success` or the robot reports
`ConnectionCode_Bandwidth` instead of `ConnectionCode_Available`
(`wire-os-victor/cloud/internal/voice/stream/context.go`). Note also that `num_packets` and
`expected_packets` are narrowed to `uint8` on the robot, so a frame count above 255 wraps. The proto
comment on `input_audio` says 100 ms chunks; the robot sends 120 ms and reports the real value in
`audio_per_request`, so read the field rather than the comment.

### 2.3 Jdocs, the document service

Defined in `wire-pod-rs/crates/wirepod-proto/proto/jdocs/jdocs.proto`, package `jdocspb`. Documents
are keyed by two values the robot derives, not configures. `user_id` is the `user_id` claim of the
stored JWT, which is why wire-pod's literal `wirepod` becomes the account key for every document. If
no token is available the engine substitutes the string `NotLoggedIn` and then silently drops every
jdocs request, so a robot that never got a parseable JWT will look like a robot with no jdocs traffic
rather than one reporting an error. `thing` is `vic:<esn>`, taken from the subject common name of the
factory certificate in `/factory/cloud/`, with the ESN in lowercase hex.

| RPC | Request | Response | Robot uses it? |
|---|---|---|---|
| `ReadDocs` | `ReadDocsReq` | `ReadDocsResp` | Yes. Required. |
| `WriteDoc` | `WriteDocReq` | `WriteDocResp` | Yes. Required. |
| `DeleteDoc` | `DeleteDocReq` | `DeleteDocResp` | The robot's CLAD surface has a delete and the cloud process translates it, but nothing in normal operation calls it and wire-pod leaves it unimplemented. |
| `PurgeAccountDocs` | `PurgeAccountDocsReq` | `PurgeAccountDocsResp` | No. |
| `ViewAccountDocs` | `ViewAccountDocsReq` | `ViewDocsResp` | No. |
| `ViewAccountDocsWithPII` | `ViewAccountDocsReq` | `ViewDocsResp` | No. |

A `Jdoc` is four fields: `doc_version` (1 is the first version, 0 means it does not exist),
`fmt_version`, `client_metadata` (a free string, at most 32 characters by convention) and `json_doc`
(the payload as a JSON string, not a nested message). `WriteDoc` is rejected with
`REJECTED_BAD_DOC_VERSION` if the version the client sends does not match the service's, and a
client creates a document by sending `doc_version = 0`.

There are exactly five document names, and which process owns each one decides what a server can
expect to see:

| Document | Owner on the robot | Direction | `fmt_version` |
|---|---|---|---|
| `vic.AppTokens` | the gateway inside `vic-cloud` | read only | not set by the robot |
| `vic.RobotSettings` | `vic-engine`, `JdocsManager` and `SettingsManager` | read and write | 2 |
| `vic.AccountSettings` | `vic-engine`, `AccountSettingsManager` | read and write | 1 |
| `vic.UserEntitlements` | `vic-engine`, `UserEntitlementsManager` | read and write | 1 |
| `vic.RobotLifetimeStats` | `vic-engine`, `RobotStatsTracker` | read and write | 1 |

Of these, `vic.AppTokens` is the one the server side must get right, because it is how the robot
learns which SDK client tokens to accept. Its shape
is one key, `client_tokens`, holding a list of objects with exactly four fields that either side
reads: `hash`, `client_name`, `app_id` and `issued_at`
(`wire-os-victor/cloud/cloud/tokens.go`, `wire-pod/chipper/pkg/servers/token/hashing.go`). An
`is_primary` field appears in the hardcoded fallback document wire-pod serves to an unknown robot,
but neither side's struct declares it. The `hash` is not bcrypt: it is
`base64(sha256(token_bytes || salt) || salt)` with a 16-byte token and a 16-byte salt, so 48 raw
bytes and 64 base64 characters, and the plaintext token is `base64(token_bytes)`, 24 characters
including padding. The robot's comparison is in
`wire-os-victor/cloud/internal/token/compare_and_hash.go` and the server's generator is in
`wire-pod/chipper/pkg/servers/token/hashing.go`; they agree byte for byte, and existing jdocs on
disk hold Go-produced hashes, so this is one of the places where the port must not drift.

The other four documents belong to the engine, which declares them in
`wire-os-victor/resources/config/engine/jdocs_config.json` and rejects any other name. The engine
reads all four in one batched `ReadDocs` at startup, each item with `my_doc_version = 0`, but only if
more than 7200 seconds have passed since its last successful cloud read or a local file was missing.
Writes go out one document at a time, throttled by per-document abuse rules that escalate the minimum
save period to 10, then 30 or 60, then 120 or 300 seconds; `vic.RobotLifetimeStats` instead has a
flat cloud period of 86400 seconds. A server should not expect chatty jdocs traffic.

`vic.RobotSettings` is at `fmt_version` 2 and its body is a flat object whose keys are the lowercase
`RobotSetting` enum names. The defaults, from
`wire-os-victor/resources/config/engine/settings_config.json`, are:

```json
{
  "clock_24_hour": false,
  "eye_color": 0,
  "custom_eye_color": { "hue": 0, "saturation": 0, "enabled": false },
  "default_location": "San Francisco, California, United States",
  "dist_is_metric": false,
  "locale": "en-US",
  "master_volume": 4,
  "temp_is_fahrenheit": true,
  "time_zone": "America/Los_Angeles",
  "button_wakeword": 0
}
```

Note `custom_eye_color`, which is the same key that shifts the proto field numbers in
[3.6](#36-the-vendored-sdk-proto-does-not-match-the-robots). The engine normalises this object on
every boot, filling in missing keys from the defaults and deleting keys it does not recognise, so a
server cannot smuggle extra fields into it. `vic.AccountSettings` holds `DATA_COLLECTION` and
`APP_LOCALE`; `vic.UserEntitlements` holds `KICKSTARTER_EYES`; `vic.RobotLifetimeStats` is an
open-ended flat object of `Category.Stat` counters created lazily on first increment.

Two behaviours of the robot's reader are worth designing around. It guards against an empty `items`
list in the read response and returns an error rather than indexing
(`wire-os-victor/cloud/cloud/tokens.go`), but the server side of wire-pod does index `req.Items[0]`
without checking, so a read request with no items crashes the server rather than the robot
(`wire-pod/chipper/pkg/servers/jdocs/server.go`). Also, `ReadDocs` is where wire-pod does most of its
robot bookkeeping: it records the caller's IP against the ESN, matches pending token hashes from the
in-memory stores, writes the session certificate out to `~/.anki_vector` and to `session-certs/`, and
synthesises a `vic.AppTokens` document for a robot it has never seen. A port that treats `ReadDocs`
as a pure lookup will authenticate no robots.

### 2.4 Token, the identity service

Defined in `wire-pod-rs/crates/wirepod-proto/proto/token/token.proto`, package `tokenpb`.

| RPC | Request | Response | Robot uses it? |
|---|---|---|---|
| `AssociatePrimaryUser` | `AssociatePrimaryUserRequest` | `AssociatePrimaryUserResponse` | Yes, once, at association. Required. |
| `RefreshToken` | `RefreshTokenRequest` | `RefreshTokenResponse` | Yes, on a schedule. Required. |
| `AssociateSecondaryClient` | `AssociateSecondaryClientRequest` | `AssociateSecondaryClientResponse` | Yes, when a second client is added. |
| `ReassociatePrimaryUser` | `ReassociatePrimaryUserRequest` | `ReassociatePrimaryUserResponse` | Not implemented by wire-pod. |
| `DisassociatePrimaryUser` | `DisassociatePrimaryUserRequest` | `DisassociatePrimaryUserResponse` | Not implemented by wire-pod. |
| `ListRevokedTokens` | `ListRevokedTokensRequest` | `ListRevokedTokensResponse` | Service-to-service only. Not implemented. |
| `RevokeFactoryCertificate`, `RevokeTokens` | | | Admin only. Not implemented. |

Every response wraps a `TokenBundle` with `token` (the JWT) and `client_token` (the SDK GUID). The
`sts_token` field is left empty by wire-pod.

`AssociatePrimaryUser` populates exactly one proto field, `session_certificate`, and its contents are
the raw bytes of `/data/vic-gateway/gateway.cert`, the robot's own gateway TLS certificate, read
fresh on every call (`wire-os-victor/cloud/internal/token/client.go`). `client_name` and `app_id`
reach the robot's cloud process over CLAD but are dropped on this path; they are sent as proto fields
only by `AssociateSecondaryClient` and `ReassociatePrimaryUser`. The user session token is not a
proto field at all: it travels as the gRPC metadata key `anki-user-session`. wire-pod parses the
certificate only to read its issuer common name, which becomes the robot's display name such as
`Vector-B6H9`, and stores the raw bytes to write out later when the matching `ReadDocs` arrives
(`wire-pod/chipper/pkg/servers/token/token.go`). That works because the certificate is self-signed,
so issuer and subject carry the same name.

The metadata keys, all lowercase, are:

| Key | Sent on | Value |
|---|---|---|
| `anki-app-key` | every token, jdocs and chipper call | the `appkey` from `server_config.json` |
| `anki-user-session` | the three association calls | the session token supplied by the app or the SDK |
| `anki-access-token` | JWT refresh, and every jdocs and chipper call | the raw JWT string |

There is no `app-key` key without the `anki-` prefix. The `app_key` field inside the chipper request
messages is a separate thing, and wire-pod reads neither.

The JWT is the critical artefact. The robot parses it with `ParseUnverified` and never checks the
signature (`wire-os-victor/cloud/internal/token/identity/identity.go`), but it does reject the token
if any of six claims is missing or is not a JSON string
(`wire-os-victor/cloud/internal/token/identity/token.go`):

| Claim | Meaning | wire-pod's value |
|---|---|---|
| `token_id` | unique id for this token | a fresh UUID |
| `token_type` | | the literal `user+robot` |
| `user_id` | account identifier | the literal `wirepod` |
| `requestor_id` | robot identifier | `vic:<esn>`, or `vic:00601b50` before association |
| `iat` | issued at, RFC 3339 in UTC | `time.RFC3339Nano` |
| `expires` | expiry, RFC 3339 in UTC | one month out |

`iat` and `expires` are parsed as RFC 3339 strings, not as numeric epochs. A `permissions` claim is
optional. wire-pod signs with RS512 using a throwaway 1024-bit RSA key generated per request, which
works only because nothing verifies it.

The refresh logic is in `wire-os-victor/cloud/internal/token/refresher.go` and is worth knowing in
full, because several of its branches look like server faults from the outside. The refresh time is
`expires` minus three hours. While no token exists at all the robot sleeps in five-minute
increments. While `time.Now()` is still before the token's `iat` it sleeps in twenty-second
increments waiting for NTP to correct the clock, so a JWT issued with a clock ahead of the robot's
stalls everything until the robot's clock catches up. Otherwise it sleeps until the refresh time
plus ten seconds. An expiry shorter than three hours therefore produces a refresh every five
minutes.

The token is written atomically to
`/data/data/com.anki.victor/persistent/token/token.jwt` by way of a `.tmp` file and a rename. A
token that parses but has an empty `user_id` is deleted as a legacy test token, and a token that
fails to parse is deleted outright. If token initialisation fails the robot writes fault code 851 to
`/run/fault_code`. Separately, a gRPC `PermissionDenied` from jdocs, voice or the log collector
starts an exponential backoff that repeatedly forces a JWT refresh, and a second `PermissionDenied`
during that retry sets a sticky denied flag that only a later successful token call clears. A server
that answers `PermissionDenied` while it is still starting up can therefore lock a robot out until
it is restarted.

The placeholder `requestor_id` of `vic:00601b50` is deliberate: on the first association wire-pod has
no access to the factory certificates the real servers used, so it cannot know the ESN yet. Later
token requests are matched to a robot by source IP and carry the real `vic:<esn>`.

### 2.5 What the robot will not work without

Ranked by what breaks first.

The token service comes before everything, because `Token/AssociatePrimaryUser` and
`Token/RefreshToken` are what give the robot a JWT, and without a parseable JWT the robot has no
`user_id` for jdocs and no credentials for chipper. `Jdocs/ReadDocs` and `Jdocs/WriteDoc` come next,
because without them the robot has no SDK tokens and no settings, and its own SDK surface on port 443
rejects every caller. `ChipperGrpc/StreamingIntentGraph` is third, because without it no voice
command works at all. `ChipperGrpc/StreamingConnectionCheck` together with the `check` HTTP endpoint
is fourth, because without them the robot reports no cloud connectivity, which shows on its face and
in the app but does not stop anything else. `ChipperGrpc/StreamingKnowledgeGraph` is last, and losing
it breaks only "I have a question". Every other RPC in the three protos can stay unimplemented.

---

## 3. Outbound: the SDK surface on the robot

### 3.1 Transport, port and authentication

`vic-gateway` no longer exists as a separate process on WireOS; the gateway is compiled into
`vic-cloud` and started as a goroutine (`wire-os-victor/cloud/cloud/gateway.go`). It listens on TCP
443 (`wire-os-victor/cloud/cloud/config_linux.go`) with a TLS config whose `NextProtos` is `["h2"]`
only, and serves both the gRPC service and a grpc-gateway JSON mux from the same handler.

Authentication is a check on the HTTP `Authorization` header, in
`wire-os-victor/cloud/cloud/config_linux.go`:

- The header must be present exactly once.
- It must start with `Bearer ` (a `Basic ` prefix is recognised but its handling is an unfinished
  `todo` and mis-slices the value by one character, so treat Basic as unsupported).
- The remainder is compared against the hashes in `vic.AppTokens`, most-recently-used first, and
  against a per-runtime token that WireOS writes to `/run/vic-cloud/perRuntimeToken` at startup.
- One path is exempt: `/Anki.Vector.external_interface.ExternalInterface/UserAuthentication`.

The hash list is cached on the robot at `/data/vic-gateway/token-hashes.json` and is refreshed from
jdocs at most once a minute, three times in fifteen minutes and six times an hour, and only when the
cache is over an hour old. So a token the server has just written into `vic.AppTokens` is not
necessarily accepted immediately. `UserAuthentication` itself is rate limited separately at ten calls
per ten seconds and twenty-five per ten minutes, returning `ResourceExhausted` beyond that; it
proxies to `vic-switchboard` with `AppId: "SDK"` and force-refreshes the cache on success.

The port does this correctly today: `wire-pod-rs/crates/wirepod-vector/src/conn.rs` attaches
`authorization: Bearer <guid>` through a tonic interceptor.

One structural property of the gateway shapes how a client should use it. Engine responses are routed
back to waiting RPCs by protobuf message type, not per request: `CreateChannel` keys listeners on the
response type's name and fans each message out to every listener registered for it
(`wire-os-victor/cloud/cloud/ipc_manager.go`). Only `BehaviorControl` and `AssumeBehaviorControl`
additionally filter on the connection id. Two clients issuing the same call concurrently can
therefore receive each other's answers. Serialise per robot, which is what wire-pod's per-robot
camera op lock effectively does.

The robot's certificate is self-signed and names the robot, not an IP, so every client in this
ecosystem skips verification. wire-pod uses `client.WithInsecureSkipVerify()` for gRPC and
`InsecureSkipVerify: true` for the REST calls
(`wire-pod/chipper/pkg/wirepod/sdkapp/robot.go`, `.../urlreqs.go`), and the port reproduces that with
an accept-all rustls verifier.

### 3.2 Behaviour control

`BehaviorControl` is a bidirectional stream and `AssumeBehaviorControl` is the server-streaming
variant that takes a single request. Both carry `BehaviorControlRequest`, a oneof of `ControlRequest`
(with a `Priority`) and `ControlRelease`. The robot answers with `BehaviorControlResponse`, a oneof of
`ControlGrantedResponse`, `ControlLostResponse`, `ReservedControlLostResponse` and `KeepAlivePing`,
and sends a `KeepAlivePing` on that stream once per second.

Priorities, from `behavior.proto`:

| Priority | Value | Effect on the robot |
|---|---|---|
| `UNKNOWN` | 0 | Silently dropped. `SDKComponent::HandleProtoMessage` returns early on a zero priority, so the stream never receives `ControlGrantedResponse`. Always set a priority. |
| `OVERRIDE_BEHAVIORS` | 10 | Activates the `SDKOverrideAll` behaviour, which sits above shut-up mode, quiet mode and the sleep cycle, and sets `disableCliffDetection`. What wire-pod uses. |
| `DEFAULT` | 20 | Activates `SDKDefault`, directly below mandatory physical reactions. |
| `RESERVE_CONTROL` | 30 | Takes the behaviour lock only. It grants `ControlGrantedResponse` immediately but never sets the engine's `_sdkWantsControl`, so the SDK behaviour does not activate and none of the action or vision privileges below apply. |

The behaviour definitions are
`wire-os-victor/resources/config/engine/behaviorComponent/behaviors/victorBehaviorTree/sdkBehaviors/SDKOverrideAll.json`
and `SDKDefault.json`, and the engine side is
`wire-os-victor/engine/aiComponent/behaviorComponent/behaviors/sdkBehaviors/behaviorSDKInterface.cpp`.

Control is not permanent. A higher-priority behaviour on the robot takes it back and the client gets
a `ControlLostResponse`. Losing control tears down every vision mode the SDK had enabled except
image streaming, restores camera auto-exposure, reapplies saved settings (which undoes an SDK eye
colour), unlocks animation tracks and re-enables cliff detection, and the robot announces the vision
part with a `VisionModesAutoDisabled` event.

Three asymmetries in the release path are worth knowing:

- Closing a `BehaviorControl` stream releases control, because the gateway writes a `ControlRelease`
  in a `defer`. Closing an `AssumeBehaviorControl` stream does not; that handler has no such defer,
  so the SDK behaviour keeps running after the client has gone.
- An explicit `ControlRelease` zeroes the connection id before the behaviour deactivates, so the
  `ControlLostResponse` that follows is tagged with id 0 and is filtered out. You get no
  confirmation of your own release. An involuntary loss is delivered correctly.
- Requesting control wakes a sleeping robot. `WakeReason::SDK` is registered against
  `SDKWantsControl()` in
  `wire-os-victor/engine/aiComponent/behaviorComponent/behaviors/sleeping/behaviorSleepCycle.cpp`,
  and it is the only SDK message that does so. This is the practical workaround for the camera hang
  in section 6: take behaviour control first, then ask for the camera.

Action RPCs that move the robot take an `id_tag`, and the gateway rejects it with `InvalidArgument`
unless it is between `FIRST_SDK_TAG` = 2000001 and `LAST_SDK_TAG` = 3000000
(`ValidateActionTag` in `wire-os-victor/cloud/cloud/message_handler.go`). A zero-valued default is
rejected. `CancelActionByIdTag` cancels by the same tag.

#### What happens when you call without control

There is no single answer, and this is the part that misleads people. The gateway does no gating at
all; it forwards everything. Three different things happen further in.

**Seven requests are explicitly refused in `SDKComponent`**, which replies with
`ResponseStatus::FORBIDDEN`: `EnableMarkerDetection`, `EnableFaceDetection`, `EnableMotionDetection`,
`EnableMirrorMode`, `SayText`, `SetCameraSettings` and `SetEyeColor`. The check is on the SDK
behaviour being activated, so a `RESERVE_CONTROL` lock does not satisfy it. What the client sees is
not the refusal, because the gateway overwrites the status field: for the four vision toggles and
`SetCameraSettings` the client gets `RESPONSE_RECEIVED` for a request that was refused, and only
`SetCameraSettings` leaks the truth through its untouched `status_message`. `SayText` is the
exception that reports honestly, as `codes.Internal, "Failed to say text"`, because the refusal
leaves its `state` field at the invalid zero value and the gateway maps that to an error.
`SetEyeColor` is worse than either: the gateway registers no listener for the response at all and
returns `REQUEST_PROCESSING` immediately, so the refusal is discarded and the client sees success
while nothing happens.

**Action RPCs are queued regardless and then never answered.** `GoToPose`, `DriveStraight`,
`TurnInPlace`, `SetHeadAngle`, `SetLiftHeight`, `TurnTowardsFace`, `GoToObject`, `RollObject`,
`PopAWheelie`, `PickupObject`, `PlaceObjectOnGroundHere` and `DockWithCube` are queued by
`RobotEventHandler` whether or not the SDK holds control, but their `*Response` messages are produced
only by `SDKComponent::OnActionCompleted`, which runs only while the SDK behaviour is activated.
Calling one of these without control hangs the gRPC call indefinitely while the robot may still
attempt the motion.

**The four direct motor RPCs are silently ignored.** `DriveWheels`, `MoveHead`, `MoveLift` and
`StopAllMotors` are translated to CLAD and land in `MovementComponent`, whose handlers check
`_allowExternalMovementCommands` and do nothing when it is unset
(`wire-os-victor/engine/components/movementComponent.cpp`). The gateway returns
`REQUEST_PROCESSING` immediately, so the client sees success.

Calls that need nothing at all include `BatteryState`, `VersionState`, `PhotosInfo`, `Photo`,
`Thumbnail`, `PullJdocs`, `IsImageStreamingEnabled`, `EnableImageStreaming`, `AudioFeed` and
`ExternalAudioStreamPlayback`. Image streaming is deliberately ungated, with a comment in
`sdkComponent.cpp` saying so, which is why wire-pod's camera page works without taking control.

### 3.3 The RPC list by area

The port vendors 88 RPCs in `external_interface.proto`. The robot defines 90; the two extra are
`UpdateUserEntitlements` and `UploadDebugLogs`, and everything the port knows about is present on the
robot. Streams are marked. "Control" marks calls that only do anything while the caller holds
behaviour control; see the note below the tables.

**Behaviour control and session**

| RPC | Request | What it does |
|---|---|---|
| `ProtocolVersion` | `ProtocolVersionRequest` | Negotiates the wire protocol; current version is 5. |
| `SDKInitialization` | `SDKInitializationRequest` | Reports client OS and Python versions for analytics only. |
| `UserAuthentication` | `UserAuthenticationRequest` | Exchanges a user session id for a client token GUID. The one RPC exempt from bearer auth. |
| `BehaviorControl` (bidi stream) | `BehaviorControlRequest` | Requests and releases behaviour control. |
| `AssumeBehaviorControl` (stream) | `BehaviorControlRequest` | Same, as a single request with a streamed response. |
| `CancelBehavior` | `CancelBehaviorRequest` | Cancels the running SDK behaviour. |
| `CancelActionByIdTag` | `CancelActionByIdTagRequest` | Cancels an action by its `id_tag`. |

**Motion and actions (control)**

| RPC | Request | What it does |
|---|---|---|
| `DriveWheels` | `DriveWheelsRequest` | Sets left and right wheel speed and acceleration directly. |
| `DriveStraight` | `DriveStraightRequest` | Drives a distance at a speed as a cancellable action. |
| `TurnInPlace` | `TurnInPlaceRequest` | Rotates by an angle. |
| `MoveHead` | `MoveHeadRequest` | Sets head angular velocity. |
| `MoveLift` | `MoveLiftRequest` | Sets lift angular velocity. |
| `SetHeadAngle` | `SetHeadAngleRequest` | Moves the head to an absolute angle. |
| `SetLiftHeight` | `SetLiftHeightRequest` | Moves the lift to an absolute height. |
| `StopAllMotors` | `StopAllMotorsRequest` | Stops head, lift and wheels. |
| `GoToPose` | `GoToPoseRequest` | Drives to an x, y, angle pose using the planner. |
| `GoToObject` | `GoToObjectRequest` | Drives to a known object. |
| `DockWithCube` | `DockWithCubeRequest` | Approaches and docks with the cube. |
| `PickupObject`, `PlaceObjectOnGroundHere`, `RollObject`, `PopAWheelie` | matching `*Request` | Cube manipulation actions. |
| `RollBlock` | `RollBlockRequest` | Rolls the cube regardless of relative pose. |
| `DriveOffCharger`, `DriveOnCharger` | matching `*Request` | Charger entry and exit. |
| `TurnTowardsFace` | `TurnTowardsFaceRequest` | Turns to a known face id. |
| `FindFaces`, `LookAroundInPlace` | matching `*Request` | Search behaviours. |

**Speech and audio**

| RPC | Request | What it does |
|---|---|---|
| `SayText` | `SayTextRequest` | Speaks text. Takes `use_vector_voice`, `duration_scalar` from 0.05 to 20.0, and `pitch_scalar` from -1.0 to 1.0. |
| `ExternalAudioStreamPlayback` (bidi stream) | `ExternalAudioStreamRequest` | Plays client-supplied 16-bit little-endian PCM. The request is a oneof of prepare, chunk, complete and cancel; chunks are at most 1024 bytes and the prepare message sets a frame rate between 8000 and 16025 and a volume from 0 to 100. |
| `AudioFeed` (stream) | `AudioFeedRequest` | Streams 1600 samples per message with beam direction and noise-floor metadata. On this firmware the samples are not the microphone; see below. |
| `SetMasterVolume` | `MasterVolumeRequest` | Sets the speaker volume level. |

`AudioFeed` does not carry microphone audio on a WireOS robot. The only producer of
`external_interface::AudioChunk` in the tree is `SDKComponent::UpdateDependent`, and it synthesises a
1 kHz sine wave with a comment calling itself a "temporary 1 kHz sine wave generator to test over the
wire transfer", zeroed `direction_strengths` and `noise_floor_power`, and `source_direction` set to
the invalid value 12. The gRPC plumbing is complete and the stream works; the content is a test tone.

There is no RPC for `AudioSendMode` either. `AudioSendModeRequest` and `AudioSendModeChanged` exist
as messages and the engine does consume the request, but no method on `ExternalInterface` takes them
in either proto, and the engine's handler only sets a boolean and echoes the mode back, so
`AUDIO_FAST_MODE`, `AUDIO_DIRECTIONAL_MODE` and `AUDIO_VOICE_DETECT_MODE` are indistinguishable.
Playback in the other direction, `ExternalAudioStreamPlayback`, is fully implemented and reaches the
animation process.

**Animation and face**

| RPC | Request | What it does |
|---|---|---|
| `PlayAnimation` | `PlayAnimationRequest` | Plays a named animation, with loop count and ignore flags. |
| `PlayAnimationTrigger` | `PlayAnimationTriggerRequest` | Plays an animation by trigger name, letting the robot choose a variant. |
| `ListAnimations` | `ListAnimationsRequest` | Returns every animation name. |
| `ListAnimationTriggers` | `ListAnimationTriggersRequest` | Returns every trigger name. |
| `DisplayFaceImageRGB` | `DisplayFaceImageRGBRequest` | Draws a raw image on the screen for `duration_ms`, optionally interrupting what is there. See the pixel format note below. |
| `SetEyeColor` | `SetEyeColorRequest` | Sets eye hue and saturation as floats. Produces no response and is undone when the SDK behaviour deactivates. |

**Camera and vision**

| RPC | Request | What it does |
|---|---|---|
| `EnableImageStreaming` | `EnableImageStreamingRequest` | Turns the image stream on or off, optionally at high resolution. |
| `IsImageStreamingEnabled` | `IsImageStreamingEnabledRequest` | Reports the current state. |
| `CameraFeed` (stream) | `CameraFeedRequest` | Streams reassembled frames as `CameraFeedResponse` with `frame_time_stamp`, `image_id`, `image_encoding` and `data`. |
| `CaptureSingleImage` | `CaptureSingleImageRequest` | Captures one frame, 640x360 by default and 1280x720 with `enable_high_resolution`. Implemented entirely in the gateway as enable, grab one frame, disable; the engine's subscription for it is commented out. |
| `EnableMirrorMode` | `EnableMirrorModeRequest` | Shows the camera feed on the robot's own screen. |
| `EnableMarkerDetection`, `EnableFaceDetection`, `EnableMotionDetection` | matching `*Request` | Toggle vision modes that drive events. |
| `GetCameraConfig` | `CameraConfigRequest` | Returns intrinsics, field of view and the exposure and gain limits. |
| `SetCameraSettings` | `SetCameraSettingsRequest` | Sets gain, exposure and auto-exposure. |
| `NavMapFeed` (stream) | `NavMapFeedRequest` | Streams the navigation memory map at a requested frequency. |

**Faces**

| RPC | Request | What it does |
|---|---|---|
| `RequestEnrolledNames` | `RequestEnrolledNamesRequest` | Lists known faces as `LoadedKnownFace` records. |
| `SetFaceToEnroll` | `SetFaceToEnrollRequest` | Names the face id and name for the next enrollment. |
| `EnrollFace` | `EnrollFaceRequest` | Runs the enrollment. Must follow `SetFaceToEnroll`. |
| `CancelFaceEnrollment` | `CancelFaceEnrollmentRequest` | Aborts enrollment. |
| `UpdateEnrolledFaceByID` | `UpdateEnrolledFaceByIDRequest` | Renames a face. |
| `EraseEnrolledFaceByID`, `EraseAllEnrolledFaces` | matching `*Request` | Forget one or all faces. |

**Photos**

| RPC | Request | What it does |
|---|---|---|
| `PhotosInfo` | `PhotosInfoRequest` | Lists `PhotoInfo` records with ids and timestamps. |
| `Photo` | `PhotoRequest` | Returns one full photo as bytes. |
| `Thumbnail` | `ThumbnailRequest` | Returns one thumbnail as bytes. |
| `DeletePhoto` | `DeletePhotoRequest` | Deletes one photo. |

**Cubes and custom objects**

| RPC | Request | What it does |
|---|---|---|
| `ConnectCube`, `DisconnectCube` | matching `*Request` | Open and close the cube BLE link. |
| `CubesAvailable` | `CubesAvailableRequest` | Scans for cubes. |
| `FlashCubeLights` | `FlashCubeLightsRequest` | Plays the default cube flash. |
| `SetCubeLights` | `SetCubeLightsRequest` | Sets each LED with two colours and transition timings. |
| `ForgetPreferredCube`, `SetPreferredCube` | matching `*Request` | Manage the saved cube preference. |
| `DefineCustomObject`, `CreateFixedCustomObject`, `DeleteCustomObjects` | matching `*Request` | Custom marker objects and fixed obstacles. |

**Settings, jdocs and state**

| RPC | Request | What it does |
|---|---|---|
| `PullJdocs` | `PullJdocsRequest` | Returns the robot's own copies of the named jdoc types. This is how wire-pod reads `vic.RobotSettings` back from the robot. |
| `UpdateSettings` | `UpdateSettingsRequest` | Changes robot settings. Read [3.6](#36-the-vendored-sdk-proto-does-not-match-the-robots) before calling this over gRPC. |
| `UpdateAccountSettings` | `UpdateAccountSettingsRequest` | Changes account-level settings. |
| `BatteryState` | `BatteryStateRequest` | Battery level, voltage, charging and charger state, plus cube battery. |
| `VersionState` | `VersionStateRequest` | OS version and engine build id. |
| `GetLatestAttentionTransfer` | `LatestAttentionTransferRequest` | Why the robot last broke off what it was doing. |
| `GetFeatureFlag`, `GetFeatureFlagList` | matching `*Request` | Feature gates. |
| `CheckCloudConnection` | `CheckCloudRequest` | Asks the robot to run its own connection check against the configured server. |

**Onboarding, updates and Alexa**

| RPC | Request | What it does |
|---|---|---|
| `GetOnboardingState`, `SendOnboardingInput` | matching `*Request` | Drive the out-of-box flow. |
| `AppIntent` | `AppIntentRequest` | Injects an intent as if the app had sent it. |
| `StartUpdateEngine`, `CheckUpdateStatus`, `UpdateAndRestart` | matching `*Request` | OTA control. |
| `GetAlexaAuthState`, `AlexaOptIn` | matching `*Request` | Alexa enrollment. |

**Events**

| RPC | Request | What it does |
|---|---|---|
| `EventStream` (stream) | `EventRequest` | Streams `EventResponse`, each wrapping one `Event`. |

Which of these need behaviour control is decided in the engine, not in the gateway, and the three
different failure modes are set out in [3.2](#32-behaviour-control). wire-pod's own scripting layer
takes `OVERRIDE_BEHAVIORS` before `SayText` and releases afterwards
(`wire-pod/chipper/pkg/wirepod/ttr/bcontrol.go`), and calls `BatteryState` and `PullJdocs` with no
control at all (`wire-pod/chipper/pkg/wirepod/sdkapp/jdocspinger.go`).

Two of these calls end their reply with a sentinel rather than a count. `ListAnimations` and
`ListAnimationTriggers` send one response message per entry and then a final entry whose name is the
literal string `EndOfListAnimationsResponses`. A client that does not stop on that string will treat
it as an animation.

`DisplayFaceImageRGB.face_data` is not RGB888 and it is not standard RGB565. The packing wire-pod
uses, inherited from the fforchino SDK, is one little-endian `uint16` per pixel laid out as
`000bbbbbrrrrrggg`: three unused high bits, then five bits of blue, five of red, and three of green
(`wire-pod/chipper/pkg/scripting/display.go`). Green loses the most precision. Get this wrong and the
image renders with the colour channels swapped rather than failing.

### 3.4 The event stream in detail

`EventRequest` carries a `connection_id` and an optional filter, either a `white_list` or a
`black_list` of strings. The strings are the protobuf field names of the `Event` oneof, spelled in
`snake_case`: `robot_state`, `wake_word`, `user_intent`, `photo_taken`, `jdocs_changed`,
`stimulation_info`, and so on (`checkFilters` in
`wire-os-victor/cloud/cloud/message_handler.go`). With neither list set, everything is delivered.

The stream's first message is always a `ConnectionResponse` whose `is_primary` says whether this
connection id won the single primary slot. The robot then sends a `KeepAlivePing` event once per
second for as long as the stream is open, and that ping bypasses the filter, so a silent stream means
a dead stream. A connection id is accepted but not required, and a non-primary connection is not
rejected: it receives the full event stream. Being primary only decides who triggers the DAS
connection events and whose disconnect sends `AppDisconnected` to the engine. If `vic-switchboard`
reports a BLE client already holding a different id, the new caller is non-primary.

The full vocabulary of `Event.event_type` is `time_stamped_status`, `onboarding`, `wake_word`,
`attention_transfer`, `robot_observed_face`, `robot_changed_observed_face_id`, `object_event`,
`stimulation_info`, `photo_taken`, `robot_state`, `cube_battery`, `keep_alive`,
`connection_response`, `jdocs_changed`, `alexa_auth_event`, `mirror_mode_disabled`,
`vision_modes_auto_disabled`, `check_update_status_response`, `user_intent`, `robot_observed_motion`,
`robot_erased_enrolled_face`, `robot_renamed_enrolled_face`, `camera_settings_update` and
`unexpected_movement`. `robot_state` is emitted every engine tick, so an unfiltered stream is busy.

Nothing has to be enabled to open the stream, but three groups of events depend on something else.
The vision-derived ones (`robot_observed_face`, `robot_observed_motion`, and object observations
through `object_event`) fire only while the matching vision mode is on, and those toggles need
behaviour control. `user_intent` is forwarded only while the SDK behaviour is activated, and only
for the intents whitelisted in `behaviorSDKInterface.cpp`, so most voice commands never appear on the
stream. The cube and face events arrive over the legacy CLAD socket and are converted in the gateway
rather than produced as protobuf by the engine.

### 3.5 The REST mirror

Every RPC that carries a `google.api.http` option in `external_interface.proto` is also reachable as
JSON on the same port and TLS socket, at the path in that option, for example
`POST https://<robot>:443/v1/update_settings`. The marshaller is configured with
`EmitDefaultValues`, `UseEnumNumbers`, `UseProtoNames` and, on WireOS, `DiscardUnknown`
(`wire-os-victor/cloud/cloud/gateway.go`). So JSON uses proto field names, enums are numbers, and
unknown keys are silently dropped rather than rejected.

wire-pod uses this path rather than gRPC for every settings change
(`wire-pod/chipper/pkg/wirepod/sdkapp/urlreqs.go`), sending bodies such as
`{"update_settings": true, "settings": {"eye_color": 3}}` with the bearer header. The
`"update_settings": true` key is not a field of `UpdateSettingsRequest` and survives only because of
`DiscardUnknown`. The next section explains why the REST path is the right choice here and the gRPC
path is not.

### 3.6 The vendored SDK proto does not match the robot's

This is the one field-level divergence I found, and it is in a message the server writes.

`RobotSettingsConfig` in the robot's own
`wire-os-victor/tools/protobuf/gateway/public/settings.proto` has an extra member,
`CustomEyeColor custom_eye_color = 3`, and every field after it is shifted by one:

| Field | Port and fforchino SDK | Robot |
|---|---|---|
| `clock_24_hour` | 1 | 1 |
| `eye_color` | 2 | 2 |
| `custom_eye_color` | absent | 3 |
| `default_location` | 3 | 4 |
| `dist_is_metric` | 4 | 5 |
| `locale` | 5 | 6 |
| `master_volume` | 6 | 7 |
| `temp_is_fahrenheit` | 7 | 8 |
| `time_zone` | 8 | 9 |
| `button_wakeword` | 9 | 10 |

The `RobotSetting` enum is shifted the same way, with `custom_eye_color = 2` on the robot pushing
`default_location` to 3 and the rest down by one.

The consequence is that a binary `UpdateSettings` gRPC call built from the vendored proto sets the
wrong settings on a WireOS robot: `default_location` arrives as `custom_eye_color`, `locale` arrives
as `dist_is_metric`, and so on. JSON over the REST mirror is immune because grpc-gateway matches on
field names, which is exactly why wire-pod never calls `UpdateSettings` over gRPC. I checked
`fforchino/vector-go-sdk@v0.0.0-20231108155304-62168f3595d6`, the SDK wire-pod itself links, and it
has the unshifted numbering too, so this is not a wire-pod-specific mistake; both copies predate the
robot's.

I compared field numbers across `settings.proto`, `messages.proto`, `shared.proto`, `behavior.proto`,
`cube.proto`, `nav_map.proto`, `alexa.proto`, `response_status.proto` and `external_interface.proto`
and found no other renumbering; the remaining differences are messages the robot has and the vendored
copy does not, or messages that live in a different file. The three inbound protos are byte-identical,
as noted in section 1.

### 3.7 Declared but not what the name suggests

Every one of the robot's 90 RPCs has a handler, so nothing returns `Unimplemented`. Several do less
than their name implies, and they are collected here so the list is in one place.

- `AudioFeed` streams a synthetic sine wave, and `AudioSendMode` has no RPC and no real effect. See
  the audio table in [3.3](#33-the-rpc-list-by-area).
- `CaptureSingleImage` never reaches the engine; the engine's subscription for it is commented out
  and the gateway implements it as enable, grab, disable.
- `SetEyeColor` sends the hue and saturation to the robot but produces no response and no error, and
  the colour is reverted when the SDK behaviour deactivates and settings are reapplied.
- `SetPreferredCube` carries a proto comment saying it is only used in simulation.
- `RobotStatusHistory` exists as a handler in the gateway but has no `rpc` entry in the service, so
  no client can reach it.
- `UpdateUserEntitlements` and `UploadDebugLogs` exist on the robot but are absent from the proto the
  port vendors, so the port cannot call them without adding them.

---

## 4. Setup and identity

### 4.1 How a robot is pointed at a server

`vic-cloud` reads its endpoints from `server_config.json`, and the lookup is two lines of
`wire-os-victor/cloud/internal/config/urls.go`: if `/data/data/server_config.json` can be opened, it
wins; otherwise `/anki/data/assets/cozmo_resources/config/server_config.json` is used. The check is
`os.Open` succeeding, not the file parsing, so an empty or malformed override still wins and then
fails to decode, leaving the process on the compiled-in defaults, which point at Anki's long-dead dev
servers.

The keys are exactly these, from the `URLs` struct in the same file:

| Key | Meaning | Escape-pod value |
|---|---|---|
| `jdocs` | host:port for the `Jdocs` service | `escapepod.local:8084` in the shipped resource, `escapepod.local:443` as wire-pod writes it |
| `tms` | host:port for the `Token` service | same as jdocs |
| `chipper` | host:port for `ChipperGrpc` | same as jdocs |
| `check` | host and path for the connection check, no scheme | `escapepod.local/ok` |
| `logfiles` | S3 URL for log upload. Parsed but unused; the log collector is commented out in this fork's `cloud/cloud/main.go`. | `s3://anki-device-logs-prod/victor` |
| `appkey` | sent as the `anki-app-key` metadata on every token, jdocs and chipper call | `oDoa0quieSeir6goowai7f` |
| `offboard_vision` | optional, host:port for offboard vision | absent |

WireOS ships four resource flavours and three of them, `development`, `beta` and `shipping`, are
byte-identical and point at `vicapi.pvic.xyz:8081`, the WireOS project's own cloud. Only the
`escapepod` flavour points at `escapepod.local`, and the CMake flag that selects it,
`ANKI_RESOURCE_ESCAPEPOD`, is not exported by the WireOS build scripts. A stock WireOS OTA is
therefore not an escape-pod image, and what makes it talk to wire-pod is the
`/data/data/server_config.json` override, the same mechanism as on stock firmware. Exactly one
flavour is installed per build, at
`/anki/data/assets/cozmo_resources/config/server_config.json`.

One other process reads this file. `vic-switchboard` looks at the same two paths in the same order
and reads only the `chipper` key, testing whether it contains the substring `escapepod.local` to
decide which product byte to put in its BLE advertisement
(`wire-os-victor/platform/switchboard/switchboardd/daemon.cpp`). Nothing else in the tree reads it.

The port 8084 in the shipped escape-pod config is why wire-pod opens a second TLS listener there
"for 2.0.1 compatibility" (`wire-pod/chipper/pkg/initwirepod/startserver.go`), serving the same three
gRPC services and the same `/ok` routes. wire-pod's own onboarding writes 443 instead
(`CreateServerConfig` in `wire-pod/chipper/pkg/wirepod/setup/certs.go`).

wire-pod points a robot at itself over SSH (`wire-pod/chipper/pkg/wirepod/setup/ssh.go`). It copies
`pod-bot-install.sh` to `/data/`, `server_config.json` to `/data/data/server_config.json`, its CA
certificate to `/data/data/wirepod-cert.crt`, and then runs the script, which resets the robot to
onboarding, deletes `/data/data/com.anki.victor/persistent/token/token.jwt`, regenerates
`/data/etc/robot.pem`, clears `/data/vic-gateway/` and re-runs `vic-gateway-cert`.

### 4.2 Certificates

Two different certificates matter and they are easy to confuse.

**The certificate the robot checks**, presented by the server on the chipper, jdocs and token port.
At startup `vic-cloud` builds its root pool from the Mozilla set in `gwatts/rootcerts` and appends
`/anki/etc/wirepod-cert.crt` if it exists, copying it to `/data/data/wirepod-cert.crt` as a cache, or
else reads `/data/data/wirepod-cert.crt` directly (`wire-os-victor/cloud/cloud/main.go`; the file
name comes from a constant literally named `podCert`). There is no pinning, and TLS is set up with no
ServerName override, so ordinary hostname verification against the dialled host applies.

The jdocs client is a special case. It builds its own pool on every connection and appends a
compiled-in escape-pod root, a Digital Dream Labs self-signed CA valid from 2020 to 2220
(`wire-os-victor/cloud/internal/jdocs/escapepod_root_cert.go`,
`wire-os-victor/cloud/internal/jdocs/client.go`). That is why the escape-pod certificate works for
jdocs with no file installed on the robot, and why the jdocs connection is the one that has to be
made for that root to be in play at all. Whether the custom `wirepod-cert.crt` appended in `main`
also reaches the token connection depends on whether `rootcerts.ServerCertPool()` returns a shared
pool or a fresh one, which neither checkout answers.

The escape-pod certificate wire-pod ships, `wire-pod-rs/assets/epod/ep.crt`, is issued by Digital
Dream Labs, carries `CN=escapepod.local` and `subjectAltName DNS:escapepod.local`, and is valid until
2220. In non-escape-pod mode wire-pod generates its own pair instead, a 30-year CA and a 10-year leaf
with the machine's outbound IP as the only SAN and an empty subject
(`CreateCertCombo` in `wire-pod/chipper/pkg/wirepod/setup/certs.go`). The 1028-bit RSA key size there
is unusual but is what the Go server does.

**The certificate the robot presents**, on port 443 for the SDK surface. The key is
`/data/etc/robot.pem`, a plain 2048-bit RSA key created by `openssl genrsa`, and the certificate is
`/data/vic-gateway/gateway.cert`, produced by a separate `vic-gateway-cert` binary that
`pod-bot-install.sh` re-runs. That binary is not in either checkout, so its exact common name,
whether it carries a subjectAltName, its key usage and its validity window are not confirmable from
source here. What is confirmable is that the Python SDK validates the certificate's subject common
name against the robot's name and passes `grpc.ssl_target_name_override` set to that name, which is
what you would expect if the certificate carries a common name and no SAN. Robot names have the
form `Vector XYXY`, with the space becoming a hyphen in hostname and certificate form, so
`Vector-B6H9`. No client in this ecosystem validates the chain.

The robot never serves this certificate over the network. It uploads it to the token service as the
`session_certificate` field of `AssociatePrimaryUser`, and the client fetches its own copy out of
band; in the Anki flow that was a download from `session-certs.token.global.anki-services.com`, and
in wire-pod's flow it is the copy the server kept from that association and writes to
`~/.anki_vector/<name>-<esn>.cert`.

Separate from both is the **factory device certificate** at `/factory/cloud/`, holding
`AnkiRobotDeviceCert.pem` and `AnkiRobotDeviceKeys.pem` with a common name of `vic:<esn>` and an
organization of `Anki Inc.` (`wire-os-victor/cloud/internal/robot/cert.go`). That common name is the
robot's identity: it is what becomes the `thing` key on every jdocs call, and on a vicos build it is
also presented as a **client certificate** on the TLS connection to the token service
(`wire-os-victor/cloud/internal/token/identity/getcert_vicos.go`). A replacement server must
tolerate a client certificate being offered; it must not require or verify one, because wire-pod has
no access to the factory-signing infrastructure and therefore cannot derive the ESN from it on a
first association. That is the reason for the `vic:00601b50` placeholder in section 2.4.

### 4.3 The token exchange and where things land

The order of events on a fresh association, reading the server side in
`wire-pod/chipper/pkg/servers/token/token.go` and `.../jdocs/server.go` together with the robot side
in `wire-os-victor/cloud/internal/token/` and `wire-os-victor/cloud/cloud/tokens.go`:

1. The robot calls `Token/AssociatePrimaryUser` with its session certificate. wire-pod parses the
   issuer common name out of it as the robot's name, stashes the raw PEM keyed by source address,
   generates a client GUID and its hash, and returns a `TokenBundle`.
2. The robot stores the JWT at `/data/data/com.anki.victor/persistent/token/token.jwt`. It does not
   keep the plaintext client token; the GUID in the bundle is for the client, and the robot only
   ever sees hashes. wire-pod keeps the GUID in `botInfo.json` and writes it into `sdk_config.ini`
   for the Python SDK.
3. The robot calls `Jdocs/ReadDocs` for `vic.AppTokens`. wire-pod matches the caller's IP against
   the pending stash from step 1, writes the hash into the `vic.AppTokens` document, records the
   GUID in `botInfo.json`, writes the session certificate to `~/.anki_vector/<name>-<esn>.cert` and
   to `session-certs/<esn>`, and updates `sdk_config.ini`.
4. From then on the robot's gateway accepts `Authorization: Bearer <guid>` for that GUID, because
   its hash is in `vic.AppTokens`. The robot caches that hash list at
   `/data/vic-gateway/token-hashes.json` and refreshes it from jdocs at most once a minute, so a
   newly written hash is not necessarily live at once.
5. The robot schedules `Token/RefreshToken` for three hours before the JWT's `expires` claim.

The metadata on those calls is set out in the table in [2.4](#24-token-the-identity-service). Both
`anki-app-key` and `anki-access-token` are built by the same helper
(`wire-os-victor/cloud/internal/util/grpc.go`, `internal/token/accessor.go`), so every jdocs and
chipper call carries the app key as well as the JWT. wire-pod validates the app key nowhere.

A detail that matters for connection reuse: the robot creates a new `grpc.ClientConn` for every jdocs
request and closes it afterwards, re-fetching the token each time
(`wire-os-victor/cloud/internal/jdocs/server.go`). A server that caches per-connection state keyed by
the TCP connection will see a new one on every document operation.

### 4.4 mDNS

There is no mDNS, Avahi, Bonjour or zeroconf implementation anywhere in the robot's `/anki` source.
The robot resolves the host in `server_config.json` as an ordinary name through the Go resolver, and
`escapepod.local` works only because whatever VicOS provides resolves `.local`. The only two places
the name appears in robot code are the escape-pod `server_config.json` itself and a substring test in
`wire-os-victor/platform/switchboard/switchboardd/daemon.cpp`, which checks whether the `chipper`
value contains `escapepod.local` purely to pick which product byte to put in the BLE advertisement.
`avahi-daemon` is installed in the WireOS image and `avahi` is a distro feature, but no `nss-mdns`
module was found, so how `.local` resolution succeeds is still open.

On the server side, wire-pod registers the instance `escapepod` on the service `_app-proto._tcp` in
the `local.` domain, port 8084, with TXT records `txtv=0`, `lo=1`, `la=2`, re-registering every 30
seconds (`wire-pod/chipper/pkg/mdnshandler/mdns.go`). It also browses for `_ankivector._tcp` to
notice a Vector appearing on the network and re-broadcast immediately, and `DISABLE_MDNS=true` turns
the whole thing off. Something must answer `_ankivector._tcp` for that browse and for the Python
SDK's own discovery helper to work, but no responder for it exists in the robot's `/anki` tree. There
is an avahi service file in the `wire-os` tree declaring `_ankivector._tcp` on port 443, and no
recipe appears to install it, so whether a WireOS robot advertises itself is unconfirmed.

Note the mismatch: the advertised port is 8084 while the config wire-pod writes says 443. The mDNS
record exists to make the name resolve; the port in it is not what the robot dials.

---

## 5. CLAD, briefly

CLAD is "C-Like Abstract Data language", Anki's own interface definition language, predating their
use of protobuf. It has `structure`, `message`, `union` and `enum` declarations, fixed-width types
such as `uint_8` and `float_32`, and length-prefixed strings and arrays written as `string[uint_16]`.
The wire format is a compact tagged binary encoding and is not self-describing. The compiler lives in
`wire-os-victor/victor-clad/tools/message-buffers/` and generates C++, Go and Python. There are 180
`.clad` files in the tree, on the order of 600 message and structure definitions, spread across
`wire-os-victor/clad/src/clad/` for the engine-side interfaces, `wire-os-victor/robot/clad/` for the
engine-to-firmware interface, and `wire-os-victor/coretech/*/clad_src/` for vision and geometry
types. The Go copies that `vic-cloud` uses are checked in at
`wire-os-victor/cloud/internal/clad/`.

Transport is Unix domain sockets in `SOCK_DGRAM` mode, not shared memory, with the paths declared in
`wire-os-victor/coretech/messaging/shared/socketConstants.h` and living under `/dev/socket/` on the
robot. `vic-engine` talks to `vic-anim`, `vic-robot` and `vic-switchboard` this way, and both the
engine and the gateway talk to `vic-cloud` over `/dev/socket/jdocs_server` and
`/dev/socket/token_server`.

The relationship to the SDK protobuf surface is that the gateway is mostly not a translator. It uses
two sockets to the engine. The main one, `_engine_gateway_proto_server_`, carries protobuf
unchanged: an `ExternalInterface` call is wrapped in a `GatewayWrapper` and forwarded verbatim to
`ProtoMessageHandler::ProcessMessages`
(`wire-os-victor/engine/cozmoAPI/comms/protoMessageHandler.cpp`), which broadcasts it to whichever
component subscribed to that tag. A subset is then translated into CLAD inside the engine by
`ProtoCladInterpreter::Redirect`. The second socket, `_engine_gateway_server_`, is the residual CLAD
path for messages never ported, and for those the gateway does translate, with converters named
`ProtoAppIntentToClad`, `ProtoDefineCustomBoxToClad` and the like in
`wire-os-victor/cloud/cloud/message_handler.go`. A comment at the top of
`wire-os-victor/cloud/cloud/ipc_manager.go` says the CLAD socket is being deprecated. Jdocs is the
same story one layer down: the engine speaks CLAD `DocRequest`, and `vic-cloud` translates it to
`jdocspb` protobuf in `wire-os-victor/cloud/internal/jdocs/translate.go`.

The voice path is CLAD in the other direction: the animation process detects the wake word and sends
the cloud process a CLAD hotword message, the cloud process opens the chipper stream, and the result
goes back to the engine as a CLAD `IntentResult`.

A server author does not touch CLAD. It never crosses the network, it is not versioned as part of any
contract the server participates in, and everything a server can observe has already been translated
into protobuf or JSON by the time it leaves the robot. It is worth knowing only because it explains
why some gateway calls behave the way they do: the gateway is a thin relay with no timeout of its
own, so when the engine does not answer, the RPC does not either.

---

## 6. Things that will cost you a day

**`EnableImageStreaming` waits for the vision system, so a sleeping robot never answers.** The
gateway forwards the request and then blocks on a channel with no timeout and without consulting the
request context (`wire-os-victor/cloud/cloud/message_handler.go`). The engine does not reply
immediately: it adds the mode to `_visionModesWaitingToChange` and only broadcasts
`EnableImageStreamingResponse` once a `RobotProcessedImage` message shows `VisionMode::Viz` has
actually appeared or disappeared (`wire-os-victor/engine/components/sdkComponent.cpp`). A sleeping
robot has entered power save, which pauses the vision component and may delete the camera outright
(`wire-os-victor/engine/components/powerStateManager.cpp`), so no `RobotProcessedImage` is produced
and the confirmation never comes. `CameraFeed` calls `EnableImageStreaming` first and returns its
error, so the camera stream never sends headers either and the request hangs until the client gives
up. The deferred disable at the end of `CameraFeed` and `CaptureSingleImage` waits on the same
confirmation, so teardown can block too. wire-pod papers over this with a five-second client-side
deadline (`wire-pod/chipper/pkg/wirepod/sdkapp/server.go`); the robot itself imposes none.

The same confirm-before-reply rule applies to `EnableMarkerDetection`, `EnableFaceDetection`,
`EnableMotionDetection` and `EnableMirrorMode`. The engine notes a one-frame lag between the mode
changing and the response, so a client that waits for the response can still miss the first frame's
detections. One of the five has a fix: `EnableMarkerDetection` with `enable = false` returns
`REQUEST_PROCESSING` early rather than waiting, with a comment referencing VIC-12762 and a stall of
roughly fifty seconds, plus a TODO asking whether the other modes have the same problem. They do,
and they did not get the fix.

**The fix on the client side is to take behaviour control first.** Requesting control is the only SDK
message that registers as a wake reason, so `BehaviorControl` with `OVERRIDE_BEHAVIORS` wakes a
sleeping robot, and the camera calls then answer. This is worth building into the port's camera
route rather than relying on a five-second deadline.

**Every action RPC can hang the same way, and without behaviour control they always do.** The
blocking relay is not special to the camera. Action responses are produced only while the SDK
behaviour is activated, so `DriveStraight`, `GoToPose`, `PickupObject` and their siblings queue the
motion and then never return if you have not taken control. Set a deadline on every call.

**Refusals are usually invisible.** The gateway overwrites the engine's `ResponseStatus` with
`RESPONSE_RECEIVED` on the vision toggles and `SetCameraSettings`, so a `FORBIDDEN` from the engine
reaches the client as success. `SetEyeColor` has no response path at all and always reports success.
`DriveWheels`, `MoveHead`, `MoveLift` and `StopAllMotors` return success and do nothing when external
movement commands are not allowed. Do not infer that a call worked from its status code; check the
robot.

**A `ControlRequest` with no priority set is dropped silently.** Priority zero is `UNKNOWN`, and the
engine returns early on it, so the stream simply never grants control.

**`AssumeBehaviorControl` does not release control when the stream closes.** Only `BehaviorControl`
has the releasing defer. A client that uses the simpler call and then exits leaves the SDK behaviour
running on the robot.

**Engine responses are matched by message type, not by request.** Two concurrent clients making the
same call against one robot can get each other's answers. Serialise per robot.

**`AudioFeed` returns a 1 kHz sine wave, not microphone audio.** The engine's generator is a
placeholder with a TODO next to it. The transport is entirely correct, so the stream opens, frames
arrive, and the data is meaningless. Nothing reports an error.

**`id_tag` must be in a narrow range.** Actions with a zero or arbitrary `id_tag` are rejected with
`InvalidArgument`. The valid range is 2000001 to 3000000.

**The connection check passes on a 404.** The robot checks only that the HTTP request completed, not
what it returned. A misconfigured `check` route can look healthy while serving nothing useful.

**The `check` path can contain a literal colon.** `/ok:80` is a real path, not a host-port suffix.
Test your router against it explicitly.

**`ConnectionCheckResponse.status` must be the exact string `Success`.** Anything else is reported to
the engine as a bandwidth problem.

**The JWT is never verified but is strictly validated.** Six claims, all as JSON strings, with `iat`
and `expires` as RFC 3339 timestamps in UTC. Getting the signature right buys nothing; getting a
claim type wrong breaks the robot.

**Refresh is scheduled three hours before expiry.** A token with a short life makes the robot loop
every five minutes.

**A JWT whose `iat` is in the robot's future stalls everything.** The refresher sleeps in
twenty-second increments while `time.Now()` is before `iat`, waiting for NTP. If your server's clock
runs ahead of the robot's, nothing happens and nothing is logged as an error.

**A `PermissionDenied` answer can lock a robot out until it restarts.** The first one starts a
forced-refresh backoff; a second one during that backoff sets a sticky denied flag. Return something
else while your server is still warming up.

**With no parseable JWT the robot sends no jdocs traffic at all.** The engine substitutes the account
string `NotLoggedIn` and then silently drops every request, so the symptom of a broken token service
is silence on the jdocs service rather than errors on it.

**The `session_certificate` in `AssociatePrimaryUser` is the robot's own gateway certificate**, not a
session artefact the server issued. It is also the only field the robot fills on that request; the
session token arrives as the `anki-user-session` metadata key.

**Each jdocs request opens a new gRPC connection.** Do not key server state on the transport
connection.

**The escape-pod CA enters the robot's trust pool only when a jdocs connection is created**, not at
boot. This is why wire-pod pings the robot with `PullJdocs` when the heartbeat resumes. A port that
drops that ping will see robots that were fine yesterday fail TLS today.

**`vic.AppTokens` hashing is not bcrypt.** It is `base64(sha256(token || salt) || salt)` with 16-byte
token and salt. Existing jdocs on disk hold hashes produced by the Go server, so the scheme is fixed.

**Settings sent as binary protobuf land on the wrong fields.** See
[3.6](#36-the-vendored-sdk-proto-does-not-match-the-robots). Use the JSON REST path for settings.

**The robot's REST surface ignores unknown JSON keys.** `DiscardUnknown` is on, so a typo in a
settings key is silently a no-op rather than an error. A test that asserts a 400 on a bad field will
not hold.

**wire-pod's `ReadDocs` handler indexes `req.Items[0]` without checking.** A read request with an
empty item list takes the server down. The robot guards the equivalent on its side; the server does
not.

**`Basic` authorization on the robot's gateway is broken, not merely unsupported.** The code accepts
the prefix, decodes it, drops the result on the floor, and then slices the header at a fixed offset
that is correct only for `Bearer `. Use `Bearer`.

---

## 7. What this document does not confirm

`chipper.IsIntent`, the helper the robot uses to decide which half of an `IntentGraphResponse` to
read, lives in `digital-dream-labs/api-clients`, which is not vendored in either checkout here and is
not in the local Go module cache. That `response_type` is the deciding field is a reasonable reading
of the proto and of what wire-pod sets, but I did not see the predicate.

The same module builds the chipper request messages, so whether the `app_key` proto field on the
voice requests is populated at all is unconfirmed. The robot passes an empty string as the second
argument to `chipper.NewConn`, and wire-pod never reads the field.

Whether a queued action actually completes its motion when the SDK behaviour is not active was not
traced, because it depends on which animation tracks the running behaviour holds. What is certain is
that the action is queued and that no completion response is sent.

The grpc-gateway JSON path's handling of the `Authorization` header is taken from an in-code comment
saying that the check runs when the JSON handler forwards to the local gRPC endpoint. The generated
`external_interface.pb.gw.go` is not vendored in either checkout, so that path was not read.

How `escapepod.local` resolves on a WireOS robot, and whether the robot advertises itself over mDNS
at all, are both open questions carried over from `wireos-study.md`. What this document adds is that
the answer is definitely not in the robot's `/anki` source, which contains no mDNS implementation of
any kind, and that nothing in that source answers `_ankivector._tcp` either.

The `vic-gateway-cert` binary that produces the robot's own TLS certificate is not in either
checkout, so the certificate's common name, its subjectAltName if any, its key usage and its validity
window are inferred from how the Python SDK validates it rather than read from the generator.

Whether the custom `wirepod-cert.crt` that `vic-cloud` appends at startup reaches the token
connection depends on whether `gwatts/rootcerts` returns a shared pool or a fresh one. That module is
not vendored and is not in the local Go module cache.
