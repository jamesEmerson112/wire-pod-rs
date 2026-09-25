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
6. [Mapping, localization and the planner](#6-mapping-localization-and-the-planner)
7. [Things that will cost you a day](#7-things-that-will-cost-you-a-day)
8. [What this document does not confirm](#8-what-this-document-does-not-confirm)

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
| `OVERRIDE_BEHAVIORS` | 10 | Activates the `SDKOverrideAll` behaviour, which sits above shut-up mode, quiet mode and the sleep cycle, and sets `disableCliffDetection`, which also keeps cliffs out of the map (see [6.10](#610-behaviour-control-keeps-cliffs-out-of-the-map)). What wire-pod uses. |
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
  in section 7: take behaviour control first, then ask for the camera.

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
| `GoToPose` | `GoToPoseRequest` | Drives to an x, y, angle pose in the current origin using the planner. See [6.8](#68-the-planner). |
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
| `NavMapFeed` (stream) | `NavMapFeedRequest` | Streams the navigation memory map. `frequency` is a period in seconds, not a rate. See [6.9](#69-the-navmapfeed-wire-format). |

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

The connection-id rules are worth reading exactly, because wire-pod opens two streams. Each
`EventStream` call gets its own channel of engine events, 512 slots deep, filtered by that call's own
lists, and its own one-second keep-alive ticker
(`wire-os-victor/cloud/cloud/message_handler.go:1079-1168`). `checkConnectionID` decides primacy
(`message_handler.go:1031-1062`). If no id is recorded yet, or the incoming id equals the recorded
one, the stream is primary and its id becomes the recorded id. Otherwise the stream is secondary, and
it still receives every event. An empty id is recorded as the empty string, which the check treats
as nothing recorded, so a stream that sends no id never stops a later stream from becoming primary,
and two primary streams can be open at once. When a primary stream ends, `onDisconnect` calls
`SendAppDisconnected()`, which tells the engine the app has left, and clears the recorded id
(`message_handler.go:1022-1029`, `:409-417`). In the engine, the onboarding coordinator and the Alexa
component listen for that message
(`wire-os-victor/engine/aiComponent/behaviorComponent/behaviors/onboarding/behaviorOnboardingCoordinator.cpp`,
`wire-os-victor/engine/aiComponent/alexaComponent.cpp`). One more gateway rule applies to every
stream: if a listener's channel stays full for 250 ms, the IPC manager logs an error and removes that
listener from the fan-out (`wire-os-victor/cloud/cloud/ipc_manager.go:234-250`).

wire-pod opens the first stream when it connects to a robot. It is whitelisted to `stimulation_info`,
carries no connection id, and is stored in `EventStreamClient`, which nothing ever reads
(`wire-pod/chipper/pkg/wirepod/sdkapp/robot.go:371-386`, field at `:317`). It opens a second stream
for each Stim session in the web UI, with the same whitelist and the connection id `wirepod`
(`wire-pod/chipper/pkg/wirepod/sdkapp/server.go:482-493`). By the rules above both streams are
primary, unless a BLE client holds a different id. Ending a Stim session therefore sends the engine
`AppDisconnected` and clears the recorded id while the first stream is still open. What the unread
first stream does to the gateway over time is in [section 8](#8-what-this-document-does-not-confirm).

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

## 6. Mapping, localization and the planner

Two SDK calls reach into the robot's model of the space around him. `NavMapFeed` streams his
navigation map, and `GoToPose` hands a target to the path planner, which plans across that map. This
section describes what sits behind both, read from the engine (`vic-engine`) and the robot-process
firmware. Line numbers refer to the checkout at `E:/GitHub/wire-os-victor` as it stood on
2026-09-25. Nothing here concerns the server. It matters to anything that reads the map, sends the
robot somewhere, or holds behaviour control while he drives.

### 6.1 Axes and units

Distances are millimetres and angles are radians. In the robot's own frame +x points forward, +y
points to his left and +z points up, so a positive rotation about z turns him left. Three places in
the source agree on this. The front-left cliff sensor sits at +y and the front-right one at -y
(`wire-os-victor/robot/include/anki/cozmo/shared/cozmoConfig.h:219-221`). A cliff seen by the
front-left sensor alone is given a heading of +45 degrees, and one seen by the front-right sensor
alone a heading of -45 degrees
(`wire-os-victor/engine/components/sensors/cliffSensorComponent.cpp:351-357`). When the left tread
drives forward and the right one does not, the robot is turning right, and the slip handler puts its
obstacle at -y with the log text "to right of"
(`wire-os-victor/engine/components/movementComponent.cpp:395-410`).

To draw the map with the robot's nose pointing up the screen, put +x up and +y to the left. The
engine's own comments label the +x+y child "up L"
(`wire-os-victor/engine/navMap/quadTree/quadTreeNode.cpp:74-77`). The Python SDK's docstring draws
the same frame turned a quarter turn, with +x to the right and +y up
(`wire-os-victor/tools/sdk/vector-python-sdk-private/sdk/anki_vector/nav_map.py:94-107`).

### 6.2 The map is a quadtree, not an occupancy grid

An occupancy grid, the usual textbook map, is a fixed array of cells that each hold a probability of
being occupied. Vector's map is neither fixed nor probabilistic. `MemoryMap`
(`wire-os-victor/engine/navMap/memoryMap/memoryMap.h:25`) wraps a `QuadTree`, a tree in which every
node is a square that is either a leaf or split into four half-size children. The tree sits behind a
`std::shared_timed_mutex` whose comment reads "safe thread access for planner" (`memoryMap.h:93-94`),
because the planner reads the map from its own thread while the engine tick writes it. Reads take the
lock shared and writes take it exclusively
(`wire-os-victor/engine/navMap/memoryMap/memoryMap.cpp:169-300`). The Python SDK's module docstring
draws the same line: the map "doesn't deal with probabilities of occupancy, but instead encodes what
type of content is there" (`nav_map.py:22-23`).

Three constants set the geometry (`wire-os-victor/engine/navMap/quadTree/quadTree.cpp:32-34`). The
root starts 128 mm on a side with a height of 4, and its height may grow to 8. Height here means the
number of levels still available below a node. The smallest quad the tree ever makes is therefore
128 / 2^4 = 8 mm on a side, which `GetContentPrecisionMM` computes (`quadTree.cpp:54-59`) and which
the planner config repeats in a comment, "minimum step size of 8mm == navMap resolution"
(`wire-os-victor/engine/xyPlannerConfig.h:46`).

When something is inserted outside the root, `ExpandToFit` grows the tree (`quadTree.cpp:202-242`).
Each call to `UpgradeRootLevel` moves the centre toward the new data, doubles the side and adds one
level of height, so the leaf size stays at 8 mm (`quadTree.cpp:357-361`). It refuses once the height
reaches 8 (`quadTree.cpp:334`), which caps the root at 128 * 2^4 = 2048 mm. After that, `ShiftRoot`
moves the root by half its side toward the new data, along each axis the data lies outside, and
keeps only the half of the tree on that side, so the trailing half of the map is dropped
(`quadTree.cpp:220-228`, `:245-328`). The map is a sliding window about two metres square, and
`wire-os-victor/docs/architecture/map.md:21` says as much: drive far enough in one direction and the
robot forgets what is behind him.

A leaf does not hold a cell value. It holds a `MemoryMapDataPtr`
(`wire-os-victor/engine/navMap/quadTree/quadTreeTypes.h:133-134`), which wraps a `std::shared_ptr`
(`wire-os-victor/engine/navMap/memoryMap/data/memoryMapDataWrapper.h:26-49`). One insertion hands the
same pointer to every leaf it covers, so those leaves share one data object and whatever identity it
carries, such as a cube's object id. After an insertion the tree calls `TryAutoMerge` on the nodes it
touched, and when four sibling leaves compare equal the parent takes their content and the children
are deleted (`quadTreeNode.cpp:89-119`, called from `quadTree.cpp:90-94`). Large uniform areas cost
one node, and fine detail exists only near boundaries.

### 6.3 What a quad can hold

The engine has nine content types (`wire-os-victor/engine/navMap/memoryMap/memoryMapTypes.h:36-48`).
The SDK's `NavNodeContentType` has ten (`wire-pod-rs/crates/wirepod-proto/proto/vector/nav_map.proto`),
because it splits the proximity obstacle by whether the robot has been to look at it. Each data class
chooses its SDK type when the map is broadcast (`memoryMapData.cpp:85-109`,
`memoryMapData_ProxObstacle.cpp:50-53`, `memoryMapData_ObservableObject.cpp:52`,
`memoryMapData_Cliff.cpp:61`, all under `wire-os-victor/engine/navMap/memoryMap/data/`).

| Engine `EContentType` | SDK `NavNodeContentType` | Blocks the planner? |
|---|---|---|
| `Unknown` | `NAV_NODE_UNKNOWN` (0) | No. |
| `ClearOfObstacle` | `NAV_NODE_CLEAR_OF_OBSTACLE` (1) | No. |
| `ClearOfCliff` | `NAV_NODE_CLEAR_OF_CLIFF` (2) | No. |
| `ObstacleObservable` | `NAV_NODE_OBSTACLE_CUBE` (3), which also covers the charger and custom objects | Only while the object's pose is verified. |
| `ObstacleProx` | `NAV_NODE_OBSTACLE_PROXIMITY` (4), or `NAV_NODE_OBSTACLE_PROXIMITY_EXPLORED` (5) once explored | Only once confirmed; see 6.4. |
| `ObstacleUnrecognized` | `NAV_NODE_OBSTACLE_UNRECOGNIZED` (6) | Yes. |
| `Cliff` | `NAV_NODE_CLIFF` (7) | Yes. |
| `InterestingEdge` | `NAV_NODE_INTERESTING_EDGE` (8) | No. Never written; see 6.4. |
| `NotInterestingEdge` | `NAV_NODE_NON_INTERESTING_EDGE` (9) | No. Never written. |

The planner asks each quad's data whether it is a collision type. The base class answers yes only
for `ObstacleUnrecognized` and `Cliff` (`memoryMapData.h:47`). A proximity obstacle answers yes only
once its belief is confirmed (`memoryMapData_ProxObstacle.h:41`, `:64`). A recognised object answers
yes only while its pose is verified, and BlockWorld clears that flag when the object should have been
seen and was not (`memoryMapData_ObservableObject.h:37-40`,
`wire-os-victor/engine/blockWorld/blockWorld.cpp:1220`). `MapComponent::SetUseProxObstaclesInPlanning`
sets a `_collidable` flag on every proximity obstacle
(`wire-os-victor/engine/navMap/mapComponent.cpp:1332-1349`), but nothing reads that flag
(`memoryMapData_ProxObstacle.h:44`, `:82`), so it has no effect.

A new insertion does not always win. `CanOverrideSelfWithContent` decides, leaf by leaf, whether the
incoming content may replace what is there (`memoryMapData.cpp:19-82`):

| Incoming content | What it cannot replace |
|---|---|
| `Cliff` | An existing `Cliff`. Two cliffs are merged by a special transform instead. |
| `ClearOfCliff` | Nothing. It replaces every type, cliffs and recognised objects included. |
| `ClearOfObstacle` | `Cliff`, `ClearOfCliff`, `ObstacleUnrecognized` and `ObstacleObservable`. It does replace a proximity obstacle, whatever that obstacle's belief. |
| `ObstacleProx` | `Cliff`, `ObstacleObservable`, and a proximity obstacle already marked explored. |
| `NotInterestingEdge` | Anything except `InterestingEdge`. |
| `Unknown`, `ObstacleObservable`, `ObstacleUnrecognized` | An existing `Cliff`. |

These rules govern insertions. The timeout sweep in 6.5 and a few explicit transforms rewrite leaves
directly and ignore them.

### 6.4 What writes the map

Every writer goes through `MapComponent` and writes into the map of the current origin. Six sources
write in production, and two content types have no writer at all.

**The time-of-flight proximity sensor.** On every robot state message, `ProxSensorComponent` looks up
the robot's pose at the sensor reading's own timestamp from the pose history, rather than using the
current pose, so a reading taken mid-turn lands where the beam was pointing
(`wire-os-victor/engine/components/sensors/proxSensorComponent.cpp:150-170`). The distance is
clamped to between 30 and 400 mm (`:92-93`, `:173`). The reading counts as an object only when the
lift is out of the beam, the signal quality is above 0.01, the raw distance is under 400 mm, the
sensor reports a valid range, and the robot's pitch at that moment is within 5 degrees of level
either way (`:57-58`, `:177-182`). The pitch test keeps a tilted robot from mapping the floor or the
ceiling. While the robot stays still, only 32 readings at one pose reach the map (`:64`,
`:189-207`).

`UpdateNavMap` then does two things (`:374-392`). It clears a region from the robot to 6 mm short of
the reading, shaped as the sensor cone intersected with a strip no wider than the obstacle, and it
stamps an obstacle 12 mm deep at the reading (`:318-371`). The cone's width is the distance times the
aperture constant 0.4 (`:99`), and the obstacle's width is the same figure clamped to 18 mm (`:102`).
That describes the case where the sensor found something. When nothing is in range, the code as
written builds the clearing region around a default pose at the frame's origin instead of the robot's
reading (`:184-187`, `:378-383`); section 8 has the details.

**Obstacle belief.** A proximity obstacle carries a small counter, `_belief`, not a probability
(`wire-os-victor/engine/navMap/memoryMap/data/memoryMapData_ProxObstacle.h:57-65`). A hit adds 4,
capped at 100, and a clearing pass subtracts 6, floored at 0. Above 40 the obstacle is confirmed and
blocks the planner, and only exactly 0 counts as clear. A new obstacle is created at 40
(`memoryMapData_ProxObstacle.cpp:24`), one step short of confirmed. When a later stamp lands on a quad
that already holds a proximity obstacle, `AddProxData` calls `MarkObserved` on the existing object
instead of replacing it (`mapComponent.cpp:1285-1311`), so the second reading that overlaps a fresh
obstacle confirms it. When a clearing region passes over one, `ClearRegion` calls `MarkClear` and
turns the quad into `ClearOfObstacle` only once the counter reaches 0 (`mapComponent.cpp:1263-1282`).
A fresh obstacle at 40 therefore needs seven clearing steps to disappear, and one saturated at 100
needs seventeen. A step is applied once for every leaf holding that obstacle that the insertion
visits, and the leaves one stamp covered share a single data object (6.2), so one reading can move
the counter by several steps.

**The cliff sensors.** A cliff enters the map only through a `CliffEvent` from the robot process
(`wire-os-victor/engine/robotToEngineImplMessaging.cpp:421-445`), and the robot process sends one with
cliff flags set only when it stops for the cliff
(`wire-os-victor/robot/supervisor/src/proxSensors.cpp:221-259`). It also sends one with no flags when
the cliff clears (`proxSensors.cpp:297-300`), which the engine handles without touching the map. 6.10
explains why the first rule matters. The engine computes the cliff's pose from the robot's pose at the event's
timestamp and from which sensors fired (`cliffSensorComponent.cpp:335-427`). A combination it does not
recognise, such as three sensors at once, is not inserted (`:379-383`). `UpdateNavMapWithCliffAt`
stamps a bar 10 mm deep and `ROBOT_BOUNDING_Y` wide, which is 60 mm (`cliffSensorComponent.cpp:429-442`,
`wire-os-victor/robot/include/anki/cozmo/shared/cozmoEngineConfig.h:44`). Once in the map, a cliff
can be replaced by an insertion only if the insertion is `ClearOfCliff` (`memoryMapData.cpp:30-35`).

**Driving.** Each tick, `MapComponent::UpdateRobotPose` checks whether the robot has moved about 8 mm
or turned 20 degrees since it last reported (`mapComponent.cpp:73-75`, `:457-461`). If so, it marks the
rectangle spanned by the four cliff sensors as `ClearOfCliff` and the robot's bounding footprint as
`ClearOfObstacle` (`:469-483`). Driving over a proximity obstacle therefore erases it at once,
whatever its belief. Driving is the ordinary source of `ClearOfCliff`, but not the only one:
`BehaviorGoHome` marks the charger's docking area `ClearOfCliff` before driving onto it
(`wire-os-victor/engine/aiComponent/behaviorComponent/behaviors/basicWorldInteractions/behaviorGoHome.cpp:414-420`).
The same behaviour deletes every proximity obstacle when it starts, with a comment blaming stale ones
for failed plans home (`behaviorGoHome.cpp:238-242`), and clears a disc between the robot and the
charger (`behaviorGoHome.cpp:735-752`).

**Wheel slip.** When the movement component reports an unexpected movement of the kind
`TURNED_BUT_STOPPED` or `TURNED_IN_OPPOSITE_DIRECTION`, it assumes the robot has run into something it
cannot see (`movementComponent.cpp:350-354`). It puts the robot's position back where the slip began,
keeps the gyro heading, and inserts a 30 by 10 mm `ObstacleUnrecognized` in front, behind, or on the
side the treads were turning toward (`:385-466`). The console variable that enables this defaults to
true (`:50`).

**Vision.** When BlockWorld reports a recognised object's pose, `AddObservableObject` inserts it
(`mapComponent.cpp:996-1072`). A cube or custom object goes in as its bounding polygon on the floor.
The charger goes in as a shaped charger region, joined with the habitat when the robot believes he is
in one (`:1032-1048`). An object resting too high above the floor, such as a cube stacked on another,
is remembered but not inserted (`:1016-1027`). `ClearRobotToMarkers` would clear the space between the
robot and a marker he has seen (`:1205-1240`), but nothing in the tree calls it.

The camera's ground-edge detection can extend a cliff but not create one. `AddVisionOverheadEdges`
uses the edge points only if at least one drop-sensor cliff is already in the map and at least 20
points survive a check against known obstacles (`mapComponent.cpp:1459-1551`, threshold at `:88`). It
then fits a line with a Hough transform, a voting method for finding straight lines among points,
and if the fit succeeds it rewrites the recorded pose of the newest drop-sensor cliff to lie on that
line and stamps a strip 400 mm long and 20 mm deep along it as a cliff seen by vision (`:1567-1604`,
sizes at `:91-92`).

**The edge types are dead.** `InterestingEdge` is never written in production. A search of the whole
tree finds exactly one construction of `MemoryMapData` with that type, in a unit test
(`wire-os-victor/test/engine/testNavMap.cpp:65`); everywhere in the engine it is only compared
against or erased. `NotInterestingEdge` is written only by `FlagQuadAsNotInterestingEdges`
(`mapComponent.cpp:556-559`), which has no callers, and the override rules would let it replace only
an `InterestingEdge` anyway. Neither SDK edge type can appear in a map from a stock robot.

### 6.5 Forgetting

`MapComponent::TimeoutObjects` sweeps the whole map every five seconds (`mapComponent.cpp:492-529`,
period at `:80`). A quad whose type has a timeout and whose last observation is older than that is
reset to a fresh `Unknown`, never to clear. The empty `MemoryMapDataPtr()` the sweep returns
constructs a new `Unknown` data object (`memoryMapDataWrapper.h:37`, `memoryMapData.h:36`).

| Content | Reset to `Unknown` after | Constant |
|---|---|---|
| `ObstacleUnrecognized` | 20 s | `kUnrecognizedTimeout_ms` |
| `InterestingEdge`, `NotInterestingEdge` | 120 s | `kVisionTimeout_ms` |
| `ObstacleProx` | 600 s | `kProxTimeout_ms` |
| `Cliff` | 1200 s | `kCliffTimeout_ms` |

The constants are at `mapComponent.cpp:77-81` and the comparison at `:516-520`. Despite its name,
`kVisionTimeout_ms` applies only to the two edge types. `ClearOfObstacle` and `ClearOfCliff` never
expire, and neither does `ObstacleObservable`. A recognised object leaves the map only when BlockWorld
moves or removes it (`mapComponent.cpp:917-993`, `:1075-1119`). A cliff can therefore disappear in two
ways: by a `ClearOfCliff` insertion, or by the twenty-minute timeout.

### 6.6 Poses, origins and delocalization

There is no global coordinate frame. Every pose is relative to a numbered origin, and the
`PoseOriginList` hands out origin ids and tracks the current one
(`wire-os-victor/coretech/common/engine/math/poseOriginList.h:29-79`). The SDK exposes the id as
`PoseStruct.origin_id`, field 8, whose comment says 0 means none or unknown
(`wire-pod-rs/crates/wirepod-proto/proto/vector/messages.proto`), and `NavMapFeedResponse.origin_id`
names the origin a map is in. Poses with different origin ids cannot be compared.

To delocalize is to give up the current coordinate frame. `Robot::Delocalize`
(`wire-os-victor/engine/robot.cpp:651-778`) allocates a new origin (`:673-684`), places the robot at
exactly zero position and zero heading in it (`:691-697`), and aborts any path in progress
(`:754-755`). After every delocalization, the frame's +x is the direction the robot was facing at that
moment.

Four things trigger it. The main one is the treads state: `robot.cpp:987` sets `isDelocalizing`
whenever the off-treads state changes and either the old or the new state is `OnTreads`, and
`robot.cpp:1030-1035` then delocalizes. Picking the robot up is one such change and putting him down
is another, so one pick-up burns two origins. The second trigger is a watchdog: if the engine and the
robot process disagree about the pose frame for more than 100 consecutive state messages, which the
comment puts at three seconds, the engine delocalizes to force a resync (`robot.cpp:1113-1138`). The
engine also delocalizes once at startup (`robot.cpp:376-377`) and on a `ForceDelocalizeRobot`
message (`wire-os-victor/engine/robotEventHandler.cpp:1594-1595`).

Delocalizing deletes the map. `Delocalize` calls `BlockWorld::OnRobotDelocalized`, which forgets
every located object and asks the map component for a map in the new origin
(`blockWorld.cpp:1632-1645`). `CreateLocalizedMemoryMap` treats every existing map as a zombie when
`kMergeOldMaps` is false, which is how it ships (`mapComponent.cpp:70`), erases each one, and then
creates the new empty map (`mapComponent.cpp:667-718`). One pick-up leaves the robot with an empty map
in a fresh frame.

Relocalizing is narrower than the architecture notes suggest. The notes describe a "rejigger": when
the robot re-sees an object from an older frame, the older frame is re-parented under the new one and
the maps are merged (`wire-os-victor/docs/architecture/blockWorld.md:68`; the code is at
`robot.cpp:1696-1731` and `mapComponent.cpp:399-444`). On this firmware that path is not reached from
BlockWorld. The only object the robot localizes to is the charger, and BlockWorld looks for an
existing charger only in the robot's current frame (`blockWorld.cpp:871-891`), under a comment that
reads "VIC-14462: we no longer relocalize to objects in other origins due to rejiggering bugs, and the
map timing out anyway" (`blockWorld.cpp:896`). Delocalizing has also cleared every located object,
so nothing from an older frame survives to be merged across a pick-up. Localization, meaning the
correction of the robot's pose from a landmark, happens only when he sees the charger again in the
same frame and close to where he last saw it; his pose is then corrected to agree with it
(`blockWorld.cpp:897-923`). The architecture notes explain why the charger is the only landmark: the
cube is rarely connected, while the charger has a bigger marker and usually stays put
(`blockWorld.md:74-76`).

`RobotState.localized_to_object_id` reports that landmark. It holds the charger's object id after such
a correction and -1 when the robot is localized to nothing (`robot.cpp:2347`; the unset `ObjectID` is
-1 in `wire-os-victor/coretech/common/engine/objectIDs.h:86`, and the CLAD field carries the comment
"Will be -1 if not localized to any object" in
`wire-os-victor/clad/src/clad/externalInterface/messageEngineToGame.clad:126`). At -1 the pose comes
from odometry alone, and the engine's own debug label for that state is "LocalizedTo: Odometry"
(`robot.cpp:780-788`).

The map does not survive a reboot. Nothing under `wire-os-victor/engine/navMap/` reads or writes a
file; a search there for serialization and file streams finds nothing.

### 6.7 Odometry, and the absence of SLAM

Odometry is the robot's estimate of his own motion from his own sensors, and it runs in the robot
process rather than the engine. `Localization::Update` reads the two wheel encoders and takes the
distance each tread has travelled (`wire-os-victor/robot/supervisor/src/localization.cpp:433-441`),
integrates that into x and y, and then overwrites the heading with the gyro's:
`orientation_ = IMUFilter::GetRotation() + gyroRotOffset_` (`localization.cpp:639-640`). Heading comes
from the gyro and distance from the encoders. While the robot is on the charger, wheel motion is
ignored, so a robot slipping against the charger does not drift (`localization.cpp:445-450`).

SLAM, simultaneous localization and mapping, names the family of methods that correct the pose
against the map while building it, for example by recognising a place seen before (loop closure) or
by tracking camera features between frames (visual odometry). None of that exists here. A
case-insensitive search of `wire-os-victor/engine/` and `wire-os-victor/coretech/vision/` for "slam",
"loop closure" and "visual odometry" finds three hits, all in engine comments about the lift
slamming (`heldInPalmTracker.cpp:287`, `behaviorSleepCycle.cpp:1232`, `behaviorReactToSound.cpp:59`),
and nothing at all in `coretech/vision/`. The pose is dead reckoning corrected only by charger
sightings, and the map is drawn wherever that pose says the robot is.

### 6.8 The planner

`GoToPose` becomes a `DriveToPoseAction`. The handler builds the target from `x_mm`, `y_mm` and `rad`
with z fixed at 0, parented to whatever origin is current when the request arrives
(`wire-os-victor/engine/robotEventHandler.cpp:194-212`, next to a TODO asking for a better way to
specify the target's parent). If the robot delocalizes during the action, the path is aborted (6.6).

The path component picks a planner by the straight-line distance to the target
(`wire-os-victor/engine/components/pathComponent.cpp:712-771`). Under 40 mm (`pathComponent.cpp:41`)
it uses one of two small planners, `FaceAndApproachPlanner` or `MinimalAnglePlanner`
(`pathComponent.cpp:99-100`). Neither checks for collisions while planning; the base class default
for `ChecksForCollisions()` is false (`wire-os-victor/engine/pathPlanner.h:99-100`). Their output is
checked afterwards, though. The path component runs it through the long planner's `CheckIsPathSafe`
and replans with the long planner if it collides (`pathComponent.cpp:640-665`). From 40 mm up, the
long planner is used directly.

The long planner is `XYPlanner` (`wire-os-victor/engine/xyPlanner.h:41-61`), configured in
`wire-os-victor/engine/xyPlannerConfig.h`. It is a bidirectional A*, a best-first graph search that
grows one search from the start and one from the goal and stops where they meet (`:127`). The grid
is 4-connected, so the only moves are +x, -x, +y and -y (`:54-59`), and the heuristic is Manhattan
distance (`:87-90`, `:179-186`). The step is 32 mm (`:45`). When any of the four full steps from a
point collides, the search also offers the four half steps from that point, so the step shrinks near
obstacles (`:107-116`, `:142-159`). The halving is limited by `kMaxSubsampleDepth = 2`, whose comment
reads "minimum step size of 8mm == navMap resolution" (`:46`); section 8 notes that the code as
written allows one halving more than the comment says. The search gives up after 100000 expansions
(`:51`, `:175`). A separate escape search, for a start point that is already inside an obstacle, is
capped at 10000 (`:50`; see `wire-os-victor/docs/architecture/planner.md:58`).
Each candidate point is tested against the map as a disc of radius 33 mm, half of `ROBOT_BOUNDING_Y`
plus 3 mm of padding (`xyPlannerConfig.h:47-48`, `:155`), through `MapComponent::CheckForCollisions`,
which asks whether any quad inside the disc is a collision type (`mapComponent.cpp:1368-1375`).

The planner runs on its own thread, started in its constructor, and holds the map component by const
reference, with a `static_assert` that fails the build if the reference ever loses its `const`
(`wire-os-victor/engine/xyPlanner.cpp:44-58`). The plan is purely positional. `XYPlanner` snaps start
and goal to the 32 mm grid, searches, puts the true end points back, and smooths the corners into arcs
(`xyPlanner.cpp:156-236`, arc radii at `:33`). It never considers heading, so the robot point-turns at
the start and at the end of every path (`planner.md:36`).

`DriveToPoseAction` reports `PATH_PLANNING_FAILED_ABORT` in four places: when starting the plan fails
(`wire-os-victor/engine/actions/driveToActions.cpp:682-687`), when the path component reports
`Failed` (`driveToActions.cpp:704-708`), when planning runs past the action's timeout
(`driveToActions.cpp:829-843`), and when a precomputed plan cannot be started
(`driveToActions.cpp:864-869`). The timeout is `DEFAULT_MAX_PLANNER_COMPUTATION_TIME_S`, 6 s
(`wire-os-victor/engine/actions/driveToActions.h:87`, `cozmoEngineConfig.h:96`).
`PATH_PLANNING_FAILED_RETRY` is never produced. A search of the whole tree finds it only in its CLAD
definition (`wire-os-victor/clad/src/clad/types/actionResults.clad:77`), in the protos, and in two
comparisons (`driveToActions.cpp:808`, `behaviorGoHome.cpp:320`). A client that retries only on
`RETRY` never retries.

When the search simply finds no path, the action does not see a planning failure. `XYPlanner` sets
`CompleteNoPlan` (`xyPlanner.cpp:233-236`), the path component treats that as a finished path and
returns to `Ready` (`pathComponent.cpp:564-583`, `:326-333`), and the action then finds the robot
away from the goal. Which result it reports then is in section 8.

### 6.9 The `NavMapFeed` wire format

`MemoryMap::GetBroadcastInfo` is the serializer (`memoryMap.cpp:248-287`). It folds over the tree
and emits one record of content, depth and colour per leaf; internal nodes emit nothing
(`memoryMap.cpp:267-282`). The root contributes the header: its height as `root_depth`, its side as
`root_size_mm`, and its centre (`memoryMap.cpp:254-265`).

The fold is `QuadTreeNode::Fold` with its default direction, which is named
`FoldDirection::BreadthFirst` (`quadTreeNode.h:48`). Despite the name it is a pre-order depth-first
walk: the accumulator runs on a node first, and the fold then recurses into each child in turn
(`quadTreeNode.cpp:322-342`). `DepthFirst` in this code means post-order. Children are visited in the
order `Subdivide` creates them (`quadTreeNode.cpp:73-77`), which is the order of the `EQuadrant`
values (`quadTreeTypes.h:121-128`):

| Index | `EQuadrant` | Offset from the parent's centre | In a nose-up drawing |
|---|---|---|---|
| 0 | `PlusXPlusY` | +x, +y | upper left |
| 1 | `PlusXMinusY` | +x, -y | upper right |
| 2 | `MinusXPlusY` | -x, +y | lower left |
| 3 | `MinusXMinusY` | -x, -y | lower right |

`depth` is the remaining height, not the depth from the root. The serializer writes `GetMaxHeight()`
(`memoryMap.cpp:275`), and each child's height is its parent's minus one (`quadTreeNode.cpp:42`). The
root carries `root_depth`, and a finest 8 mm leaf carries 0, because a node of height 0 refuses to
subdivide (`quadTreeNode.cpp:71`). A leaf's side is `root_size_mm / 2^(root_depth - depth)`.

The engine splits the list across CLAD `MemoryMapMessage`s, sent between a `MemoryMapMessageBegin`
that carries the origin id and header and a `MemoryMapMessageEnd` (`mapComponent.cpp:806-827`), with
the chunk size worked out from the message packet size (`mapComponent.cpp:721-732`). The gateway's
`NavMapFeed` collects the chunks between begin and end, appends their quads in the order they arrive,
and sends one `NavMapFeedResponse` per complete map
(`wire-os-victor/cloud/cloud/message_handler.go:3432-3509`). It fills `root_center_z` with a
hardcoded 0 (`message_handler.go:322-330`); the engine's internal header has z at 1, but the begin
message has no field for it (`memoryMap.cpp:258-264`,
`wire-os-victor/clad/src/clad/gateway/messageRobotToExternal.clad:300-307`). The content value is cast
straight across, because the CLAD and proto enums share their numbering (`message_handler.go:332-338`,
`wire-os-victor/clad/src/clad/types/memoryMap.clad:19-31`).

`color_rgba` is the engine's own visualisation colour, from `GetNodeVizColor`
(`memoryMap.cpp:92-141`), packed with red in the high byte and alpha in the low byte
(`wire-os-victor/coretech/common/engine/colorRGBA.h:139-147`). It carries one thing the content type
does not, which is a cliff's provenance. A cliff seen only by the drop sensors is black, one seen only
by the camera is gold, and one seen by both is pink, each at alpha 0.8 (`memoryMap.cpp:114-125`). The
function can also shade a proximity obstacle between green and cyan by its belief, but only while the
console variable `kRenderProxBeliefs` is on, and it defaults to off (`memoryMap.cpp:39`, `:96-99`). On
a stock robot every proximity obstacle is therefore plain cyan, or blue once explored, at full alpha,
and the belief is not on the wire at all.

`NavMapFeedRequest.frequency` is a period in seconds, not a frequency. The gateway forwards it as
`SetMemoryMapBroadcastFrequency_sec` and logs it as seconds (`message_handler.go:3421-3430`). The
engine stores it as `_broadcastRate_sec` (`mapComponent.cpp:327-331`) and advances the next broadcast
time by whole multiples of it (`mapComponent.cpp:377-384`). The Python SDK sends 0.5 by default
(`nav_map.py:381`). A negative value stops the feed. That is the engine's default, and it is what the
gateway sends when the stream closes (`wire-os-victor/engine/navMap/mapComponent.h:239`,
`message_handler.go:3440-3441`). Zero is not guarded. The update at `mapComponent.cpp:382` divides by
the period, so with 0 it adds NaN, not-a-number, to the next-broadcast time, and under IEEE
floating-point rules every later comparison against NaN is false, including the `FLT_LE` test at
`:378` (`wire-os-victor/lib/util/source/anki/util/math/math.h:210-213`). Read as written, a period of
0 gets you one map and then nothing, with no error. The next-broadcast time is a function-level
`static`, so no later client gets a map either until `vic-engine` restarts. This has not been tried
on a robot.

The robot broadcasts only when the map has changed since its last broadcast (`mapComponent.cpp:347`,
flags set at `mapComponent.cpp:392-396`). Setting the period does not mark the map as changed, so a
client that connects to a robot whose map is not changing receives nothing until something does.

The gateway does not notice a client that has gone away until it next has a map to send. On entry
the handler sets the period from the request and defers setting it back to -1
(`message_handler.go:3434-3441`). It then waits in a `select` over the engine's three map channels
and nothing else, with no case for the stream's context (`message_handler.go:3456-3504`), and it
looks at the context only after a `Send` (`:3494-3499`). A handler whose client has dropped the stream
therefore stays parked until the next broadcast. Then its `Send` fails, the handler returns, and its
deferred call sets the engine's single `_broadcastRate_sec` to -1 (`mapComponent.cpp:328-331`), which
stops broadcasts to every client (`mapComponent.cpp:347`). Each handler registers its own channels
with the engine's CLAD manager (`message_handler.go:3443-3451`), so every live handler receives every
broadcast and all parked handlers fire on the same map. A client that opens the feed while one is
parked gets one map and then nothing. wire-pod-rs never drops a stream while its feed runs. On the
first map it opens a second stream beside the first, which sets the period again after every parked
handler has reset it, and it reads both until the feed ends.

The Python SDK's `NavMapGridNode.add_child` is the reference decoder (`nav_map.py:194-243`, driven
from `nav_map.py:249-257`). It recurses; the same walk with an explicit stack goes like this. Create
the root node from `map_info`, with height `root_depth`, side `root_size_mm` and centre
`root_center_x`, `root_center_y`, and push a frame holding the root and a next-child index of 0. For
each quad, in the order received, look at the node on top of the stack. If its height equals the
quad's `depth`, that node is the quad: store the content, pop the frame, and advance the next-child
index of the frame beneath. Otherwise, give the node four children if it has none yet, each at height
one less, half the side, and centred a quarter of the parent's side away in the order +x+y, +x-y,
-x+y, -x-y. Then push a frame for the child at the node's next-child index and look again. Whenever a
frame's next-child index reaches 4, that node is full, so pop it as well and advance its parent's
index. When the last quad is placed, the stack is empty.

### 6.10 Behaviour control keeps cliffs out of the map

Taking behaviour control at `OVERRIDE_BEHAVIORS` activates `SDKOverrideAll`, whose config sets
`disableCliffDetection` (3.2;
`wire-os-victor/resources/config/engine/behaviorComponent/behaviors/victorBehaviorTree/sdkBehaviors/SDKOverrideAll.json:8`).
When the behaviour activates, `BehaviorSDKInterface` sends the robot process `EnableStopOnCliff(false)`
(`wire-os-victor/engine/aiComponent/behaviorComponent/behaviors/sdkBehaviors/behaviorSDKInterface.cpp:233-235`,
`wire-os-victor/engine/aiComponent/behaviorComponent/behaviorExternalInterface/beiRobotInfo.cpp:388-391`),
which clears the firmware's `_stopOnCliff` (`proxSensors.cpp:337-340`).

The firmware queues a `CliffEvent` that reports a cliff only when it stops for one
(`proxSensors.cpp:221-259`). With
`_stopOnCliff` false it does not stop, and it sends a `PotentialCliff` message instead
(`proxSensors.cpp:260-266`). The engine's handler for that message can play an animation in one
special mode and never touches the map (`robotToEngineImplMessaging.cpp:389-419`). With no
`CliffEvent`, `HandleCliffEvent` never runs and no cliff is written. A script holding
`OVERRIDE_BEHAVIORS` can therefore drive the robot off a table, and his map will not show the edge.
Priority `DEFAULT` leaves cliff stopping on (`SDKDefault.json:8`), and deactivation turns it back on
(`behaviorSDKInterface.cpp:261-264`).

The engine has a separate switch with a similar name, and it is not the one involved.
`HandleCliffEvent` returns early on a detected cliff when `IsCliffSensorEnabled()` is false
(`robotToEngineImplMessaging.cpp:429-431`), but that flag is set only by the `EnableCliffSensor`
message (`robotEventHandler.cpp:1553-1563`,
`wire-os-victor/engine/components/sensors/cliffSensorComponent.h:69-70`), and the SDK behaviour does
not send it. Under SDK control the cliff never reaches that check, because the firmware never
reports it.

---

## 7. Things that will cost you a day

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

**Picking the robot up deletes his map.** Leaving the treads and landing again are two
delocalizations, each one erases every existing map, and nothing is merged back. See
[6.6](#66-poses-origins-and-delocalization).

**`NavMapFeedRequest.frequency` is a period in seconds, and 0 is not safe.** Ask for 0.5 to get a map
at most every half second. Zero reaches an unguarded division in the engine, and as written it yields
one map and then silence, for every client, until the engine restarts. A robot whose map is not
changing sends nothing at all, however long you wait. See [6.9](#69-the-navmapfeed-wire-format).

**A nav-map quad's `depth` counts up from the leaves.** A finest leaf is 0 and the root is
`root_depth`. Decoding it as depth from the root puts every quad in the wrong place. See
[6.9](#69-the-navmapfeed-wire-format).

**`PATH_PLANNING_FAILED_RETRY` never arrives.** Planning failures come back as
`PATH_PLANNING_FAILED_ABORT`, and a goal the planner cannot reach probably comes back as neither. See
[6.8](#68-the-planner).

**Under `OVERRIDE_BEHAVIORS` the robot neither stops at cliffs nor maps them.** The firmware stops
reporting cliffs to the engine, so an edge a script drove him over is missing from the map afterwards.
See [6.10](#610-behaviour-control-keeps-cliffs-out-of-the-map).

---

## 8. What this document does not confirm

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

Several points in section 6 are readings of the code that were not tested on a robot.

What `GoToPose` returns for a goal the planner cannot reach is an inference. The chain up to the last
step is in 6.8: `XYPlanner` reports no plan, the path component returns to `Ready`, and the action
finds the robot away from the goal. The action then compares the last path id it sent with the last
one the robot acknowledged (`wire-os-victor/engine/actions/driveToActions.cpp:762`). Both start at 0
(`wire-os-victor/engine/components/pathComponent.h:293-294`) and are equal whenever the robot has
acknowledged its latest path, which gives `FAILED_TRAVERSING_PATH` (`driveToActions.cpp:781`). Only a
path still in flight would give `FOLLOWING_PATH_BUT_NOT_TRAVERSING` (`driveToActions.cpp:788`). The
most likely answer to an unreachable goal is therefore `FAILED_TRAVERSING_PATH`, not a planning
failure.

The proximity sensor's clearing reads like a bug. Whenever a reading finds no object, `objectPose`
is set to a default `Pose2d()`, which is the identity at the frame's origin
(`wire-os-victor/engine/components/sensors/proxSensorComponent.cpp:184-187`,
`wire-os-victor/coretech/common/engine/math/pose.cpp:25-29`). `UpdateNavMap` still clears on such a
reading and builds the clearing region from that pose (`proxSensorComponent.cpp:318-355`,
`:378-383`), so the region lies around the origin rather than in front of the robot. If that holds on
a robot, a clear view ahead clears nothing, and a proximity obstacle goes away only when the robot
drives over it, when a later reading of something farther along the same line clears past it, or when
the 600 s timeout expires.

The planner's minimum step disagrees with its own comment. `PlannerPoint::HalfStep` allows a half
step while the depth is at most `kMaxSubsampleDepth`, which is 2, and a point at depth 2 has a step of
8 mm, so its half step is 4 mm (`wire-os-victor/engine/xyPlannerConfig.h:101`, `:113-116`). The
comment beside the constant says the minimum is 8 mm (`xyPlannerConfig.h:46`). Which of the two the
robot's paths show has not been checked.

The gateway's `NavMapFeed` reads the begin, data and end messages from three separate channels in one
`select` (`wire-os-victor/cloud/cloud/message_handler.go:3443-3503`). Go chooses at random among
ready cases, so if the handler falls behind, an end message could be taken before the last data chunk
and a truncated map sent. That is an inference from the language's rules and has not been observed.

wire-pod's first event stream is never read (3.4). Whether that matters depends on gRPC flow control.
If the client's receive window fills, the gateway's send on that stream blocks, the stream's 512-slot
channel fills, and 250 ms later the IPC manager removes the listener
(`wire-os-victor/cloud/cloud/ipc_manager.go:234-250`). How long that would take with only
`stimulation_info` events and one keep-alive a second has not been measured.
