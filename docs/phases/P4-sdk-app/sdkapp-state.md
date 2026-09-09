# P4 state model: registry, stream ownership, meters, pinger

Everything here was read from the Go repository `C:/Users/voan2/Documents/GitHub/wire-pod` at `origin/main` (`81fa3b3`). Citations are repo-relative `path:line` at that ref. The state lives in `chipper/pkg/wirepod/sdkapp/robot.go` and `chipper/pkg/wirepod/sdkapp/jdocspinger.go`, with the on-disk shape defined in `chipper/pkg/vars/vars.go`.

This document describes the state as Go holds it, and marks the places where the Rust port deliberately reshapes it. The rule throughout is that anything the robot, the web UI or a file on disk can observe is reproduced, while the internal shape is free to change.

## The per-robot registry

### What a robot record holds

```go
type Robot struct {
    ESN               string
    GUID              string
    Target            string
    Vector            *vector.Vector
    BcAssumption      bool
    CamStreaming      bool
    EventStreamClient vectorpb.ExternalInterface_EventStreamClient
    EventsStreaming   bool
    StimState         float32
    ConnTimer         int32
    Ctx               context.Context
}
```

`robot.go:310-322`. The records live in a package-level slice `var robots []Robot` (`robot.go:20`) and are addressed by position, not by key. `ESN`, `GUID`, `Target`, `Vector` and `Ctx` are set once in `newRobot` and are effectively immutable afterwards. The five mutable fields are reached in three different ways, which is the source of most of the complexity below.

### `getRobot`: the cache

```go
func getRobot(serial string) (Robot, int, error) {
    for {
        if !inhibitCreation { break }
        time.Sleep(time.Second / 2)
    }
    for index, robot := range robots {
        if strings.EqualFold(serial, robot.ESN) {
            return robot, index, nil
        }
    }
    return newRobot(serial)
}
```

`robot.go:405-419`. Three things matter for the port.

It returns a **copy** of the struct, not a pointer. `robotObj.CamStreaming`, `.EventsStreaming`, `.StimState`, `.ConnTimer` and `.BcAssumption` on that copy are snapshots taken at lookup time and go stale immediately. This is exactly why the September work routed the live reads and writes of `CamStreaming`, `EventsStreaming` and `StimState` through ESN-keyed accessors rather than through the returned value. `ConnTimer` and `BcAssumption` were left addressed by slice index and unguarded (`server.go:65`, `server.go:330`, `bcassume.go:32`), which is the hazard described under the idle timer below. `robotObj.Vector` and `robotObj.Ctx` are pointer and interface copies, so those do alias the shared connection.

The match is `strings.EqualFold`, case-insensitive, against the stored ESN.

The cache is per-process and is only emptied by `removeRobot`. A robot stays connected until it is idle for 300 seconds or the user calls `/api-sdk/disconnect`.

### `inhibitCreation`: a global stall

`var inhibitCreation bool` (`robot.go:22`) is a plain boolean with no lock and no atomic. It is set true for the whole of `newRobot` (`robot.go:325`, cleared at `:348`, `:360`, `:367`, `:384`, `:401`) and for the whole of `removeRobot` (`robot.go:456`, cleared at `:479`), and `getRobot` spins on it at 500 ms granularity. Because `removeRobot` sleeps three seconds and `newRobot`'s liveness check has no deadline, one slow robot stalls **every** `/api-sdk/*` request in the process.

The Rust port replaces this with a per-ESN connect lock, so the same serial dials once while one robot's dial never blocks another's. This is a recorded improvement rather than a parity break: nothing observable depends on unrelated requests being stalled.

### `newRobot`: the connect path

`robot.go:324-403`, in order:

1. `RobotObj.Ctx = context.Background()` (`robot.go:329`). This context **never expires and is never cancelled**. Every RPC in `SdkapiHandler` that uses it is unbounded unless the arm shadows it, which only `get_battery` (`server.go:285-286`) and `net_probe` (`server.go:99`) do, both against `r.Context()`. Outside the handler, `enableImageStreaming` bounds a derived copy of it at five seconds (`server.go:671`).
2. Scan `vars.BotInfo.Robots` for a case-insensitive ESN match (`robot.go:333-346`). On a match it sets `ESN = strings.TrimSpace(strings.ToLower(serial))` (`robot.go:335`), `Target = robot.IPAddress + ":443"` (`robot.go:336`), and the GUID. **The loop has no `break`, so when several entries carry the same ESN the last match wins.**
3. No match returns `fmt.Errorf("error: robot not found in SDK info file")` (`robot.go:349`), which the handler prefixes again into the doubled `error: error: ...` body.
4. `vector.New(WithTarget, WithSerialNo, WithToken)` (`robot.go:354-358`).
5. **The liveness check**: `Conn.BatteryState(context.Background(), &vectorpb.BatteryStateRequest{})` (`robot.go:365`). A fresh background context with **no deadline**. A robot that is powered off but whose IP still routes can hang this call indefinitely, holding `inhibitCreation` true. The Rust port keeps the deadline as `None` by default so the observable behavior matches, but makes it an `Option<Duration>` so a test can bound it.
6. Opens an `EventStream` with whitelist `["stimulation_info"]` and **no** `connection_id`, storing it on `RobotObj.EventStreamClient` (`robot.go:372-386`). **Nothing ever reads this stream.** It is opened at connect time and held for the life of the cached robot. The stim endpoints open their own, separate stream with `connection_id: "wirepod"`. The Rust port does not open it, which is a recorded deviation: the loopback test asserts that connecting issues only `BatteryState`.
7. Appends to `robots` under `robotsMu` and captures `robotIndex := len(robots) - 1` (`robot.go:393-396`), then starts `go connTimer(robotIndex)` (`robot.go:399`).

### The 300 second idle timer

`connTimer(ind)` (`robot.go:423-453`) runs one goroutine per robot, keyed by slice index rather than by ESN. It bounds-checks once at entry, zeroes `robots[ind].ConnTimer`, then loops on a one-second sleep. On each tick it looks for its own index in `timerStopIndexes` and returns if it is there; otherwise, if `robots[ind].ConnTimer >= 300` it calls `removeRobot(robots[ind].ESN, "connTimer")` and returns; otherwise it increments the counter.

**Exactly one thing resets the timer**: the preamble's `robots[robotIndex].ConnTimer = 0` (`server.go:65`), which runs on every `/api-sdk/*` request except `get_sdk_info` and `debug`. `/cam-stream` runs its own preamble and discards the index (`server.go:710`), so a page showing only the camera is dropped after 300 seconds even while frames are flowing. The comment at `robot.go:36-38` states this explicitly. The Rust port keeps the asymmetry and tests it directly.

Both `ConnTimer` and `BcAssumption` are written without `robotsMu` by design, and the mutex comment says so (`robot.go:30-31`). Because `connTimer` re-indexes `robots[ind]` every tick without re-bounds-checking, a surviving timer can index a different robot or panic after `removeRobot` shrinks the slice. The Rust port keys everything by ESN, which removes the whole class of hazard without changing anything observable.

### `removeRobot`

`robot.go:455-480`. Sets `inhibitCreation = true`, rebuilds the slice by filtering out the matching ESN, and for the matched entry it appends the index to `timerStopIndexes` **only when the source is `"server"`** (`robot.go:462-463`), calls `stopCamStream` (`robot.go:467`), calls `stopEventStream` (`robot.go:470`), clears `BcAssumption` (`robot.go:471`), and sleeps three seconds (`robot.go:473`). Then it swaps the slice under `robotsMu` and clears `inhibitCreation`.

`/api-sdk/disconnect` passes `"server"`; `connTimer` passes `"connTimer"` and returns on its own. The three second sleep is why `disconnect` blocks for at least that long before answering `done`.

What is cleared: the `robots` entry and with it `CamStreaming`, `EventsStreaming`, `StimState`, `ConnTimer`, `BcAssumption`, the connection and the dead connect-time event stream; the event registry entry, immediately; and eventually the camera registry entry, once the departing handler runs its own cleanup. What **survives**: the per-ESN camera operation lock, and the camera meter with all of its accumulated totals. The gRPC connection is not explicitly closed; the value is simply dropped.

