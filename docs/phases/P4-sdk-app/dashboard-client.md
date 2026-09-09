# Dashboard client contract

What the Vector Brain dashboard JavaScript requires of the server. Every citation is a
`path:line` into the Go repo `C:/Users/voan2/Documents/GitHub/wire-pod` at `origin/main`
(`81fa3b3`), and the same bytes are vendored under `assets/webroot/` in this repo. The Rust
port has to honour every requirement listed here, because these files ship unchanged and the
browser is the only thing that observes the difference.

The client lives in four files:

| File | Role |
|---|---|
| `chipper/webroot/sdkapp/settings.html` | The page. Adds the dashboard card, wraps the old settings body in a drawer, and loads the new script at `settings.html:614`. |
| `chipper/webroot/sdkapp/js/vectorbrain.js` | The dashboard itself, 1240 lines, an IIFE with no dependencies on the other scripts. |
| `chipper/webroot/sdkapp/js/main.js` | Pre-existing page logic. Owns the stim poller and the photo list. |
| `chipper/webroot/sdkapp/js/common.js` | Pre-existing helper. Owns the battery fetch that the dashboard calls through. |

`vectorbrain.js` reads its own serial rather than sharing `main.js`'s global. At
`vectorbrain.js:63` it does
`vbEsn = new URLSearchParams(window.location.search).get("serial") || ""`, so an absent
`?serial=` yields the empty string and several pollers then skip their request entirely.

## The four pollers

All four are created by the same `makePoller` helper at `vectorbrain.js:113-156` and started
together in `init()` at `vectorbrain.js:1175-1178`.

| Poller | Function | Interval | Constant | Endpoint |
|---|---|---|---|---|
| Status | `pollStatus` | 2000 ms | `STATUS_POLL_MS`, `vectorbrain.js:17` | `GET /api/get_bot_status` |
| Battery | `pollBattery` | 5000 ms | `BATTERY_POLL_MS`, `vectorbrain.js:18` | `POST /api-sdk/get_battery` |
| Logs | `pollLogs` | 1000 ms | `LOG_POLL_MS`, `vectorbrain.js:19` | `GET /api/get_logs_json` |
| Network | `pollNet` | 3000 ms | `NET_POLL_MS`, `vectorbrain.js:834` | `GET /api-sdk/net_probe` |

The poller is a self-rescheduling `setTimeout` chain, not a `setInterval`, and three of its
properties matter to the server. A slow call can never stack, because `busy` is set before the
function runs and the next tick is scheduled only after the promise settles
(`vectorbrain.js:124-146`); a 15 second battery call simply skips its own ticks. A hidden tab
issues no requests at all, because the tick short-circuits on `document.hidden` and reschedules
without calling the function (`vectorbrain.js:126-129`). There is no stop path, so the pollers
run for the life of the document, and all four are kicked on `visibilitychange` back to visible
(`vectorbrain.js:1180-1194`).

Steady state for one open settings tab is roughly two requests per second plus the open camera
stream.

## The stim poller and its circuit breaker

The stim poller is separate, lives in `main.js`, and runs at 500 ms
(`main.js:76`, the `setInterval(..., 500)` closing at `main.js:122`). It is started by
`showSection('section-stim')` and driven by the module-level `stimRunning` flag.

Three behaviours are load-bearing for the server contract.

The guard at `main.js:68-70` clears any orphaned timer through `window.interval` before
installing a new one. It is read through `window` because `interval` is an implicit global that
does not exist before the first call.

The stop branch at `main.js:77-85` sends `POST /api-sdk/stop_event_stream`, clears the timer,
sets `window.interval = null` and returns, so no further `get_stim_status` is issued after a
stop.

The circuit breaker at `main.js:110-121` counts consecutive failures in `stimFails`, resets the
counter to zero on every successful parse (`main.js:89`), and at three failures sets
`stimRunning = false`, which makes the next tick send the stop and clear the timer. Three
failures at 500 ms is about 1.5 seconds, and the stop lands on the following tick, so teardown
is about two seconds from the first failure.