## Camera stream ownership

The camera registry is `map[string]*camStream` keyed by ESN, with a monotone generation counter (`robot.go:39-48`). The counter is incremented under `robotsMu` rather than atomically, starts at zero, and the first issued generation is therefore 1, so 0 is never a valid generation.

### Preemptive claim

`claimCamStream(esn, cancel)` (`robot.go:104-117`) takes the mutex, reads the previous entry, increments the generation, stores a new entry **unconditionally replacing any previous one**, sets the streaming flag, unlocks, and then cancels the previous owner **outside** the lock. It returns the new generation and a boolean meaning "I displaced a live owner".

This is last-writer-wins. A second tab, a reload or a retry takes the feed rather than stacking a second one on top. The rationale is recorded at `robot.go:99-103`: the camera has no stop protocol, so a reloaded image element must be able to take the feed.

### The settle, and why it is conditional

`startCamStream` (`server.go:684-695`) takes the per-ESN operation lock, calls `claimCamStream`, and sleeps `time.Second / 2` **only when it displaced someone**, then calls `enableImageStreaming(robotObj, true)`. The 500 ms exists to let the robot drop the `CameraFeed` that was just cancelled. A first claim on an idle robot does not pay it.

### Generation-checked release

`releaseCamStream(esn, gen)` (`robot.go:121-131`) returns false and changes nothing when the registry entry is missing or carries a different generation. Only on a match does it delete the entry, clear the flag and return true. `finishCamStream` (`server.go:700-707`) issues `EnableImageStreaming(false)` only when that returns true, so a superseded handler never turns the camera off underneath the handler that displaced it.

### The per-ESN operation lock

`camOps` is `map[string]*sync.Mutex`, one lock per ESN, created lazily by `camOpMu` (`robot.go:62-76`). Both `startCamStream` and `finishCamStream` hold it across their registry update **and** across the `EnableImageStreaming` RPC. That is the whole point: without it, a departing handler's disable can land after a replacement has already enabled the camera, and the new owner's feed dies.

Two rules go with it. The lock order is always operation lock first, `robotsMu` second; `camOpMu` is the only place that takes `robotsMu` while resolving a lock, and it releases it before returning, so the order cannot invert (`robot.go:64-66`). And entries are **never removed**, deliberately, because the key set is bounded by the robots that have ever streamed in this process and because freeing a mutex another goroutine already holds a pointer to would need a refcount (`robot.go:57-61`, which ends "Do not add cleanup here").

The long streaming loop runs entirely unlocked. Only the claim and the release hold the operation lock.

### What `stop_cam_stream` does and does not do

`stopCamStream(esn)` (`robot.go:136-144`) clears the flag and reads the current entry under the mutex, unlocks, then cancels. It **does not delete the registry entry**. The departing handler's `finishCamStream` is what deletes it and issues the disable, under the operation lock. Clearing the flag alone would not be enough, because the frame loop only samples the flag after `Recv` returns, which never happens on a robot sending no frames.

### What a superseded owner observes

Its context is cancelled by the new owner's claim, so `Recv` returns an error and the loop exits. The streaming flag reads `true` again, because the new owner set it, so the flag is not what stops it. Its release finds a generation mismatch and returns false, so it issues no disable. Its own deferred cancel runs harmlessly. It exits silently and the camera state belongs entirely to the new owner.

### Rust shape

Camera ownership is a per-entry field guarded by a `std::sync::Mutex` and never held across an await. The operation lock is a `tokio::sync::Mutex` held across the settle and the enable. Handlers hold a `#[must_use]` guard whose `async fn finish` performs the generation-checked release, with `Drop` only logging. Go's `gen` is a reserved word in edition 2024, so the type is `Generation` and the fields are `generation`.

## Event stream ownership

The event registry is `map[string]*eventStream` keyed by ESN with its own generation counter (`robot.go:190-196`), again incremented under `robotsMu` with 1 as the first issued value.

### Exclusive claim

`claimEventStream(esn, cancel)` (`robot.go:218-229`) runs entirely under `robotsMu`. If an entry already exists it returns `(0, false)` and **does not displace the incumbent**. Otherwise it increments the generation, stores the entry, sets the streaming flag and returns `(gen, true)`.

This is the deliberate opposite of the camera policy, and `robot.go:208-217` gives the reason: the camera has no stop protocol, whereas the stim graph has an explicit `stop_event_stream`, so a second begin is a double click that should cost nothing rather than tear down a working stream and blank the graph. Folding the "already running" test into the claim also closes the check-then-act window where two simultaneous begins could both read false.

The handler answers `done` either way (`server.go:474-481`, `server.go:504`), so a second begin is observably a no-op.

### Stop is one critical section, then a cancel

`stopEventStream(esn)` (`robot.go:253-263`) takes `robotsMu`, reads the current entry, **deletes it unconditionally** with no generation check, clears the streaming flag, and zeroes the stim value, all in one critical section. It then unlocks and only afterwards cancels the receiver's context.

Releasing ownership synchronously here rather than leaving it to the goroutine is what lets a `begin_event_stream` arriving immediately after a stop succeed. The old code could refuse it while the previous goroutine was still parked in `Recv`, which left the poller reading `error: must start event stream` until it gave up (`robot.go:248-252`). Cancelling after the unlock is what keeps the window closed rather than merely small.

Zeroing the stim value in the same section means there is no moment where `get_stim_status` reports a stale non-zero reading with the stream already stopped.

### Generation-gated stim writes

`setStimStateIfOwner(esn, gen, value)` (`robot.go:300-308`) looks up the registry entry under the mutex and returns without writing when the entry is missing or the generation differs. A receiver still unwinding from a cancelled `Recv` therefore cannot overwrite the value the current owner has just published. `releaseEventStream(esn, gen)` (`robot.go:236-246`) is generation-checked the same way, so a superseded receiver clears nothing belonging to its replacement.

Unlike the camera, none of this needs an operation lock. `robot.go:231-235` gives the structural reason: the camera's off switch is a separate RPC that can be issued and then land late, while this stream's off switch is a context cancellation, which can never arrive too late to matter.

### The value-present rule

`runEventStream` (`server.go:644-661`) writes the stim value only when

```go
if strings.Contains(fmt.Sprint(stimInfo), "velocity") {
    setStimStateIfOwner(esn, gen, stimInfo.Value)
}
```

`server.go:656-659`. That is a substring test against the protobuf text rendering of the `StimulationInfo` message, not a field check. Because proto3 omits zero-valued scalars from the text form, a message whose `velocity` is exactly zero is **skipped**. The Go test file calls this out and sets `Velocity: 1` on purpose so that a zero there would not let the test pass for the wrong reason.

The Rust port implements this as `velocity != 0.0`, which reproduces the effect exactly. An unconditional write would not. When the event is not a stimulation event the accessor returns a nil pointer, whose rendering contains no `"velocity"`, so it is skipped without a dereference.

Errors from the receiver go to `logger.Println("event stream: " + err.Error())` and nowhere else. Nothing is ever written to a response writer from the goroutine, because the request that owned it returned long before. Stream setup errors likewise reach only the log (`server.go:494-501`). The loop has no sleep and no backoff; it is driven entirely by `Recv` blocking.

## The camera meter

```go
type camMeter struct {
    bytes  uint64
    frames uint64
}
var camMeters = map[string]*camMeter{}
```

`robot.go:156-161`. One meter per ESN, created on first use by `getCamMeter` (`robot.go:167-176`), which takes `robotsMu` for the map lookup only. The **fields** are mutated with `atomic.AddUint64` from the frame loop (`server.go:775-776`) and read with `atomic.LoadUint64` from `readCamMeter` (`robot.go:182-185`). They are atomic rather than mutex-guarded because a 30 frames-per-second feed would otherwise take the package mutex thirty times a second per robot and contend with every status poll; the frame loop resolves the pointer once before receiving and then adds without locking (`robot.go:151-155`).