The breaker only works because the failure signal is `response.json()` rejecting. The success
body of `GET /api-sdk/get_stim_status` is a bare JSON number, printed with `fmt.Fprint` from
`stimState(esn)` at `server.go:512`, and the error body is the plain text
`error: must start event stream` at `server.go:515`. That string is never compared; it is simply
not valid JSON, and the rejection is the signal. If the Rust port ever answers with valid JSON
on the error path, for example `{"error":"..."}` or `null`, then `.json()` resolves, `stimFails`
resets to zero, the breaker never trips, and the chart plots an undefined value twice a second
for the life of the page. The error body for `get_stim_status` must stay non-JSON plain text.

The comment at `main.js:111-116` names the literal explicitly, so it is also the documented
intent, not an accident.

## `net_probe`

The request is built at `vectorbrain.js:889-891` as
`"/api-sdk/net_probe?serial=" + encodeURIComponent(vbEsn)`, and `pollNet` at
`vectorbrain.js:1052-1081` returns early without any request when `vbEsn` is empty or
`#vbLatency` is missing.

The transport wrapper is `fetchProbe` at `vectorbrain.js:868-887`, and it is the only call on
the page that treats an `/api-sdk/` plain-text error as a first-class case. It does four things
in order. A non-2xx status throws `Error("http " + status)`. The body is read with
`response.text()`, not `response.json()`. Leading whitespace is stripped once, and if the first
six characters are the literal `error:` the remainder becomes the thrown message, with its
leading whitespace stripped once more. Only then is the body handed to `JSON.parse`, and a parse
failure throws `Error("unreadable response")`.

The prefix is exactly `error:`, lowercase, with no space required. The Go handler emits
`"error: " + err.Error()` at `server.go:112` and `server.go:125`, so the message the panel prints
is whatever follows the space. That message is user-visible: it lands in `#vbNetProbe`,
whitespace-collapsed and truncated at 40 characters by `netShort` at `vectorbrain.js:942-949`.
This is the one place a server error string reaches the user verbatim, so the Rust port's
`error:` text for this route is part of the visible contract.

Fields read from the parsed body, at `vectorbrain.js:1057-1068`:

- `rttMs` must be a number. `typeof probe.rttMs !== "number"` throws `Error("bad probe")` and the
  sample is counted as lost. This is the only required field.
- `target` is used verbatim in the panel; a non-string becomes the empty string.
- `probe` names the RPC that was timed and is printed as-is; a non-string becomes the empty
  string.
- `camBytes` is read only for differencing.
- `camOn` is read only by the RUN button, at `vectorbrain.js:1083-1144`, which refuses to measure
  and reports `camera off` when the flag is false.
- `camFrames` is present in the body and is never read by any client code. The Rust port must
  still emit it, because it is part of the documented shape, but nothing observable depends on
  its value.

On any failure the catch at `vectorbrain.js:1069-1077` pushes `null` into both rolling windows,
which the loss counter sees, sets `netPrev = null` so the next successful poll also yields a null
throughput sample, and stores the message for the panel. The poller then continues at 3 seconds
indefinitely with no backoff and no give-up.

## `camBytes` differencing

Throughput is computed client-side by `netDelta` at `vectorbrain.js:1037-1049`. The current
reading is stored in `netPrev` at `vectorbrain.js:1040`, before any guard runs, so a rejected
sample still advances the baseline. With no previous reading the function returns `null`. With a previous reading it
divides the byte difference by the elapsed seconds and returns bytes per second.

The guard at `vectorbrain.js:1046-1048` is the one the server has to respect:

```js
if (seconds <= 0 || probe.camBytes < prev.bytes) {
  return null;
}
```

A counter that went backwards is read as a server restart, not a negative rate, and the sample is
dropped. So the counters must be monotone for the life of the process. In Go they are per-ESN
atomics that are never reset (`robot.go:163-166`) and are not cleared by `removeRobot`
(`robot.go:455-480`), which is why a robot that is evicted by the idle timer and then reconnects
resumes counting from its previous total. If the Rust port folds the meter into an evictable
entry, every reconnect looks like a restart and costs a throughput sample.

Differencing on the client is also why `net_probe` holds no per-request state and why the
averaging window is the page's choice. The comment at `vectorbrain.js:1034-1036` says so
directly.

## `/cam-stream`

The `<img>` source is built at `vectorbrain.js:391`:

```js
var src = "/cam-stream?serial=" + encodeURIComponent(vbEsn) + "&_=" + Date.now();
```

The `_` parameter is an unconditional cache-buster with no meaning. The server must ignore
unknown query parameters. Go's handler reads only `r.FormValue("serial")`, which does exactly
that.

`stopCam` at `vectorbrain.js:403-424` assigns an inlined one-pixel transparent GIF data URI and
then removes the `src` attribute. Assigning the empty string instead would make the browser
re-request the document URL. Aborting this way is what makes the server see the client
disconnect and disable image streaming on the robot, which is the only teardown the client has.
A handler that leaks the stream leaves a backgrounded tab draining the robot's battery while the
client is doing the right thing.

The camera state machine at `vectorbrain.js:354-544` depends on three server behaviours that are
invisible in the response body.

A second request for the same ESN must cancel the first, and the first response must end
cleanly, with no error and no truncation signal. A clean end fires no event on the `<img>`, which
is why the two stall detectors exist: `camStalled` needs at least two frames and 12 seconds of
silence (`CAM_STALL_MS`, `vectorbrain.js:24`), and `camNeverArrived` covers a docked robot whose
request hangs open with zero frames after 8 seconds (`CAM_FIRST_FRAME_MS`, `vectorbrain.js:29`).

A failure must be something the browser cannot decode as an image, so that the `error` event
fires. Go writes `"error: " + err.Error()` at HTTP 200 before it sets the multipart content type
(`server.go:712` and `server.go:744`, the header set at `server.go:747`), so the body arrives as a
sniffed `text/plain; charset=utf-8`, and the browser's decode failure is what `markCamFailure` at
`vectorbrain.js:456-463` is written to catch.

Recovery has exactly one trigger. `maybeRetryCam` is called only from the success branch of
`pollStatus`, at `vectorbrain.js:269`. Retry additionally requires `lastStatus === "online"`
(`vectorbrain.js:479`) and is capped at three failures (`CAM_MAX_RETRIES`,
`vectorbrain.js:33`) with a linear backoff of 20 seconds times the failure count
(`vectorbrain.js:490`). So the camera cannot self-heal while `/api/get_bot_status` is failing or
while the matched entry says anything other than `online`. Only a manual click on the frame,
which resets the budget with a one second debounce, revives it.

The response framing itself is documented in `camstream.md`.

## `/api/get_logs_json`

Requested at `vectorbrain.js:622-623` as
`"/api/get_logs_json?level=" + encodeURIComponent(logLevel) + "&since=" + since`. The four level
chips carry `data-level` values of the empty string, `info`, `warn` and `error`
(`settings.html:210-213`), so the default request is literally `level=&since=0`.

Three server requirements come out of `pollLogs` at `vectorbrain.js:612-674`.

The response must be an array and never the JSON literal `null`. The guard at
`vectorbrain.js:631` is `!Array.isArray(logs) || logs.length === 0`, which returns silently, so a
`null` body is not a crash, but it removes the client's ability to distinguish an empty ring from
a malformed body. Go's `GetEntries` builds with `make([]Entry, 0, ringCount)`, so the JSON is
always `[]`.