What is counted, and when: `len(response.GetData())` per received `CameraFeedResponse`, and one frame per response, both **before** the decode. A frame that fails to decode still crossed the wire, and the measurement is of the link rather than of the picture. The bytes going out to the browser are re-encoded at quality 50 and are deliberately not what is counted (`server.go:770-774`).

The counters are monotone. There is no reset anywhere in the package, and entries are never deleted for the same reason `camOps` entries are not (`robot.go:163-166`). **`removeRobot` does not clear them**, so a robot dropped by the idle timer and later reconnected resumes counting from its previous total. The only way a client sees the number go backwards is a server restart, which the dashboard handles explicitly as a counter reset rather than as a negative rate.

An ESN nobody has streamed reads `(0, 0)` rather than panicking, because `net_probe` answers for robots whose camera has never been opened. Note that `readCamMeter` reaches the value through `getCamMeter`, so **a read inserts an entry** for an unknown ESN. The Rust port drops that insert-on-read: a read of an unknown ESN returns `(0, 0)` without growing the map. Nothing observable depends on the insert. The Go test that pins per-robot isolation, `TestCamMeterKeepsRobotsApart` (`sdkapp_test.go:415-443`), asserts only that an unseen ESN reads `(0, 0)` (`sdkapp_test.go:438-442`); it cannot assert that the map stayed the same size, because that very read inserts. The Rust behavior satisfies the assertion the Go test actually makes.

In Rust the meters live in a separate, never-pruned `CamMeters` map keyed by ESN rather than in the per-robot entry, precisely so that totals survive eviction and reconnect. A test pins that.

## The jdocs pinger and the bot-status store

The pinger's state is one mutex-guarded slice (`jdocspinger.go:19-30`):

```go
var JdocsPingerBots struct {
    mu     sync.Mutex
    Robots []JdocsPingerRobot
}

type JdocsPingerRobot struct {
    ESN                string `json:"esn"`
    GUID               string `json:"guid"`
    IP                 string `json:"ip"`
    TimeSinceLastCheck int    `json:"timesince"`
    Stopped            bool   `json:"stopped"`
}
```

That mutex is entirely separate from `robotsMu`, and no code path holds both, so there is no ordering relationship between them.

`InitJdocsPinger` (`jdocspinger.go:129-150`) starts a goroutine that, every second under the mutex, increments every robot's `TimeSinceLastCheck` and sets `Stopped = true` once the counter exceeds 15 while the robot is not already stopped. `ShouldPingJdocs` (`jdocspinger.go:152-191`) is called from `connCheck` with the peer IP; it matches the IP against `vars.BotInfo.Robots`, then resets `TimeSinceLastCheck` to 0 and clears `Stopped`. It returns true, meaning "pull jdocs now", only on the stopped-to-running transition, and a robot the pinger has never seen is appended with `TimeSinceLastCheck: 0, Stopped: false` and also returns true.

The observable state machine is therefore: a fresh `/ok` makes the robot `online`; 16 seconds of silence sets `Stopped` and the robot reports `offline`; 120 seconds of silence makes it `disconnected`.

`GetConnectionStatus` (`jdocspinger.go:39-69`) is the projection `/api/get_bot_status` serializes. Under the pinger mutex it produces one element per entry in `vars.BotInfo.Robots`, in file order, so a robot the pinger has seen but that is absent from the bot-info file is not reported at all. Each element starts as `{esn, ip, status: "disconnected", timesince: -1}` and is then overwritten by the first matching pinger entry:

- `online` when `!bot.Stopped && bot.TimeSinceLastCheck <= 15`
- `offline` when `bot.Stopped && bot.TimeSinceLastCheck < 120`
- `disconnected` otherwise

The inner match is `bot.ESN == robot.Esn`, **exact byte equality**, not `EqualFold`, unlike every lookup in `robot.go`. A case mismatch between the pinger's list and the bot-info file silently yields `disconnected`. The `esn` in the output is the one stored in the bot-info file, in its stored case.

The slice is initialized as `statuses := []BotStatus{}` rather than `var statuses []BotStatus` (`jdocspinger.go:42-45`), which is the whole of the Go change in this file. It guarantees the body is `[]` and never the literal `null` when no robots are known. `/api-sdk/get_image_ids` still answers `null` when empty and the web UI depends on that literal, so the asymmetry is preserved rather than harmonized.