Each entry's `t` must be a JSON number in milliseconds since the epoch. A non-number is coerced
to `0` for the cursor comparison at `vectorbrain.js:641` and renders as the empty string.

The `since` filter must stay strict, that is `entry > since` and not `>=`. The client
deliberately asks from one millisecond earlier, `logSince > 0 ? logSince - 1 : 0` at
`vectorbrain.js:621`, and de-duplicates at the boundary with a key built from the whole entry.
The comment at `vectorbrain.js:617-620` states the reason: with a strict filter, an entry written
in the same millisecond as the newest one already returned would otherwise be skipped forever. A
server that switched to `>=` would still work but would send one redundant entry per poll.

The other fields are `level`, `comp`, `bot` and `msg`, all strings. `level` and `comp` are run
through `safeToken` at `vectorbrain.js:74-79`, which strips anything outside `[A-Za-z0-9_-]`, and
are used as CSS class suffixes, so the server's uppercase `DEBUG`, `INFO`, `WARN` and `ERROR` are
what match the stylesheet. `msg` is written with `textContent`, so no HTML injection is possible.

A chip click bumps a generation counter, resets the cursor to zero and clears the buffer
(`selectChip`, `vectorbrain.js:676-698`), so changing the level re-downloads the server's whole
retained ring filtered by the new level. An in-flight response for the previous level is dropped
by the generation check at `vectorbrain.js:628-630`.

## `/api/get_bot_status`

Requested at `vectorbrain.js:255` with no query string. The body must be a JSON array, and the
match is at `vectorbrain.js:262`:

```js
if (bots[i] && bots[i].esn === vbEsn) {
```

That is an exact, case-sensitive string comparison against the raw `?serial=` value from the
page's own URL. Everything on the Go server side matches serials with `strings.EqualFold`, so the
two sides disagree about case. The consequence is severe and hard to diagnose: if the emitted
`esn` differs in case from the case in the page's link, the status card, the subline and the
entire camera retry loop go dead while the camera's first attempt still works. The Rust port must
emit the ESN exactly as it is stored in the bot-info file, never a normalised form.

Fields read:

- `esn`, the match key, described above.
- `status`, passed through `safeToken` and then checked against the set `online`, `offline`,
  `disconnected` and `unknown` at `vectorbrain.js:48-53`. Anything else is folded to `unknown`.
  The server only ever emits the first three; `unknown` exists purely as the client's fallback.
- `ip`, a string. A falsy value renders the literal `no ip`.
- `timesince`, a number, rendered only when it is a number and not negative. The server sends
  `-1` for a robot the pinger has never seen.

Anything that is not an array, including the JSON literal `null`, yields no match and renders the
same as an empty array, so both current consumers tolerate `null`. The contract is still `[]`,
because `null` costs the client the ability to tell "no robots" from "malformed body", and the Go
side made that change deliberately (`jdocspinger.go:42-45`).

The load-bearing side effect is that the success branch calls `maybeRetryCam()` at
`vectorbrain.js:269`. This endpoint failing therefore also freezes the camera's recovery path.

On failure the catch at `vectorbrain.js:271-278` counts into `statusFails` and only renders the
unknown state at three consecutive failures, so roughly six seconds of the last good reading are
held. There is no user-visible error text.

## `serial` on a POST with an empty body

Two of the requests the page makes are POSTs that carry the serial only in the query string and
send no body at all.

`getBatteryStatus` in `common.js:26-31` issues
`POST /api-sdk/get_battery?serial=<esn>` with the header `Content-Type: application/json`, an
empty body, and `AbortSignal.timeout(15000)`.

`sendForm` in `main.js:143-156` issues `POST /api-sdk/<route>?serial=<esn>` with
`Content-Type: application/x-www-form-urlencoded` and an empty body, and is how
`begin_event_stream` (`main.js:136`), `stop_event_stream` (`main.js:78`) and every settings tile
are called. `get_sdk_settings` is not one of them: `getCurrentSettings` issues it with its own
`XMLHttpRequest` at `main.js:309-316`, in the same shape.