`PingerEnabled` (`jdocspinger.go:77`) defaults to true and is disabled by the environment variable `JDOCS_PINGER_ENABLED=false`, checked in both `InitJdocsPinger` (`jdocspinger.go:130-134`) and `BeginServer` (`server.go:802-805`). When it is disabled the ticker never starts, `connCheck` skips the ping bookkeeping, the pinger list stays empty, and every robot reports `disconnected` with `timesince: -1` forever.

## The bot-info file

`vars.BotInfo` is a `RobotInfoStore` (`vars.go:89-98`):

```go
type RobotInfoStore struct {
    GlobalGUID string `json:"global_guid"`
    Robots     []struct {
        Esn       string `json:"esn"`
        IPAddress string `json:"ip_address"`
        GUID      string `json:"guid"`
        Activated bool   `json:"activated"`
    } `json:"robots"`
}
```

Marshal order is declaration order, so `global_guid` comes first and then `robots`, and each robot serializes as `esn`, `ip_address`, `guid`, `activated`. That order is observable through `/api-sdk/get_sdk_info`, which marshals this value directly (`server.go:190`), so the Rust wire projection has to match it field for field with no extras.

Two lookup rules follow from `newRobot` (`robot.go:333-346`). The scan is case-insensitive and has **no `break`**, so when several entries share an ESN the **last** one wins. And when a matched entry's `guid` is empty the robot uses `vars.BotInfo.GlobalGUID` instead (`robot.go:338-343`), which is the global GUID fallback.

The file is read without any lock by `newRobot`, `NewWP`, `GetConnectionStatus`, `ShouldPingJdocs`, `pingJdocs` and `RunMDNS`, while `RunMDNS` writes into `vars.BotInfo.Robots[i].IPAddress` and spawns a goroutine to write the file (`jdocspinger.go:247-250`).

For the Rust port the on-disk struct carries `#[serde(default)]` plus a `#[serde(flatten)]` extra map on every level, so unknown and fork-only fields survive a round trip and rollback to the Go server stays safe. Because those extras would corrupt the byte-exact `get_sdk_info` body, a separate wire projection without them is what the route serializes. Extras are written back in sorted order so a round trip is deterministic. Never write a real GUID into a document, a test fixture or a log line; use `<guid>`.

## Robot authentication metadata

The SDK attaches credentials per RPC rather than per connection. `tokenAuth.GetRequestMetadata` returns a single header:

```go
return map[string]string{
    "authorization": "Bearer " + t.token,
}, nil
```

`vector-go-sdk@v0.0.0-20231108155304-62168f3595d6/pkg/vector/token.go:9-13`, with the literal header name and `Bearer ` prefix at `token.go:11`. It is installed with `grpc.WithPerRPCCredentials` inside `vector.New` (`pkg/vector/vector.go:42-52`, the credential option at `vector.go:46`), alongside `client.WithInsecureSkipVerify()`, so the robot's self-signed certificate is accepted without verification. `RequireTransportSecurity` returns true (`token.go:15-17`), so the header only travels over TLS.

The token is the robot's GUID, resolved by the rules above. The Rust `TonicRobotConn` attaches `authorization: Bearer <guid>` on every call, and the loopback test asserts the literal metadata is present on each RPC rather than only on the first.

The settings routes that bypass gRPC entirely use the same credential over HTTPS. Twelve `/api-sdk/*` routes reach one endpoint, `https://<ip>:443/v1/update_settings`, through four helpers in `urlreqs.go`: `eye_color`, `custom_eye_color`, `volume`, `locale`, `location`, `timezone`, `time_format_12`, `time_format_24`, `temp_c`, `temp_f`, `button_hey_vector` and `button_alexa`. Each helper sets `Authorization: Bearer <guid>` and shares one transport with certificate verification disabled (`chipper/pkg/wirepod/sdkapp/urlreqs.go:9-11`, `:17`, `:31`, `:45`, `:59`).