Go's `r.FormValue` reads the URL query regardless of the body's content type, which is why this
works today. A Rust handler that only reads a typed body extractor would reject both requests. The
port must merge the query string with any urlencoded body, with the body shadowing the query for
a repeated key, and must ignore unknown keys.

Note also that the serial is URI-encoded inconsistently. `vectorbrain.js` uses
`encodeURIComponent`; `main.js:146-148` and `common.js:26` concatenate the raw value. ESNs are
hexadecimal so this never differs in practice, but a missing `?serial=` produces the literal
string `serial=null` from `main.js`, because `URLSearchParams.get` returned `null`, and an
omitted-but-present `serial=` from `vectorbrain.js`. The server should treat both as "no robot".

## `POST /api-sdk/get_battery`

The dashboard calls through the pre-existing helper, at `vectorbrain.js:331-350`, and reads only
two fields (`vectorbrain.js:283-328`): `battery_volts` as a number, and
`is_on_charger_platform` as a truthiness check. `status` and `battery_level` are ignored.

The guard at `vectorbrain.js:301-311` is the one to preserve. The percentage helper is called
only when the voltage is genuinely present, because Go's protobuf JSON omits `battery_volts`
entirely at exactly 0.0 volts, and the helper's falsy-voltage branch would then fabricate 70
percent on precisely the robots that are in trouble. Any Rust body for this route has to
reproduce the omit-at-zero behaviour rather than emit `"battery_volts":0`.

Failure is silent. A rejecting `.json()` on the plain-text error body, a non-object result and the
15 second abort all land in one catch and render placeholder text.

## `get_image_ids` returns the literal `null`

`main.js:166` is a string comparison against the raw response text:

```js
if (xhr.response == "null") {
```

An empty photo list must therefore serialise as the JSON literal `null`, not as `[]`. This is the
opposite of the `/api/get_bot_status` rule, and the asymmetry is deliberate on both sides. The
route itself is not in the early slice, but the contract is pinned now so that whoever implements
it does not "clean it up".

## Everything else the server must honour

1. `/api-sdk/` errors are plain text at HTTP 200. Three separate clients depend on it:
   `fetchProbe` string-matches the `error:` prefix, and both `getBatteryStatus` and the stim
   poller rely on `.json()` rejecting. A 4xx or 5xx status is tolerable for battery and
   `net_probe`, which both check `response.ok`, but a valid JSON error object breaks
   `get_stim_status` silently.
2. Unknown query parameters must be ignored, on `/cam-stream` because of the cache-buster and in
   general because the page never sends a complete parameter set.
3. `begin_event_stream` while a stream is already claimed must return `done` without restarting
   anything (`server.go:474-481`). The page treats a second begin as a double click.
4. `stop_event_stream` must always return `done`, must be idempotent, and must tolerate a stop
   arriving after a subsequent begin. The drawer's close handler sets `stimRunning = false`
   (`vectorbrain.js:777-789`) and the old poller's next tick sends the stop, which can land after
   the user has reopened and re-selected Stim. The `stimFails` breaker exists to recover from
   exactly that ordering, and recovery is manual.
5. Every `sendForm` completion fires `getCurrentSettings()` (`main.js:153-155`), which issues
   `POST /api-sdk/get_sdk_settings`. So every begin and every stop of the event stream also
   triggers a settings refetch.
6. The four panels for Actions, Navigation, Talk and Memory are static mock content with TODO
   badges and issue no network calls at all. The Rust port needs no endpoints for them.
7. `settings.html` declares no `<meta charset>`, so `vectorbrain.js` builds every non-ASCII glyph
   from character codes and the file must stay pure ASCII (`vectorbrain.js:40-41`). If any part of
   the asset pipeline ever re-encodes these files, that invariant has to hold.
