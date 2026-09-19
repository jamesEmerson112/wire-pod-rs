# P4 route contract: `/api-sdk/*`, `/api/*`, `/ok`, `/cam-stream`

Every fact here was read from the Go repository `C:/Users/voan2/Documents/GitHub/wire-pod` at `origin/main` (`81fa3b3`). Citations are repo-relative `path:line` at that ref. Three files carry the whole surface: `chipper/pkg/wirepod/sdkapp/server.go` holds `SdkapiHandler`, `camStreamHandler` and the route registrations in `BeginServer`; `chipper/pkg/wirepod/sdkapp/jdocspinger.go` holds `connCheck` and the bot-status projection; `chipper/pkg/wirepod/config-ws/webserver.go` holds `apiHandler`, the root file server, and the `DisableCachingAndSniffing` middleware that shapes the fallback 404.

## Ground rules that apply to every route

**Bodies are byte-exact.** The vendored web UI in `assets/webroot/` reads several of these bodies as literal strings rather than as structured data, so the Rust port reproduces them byte for byte, including the absence of a trailing newline where Go writes with `fmt.Fprint` and its presence where Go writes with `json.Encoder.Encode` or `http.Error`. Where a Go quirk is observable it is preserved rather than corrected.

**No method is ever checked.** Neither `SdkapiHandler` (`server.go:56-632`) nor `camStreamHandler` (`server.go:709-789`) nor `apiHandler` (`webserver.go:27-79`) inspects `r.Method`. GET, POST, HEAD and anything else behave identically. The dashboard mixes them freely: it POSTs most `/api-sdk/*` calls through `XMLHttpRequest`, GETs `get_stim_status` and `net_probe` through `fetch`, and GETs `get_image_thumb` and `/cam-stream` through image elements.

**Parameters come from `r.FormValue`, not the query string alone.** `SdkapiHandler` reads `serial` with `r.FormValue("serial")` (`server.go:57`) and every per-route parameter the same way. Go's `FormValue` parses the body first for POST, PUT and PATCH, then merges the URL query, and returns the body value when both are present. So a urlencoded body parameter shadows the same-named query parameter. The one exception in scope is `/api/get_logs_json`, which reads `r.URL.Query()` directly (`webserver.go:291-301`) and therefore ignores body parameters entirely.

**Header policy differs between the two API prefixes.** `/api-sdk/*` and `/cam-stream` set no CORS headers and no cache headers at all. `DisableCachingAndSniffing` exists twice, once in `sdkapp` (`server.go:791-798`, three headers, `Cache-Control: no-cache, no-store, must-revalidate;` with a trailing semicolon) and once in `config-ws` (`webserver.go:415-421`, four headers), but the `sdkapp` copy wraps only the `/sdk-app` file server (`server.go:811`) and the `config-ws` copy wraps only the web root (`webserver.go:439`). Neither wraps an API route. The running Go server separates the two mounts observably: `curl -si http://localhost:8080/sdk-app` reaches the `sdkapp` mount and answers 404 with `Content-Type: text/plain; charset=utf-8`, `Pragma: no-cache` and `X-Content-Type-Options: nosniff` only, while `curl -si http://localhost:8080/sdk-app/` misses the exact pattern, falls through to the web root, and answers the same 404 with `Expires: 0` added. By contrast `apiHandler` sets `Access-Control-Allow-Origin: *` and `Access-Control-Allow-Headers: *` as its first two statements (`webserver.go:28-29`), so those two headers are present on every `/api/*` response including the 404 from its `default` case, which `curl -si http://localhost:8080/api/does_not_exist` confirms.

**Content types are mostly sniffed on `/api-sdk/*`, `/ok` and `/cam-stream`.** Across those three groups exactly one route sets a content type that survives: `/cam-stream` sets `multipart/x-mixed-replace; boundary=--boundary` (`server.go:747`). `/api-sdk/get_sdk_settings` attempts `application/octet-stream` but calls `w.Header().Set` after `w.WriteHeader`, so the value never reaches the wire (`server.go:225-226`). Everything else in those groups relies on Go's content sniffing, which yields `text/plain; charset=utf-8` for the text bodies and `image/jpeg` for the photo routes. `/api/*` is the opposite: twelve of its handlers set a content type explicitly (`webserver.go:165`, `:207`, `:222`, `:262`, `:270`, `:275`, `:280`, `:285`, `:302`, `:307`, `:312`, `:398`), among them `/api/get_bot_status` and `/api/get_logs_json` with `application/json` (`webserver.go:307`, `webserver.go:302`), and `get_ota` copies the upstream response headers through (`webserver.go:345-349`). In this document "text" means that sniffed `text/plain; charset=utf-8`.

**Almost every failure is HTTP 200.** The only non-200 statuses inside `SdkapiHandler` are the `default` case's 404, `get_sdk_info`'s 500 when no robots are authenticated, and `trigger_wake_word`'s two error paths. Errors are reported as a plain-text body beginning with `error: `, and the dashboard is built around that convention.

## The preamble

```go
robotObj, robotIndex, err := getRobot(r.FormValue("serial"))
robot := robotObj.Vector
ctx := robotObj.Ctx
if r.URL.Path != "/api-sdk/get_sdk_info" && r.URL.Path != "/api-sdk/debug" {
    if err != nil {
        fmt.Fprint(w, "error: "+err.Error())
        return
    }
    robots[robotIndex].ConnTimer = 0
}
```

`server.go:56-66`.

`getRobot` runs for **every** `/api-sdk/*` request, with no exception. That includes `get_sdk_info`, `debug`, and any unknown path that is about to receive a 404. It is a cache lookup that falls through to `newRobot`, which dials the robot and runs an undeadlined `BatteryState` liveness check before the request can proceed (`robot.go:405-419`, `robot.go:324-403`). The two named paths are exempt only from the error write and from the `ConnTimer` reset, never from the dial itself.

An unknown serial produces the doubled prefix `error: error: robot not found in SDK info file`, because `newRobot` returns an error whose text already starts with `error: ` (`robot.go:349`) and the preamble prepends another (`server.go:62`). A dial or liveness failure produces `error: ` followed by the raw gRPC status text, for example `error: rpc error: code = Unavailable desc = ...`. Both are HTTP 200 with a sniffed text content type.

`robots[robotIndex].ConnTimer = 0` is the only thing that resets the 300 second idle timer. `/cam-stream` runs its own copy of the preamble and discards the index (`server.go:710`), so it never resets the timer.

## Routing facts

`/api-sdk/` is registered with a trailing slash (`server.go:806`), which makes it a Go subtree pattern. Every path under the prefix reaches `SdkapiHandler`, and dispatch inside the handler is exact string equality on `r.URL.Path` in a tagless `switch` whose `default` clause is written first (`server.go:67-70`). Writing `default` first has no effect on matching; Go evaluates the cases in source order and only falls to `default` when none match. The same shape applies to `/api/`, registered at `webserver.go:428` and dispatched on `strings.TrimPrefix(r.URL.Path, "/api/")` with an exact-string switch (`webserver.go:31-78`).

Because both are subtree patterns, `GET /api-sdk/` with an empty rest runs the handler, pays for the preamble with an empty serial, and answers the doubled not-found error at 200. `GET /api-sdk` without the trailing slash never reaches the handler at all: Go's `ServeMux` answers a 301 with `Location: /api-sdk/` and Go's standard redirect body. Observed live, that response is `Content-Type: text/html; charset=utf-8`, `Content-Length: 44`, and the body `<a href="/api-sdk/">Moved Permanently</a>.` followed by two newlines. `/api` behaves the same way with `Location: /api/`.

`/cam-stream` (`server.go:818`), `/sdk-app` (`server.go:811`), `/ok` and `/ok:80` (`server.go:813-814`) are exact patterns with no trailing slash, so only those literal paths match.

The path is canonicalised before any of those patterns is tried, in two steps that are both observable on the running server. `findHandler` cleans the escaped path with `cleanPath` and answers a 301 when the cleaned form differs (`net/http/server.go:2681`, `:2690-2698`): `GET /api-sdk//debug?serial=bogus` answers 301 with `Location: /api-sdk/debug?serial=bogus` and a 62-byte body, `GET /api//get_bot_status` answers 301 with `Location: /api/get_bot_status` and a 54-byte body, and `curl --path-as-is` shows the same for `/api-sdk/./debug` and `/api-sdk/x/../debug`. That clean preserves a trailing slash, so `/ok/` is not `/ok`: it is the file-server 404. The trailing-slash redirect runs ahead of the cleaned-path one, so `//api-sdk` answers `Location: /api-sdk/` rather than `Location: /api-sdk`. Then the routing tree unescapes each segment before it compares it against a pattern (`net/http/routing_tree.go:205-215`) and the handlers switch on the already-decoded `r.URL.Path`, so `GET /ok%3A80` answers `ok`, `GET /api%2Dsdk/debug?serial=bogus` reaches `SdkapiHandler`, and `GET /api-sdk/deb%75g?serial=bogus` answers the preamble-exempt 404 rather than the doubled connect error. A `%2F` does not create a segment boundary: `GET /ok%2F80` answers the file-server 404.

Every one of these registrations goes on `http.DefaultServeMux`, and both listeners serve that same mux: port 80 from `BeginServer` (`server.go:824`) and the configurable web port from `StartWebServer` (`webserver.go:441`, default `8080`). All routes are therefore reachable on both ports, and a Rust port that splits them across routers must keep both listeners serving the union.

An unmatched path falls through to the root file server, which is wrapped in `config-ws`'s `DisableCachingAndSniffing` (`webserver.go:439`, middleware at `webserver.go:415-421`). The resulting 404 carries body `404 page not found\n` with `Content-Type: text/plain; charset=utf-8`, `X-Content-Type-Options: nosniff`, `Pragma: no-cache`, `Expires: 0`, and **no** `Cache-Control`. The middleware does set `Cache-Control`, but Go's `serveError` path resets it, which is why static 200s from `/` carry `Cache-Control: no-cache, no-store, must-revalidate, max-age=0` alongside the other three headers while the 404 does not. The plan records this as verified against the live Go server.

## `/api-sdk/*` route table

`grep -n 'case r.URL.Path' chipper/pkg/wirepod/sdkapp/server.go` at `origin/main` returns **45** lines, and the table below has one row per line. The counted set matches the plan's figure of 45.

`/api-sdk/debug` is deliberately not in the table because it has no `case` arm; it is covered in the section that follows.

The slice column marks the ten routes in the early Rust slice. All ten are named in the plan: `conn_test`, `net_probe`, `begin_event_stream`, `stop_event_stream`, `get_stim_status`, `begin_cam_stream`, `stop_cam_stream`, `disconnect`, `get_sdk_info` and `debug`. Everything else is deferred to P4 proper and answers a stub 404 in the slice.

| Route | Go | Params read | Success body | Error bodies | Status and content type | Side effects | Slice |
|---|---|---|---|---|---|---|---|
| `conn_test` | server.go:71-74 | `serial` (preamble only) | `success`, no newline | preamble only | 200, text | none beyond the preamble | in slice |
| `net_probe` | server.go:75-129 | `serial` | `{"rttMs":13.482,"probe":"ProtocolVersion","target":"<ip>:443","camBytes":0,"camFrames":0,"camOn":false}`, six keys in that order, no `omitempty`, no trailing newline | `error: rpc error: code = DeadlineExceeded desc = context deadline exceeded` on timeout, `error: <err>` otherwise, and `error: <err>` on an unreachable marshal failure | 200, text | one `ProtocolVersion(client_version=5, min_host_version=0)` RPC on a 5 s deadline parented on `r.Context()`; the verdict is discarded | in slice |
| `alexa_sign_in` | server.go:130-135 | `serial` | `success` | preamble only | 200, text | `AlexaOptIn(OptIn: true)`, error discarded | deferred (P4) |
| `alexa_sign_out` | server.go:136-141 | `serial` | `success` | preamble only | 200, text | `AlexaOptIn(OptIn: false)`, error discarded | deferred (P4) |
| `cloud_intent` | server.go:142-150 | `serial`, `intent` | `done` | preamble only | 200, text | `AppIntent(Intent: intent)`, error discarded | deferred (P4) |
| `eye_color` | server.go:151-155 | `serial`, `color` | `done` | preamble only | 200, text | direct HTTPS POST to `https://<ip>:443/v1/update_settings` setting `eye_color` and disabling `custom_eye_color`; certificate verification off; response discarded; panics on transport failure (urlreqs.go:27-39) | deferred (P4) |
| `custom_eye_color` | server.go:156-164 | `serial`, `hue`, `sat` | the two form values concatenated with no separator and no newline, written with `Fprint` so a `%` is echoed literally | preamble only | 200, text | `update_settings` with `custom_eye_color.{enabled,hue,saturation}`, values interpolated into JSON unescaped (urlreqs.go:13-25) | deferred (P4) |
| `volume` | server.go:165-169 | `serial`, `volume` | `done` | preamble only | 200, text | `update_settings` `master_volume` | deferred (P4) |
| `locale` | server.go:170-174 | `serial`, `locale` | `done` | preamble only | 200, text | `update_settings` `locale` | deferred (P4) |
| `location` | server.go:175-179 | `serial`, `location` | `done` | preamble only | 200, text | `update_settings` `default_location` | deferred (P4) |
| `timezone` | server.go:180-184 | `serial`, `timezone` | `done` | preamble only | 200, text | `update_settings` `time_zone` | deferred (P4) |
| `get_sdk_info` | server.go:185-196 | none; preamble-exempt | `json.Marshal(vars.BotInfo)`, keys `global_guid` then `robots[{esn, ip_address, guid, activated}]`, no trailing newline | `no bots are authenticated\n` at 500 when `len(vars.BotInfo.Robots) == 0`; `error marshaling json` at 200 | 200 text, or 500 text plus `X-Content-Type-Options: nosniff` | none | in slice |
| `get_sdk_settings` | server.go:197-229 | `serial` | the robot's raw `json_doc` string | the bare `err.Error()` with **no** `error: ` prefix on RPC failure; `error: bot refuses to return robotsettings` after five wrong answers | 200; the intended `application/octet-stream` is set after `WriteHeader` and lost, so text | `PullJdocs(ROBOT_SETTINGS)` up to five times with 500 ms sleeps; writes the jdocs file synchronously and drops `client_metadata`; panics on an empty `named_jdocs` | deferred (P4) |
| `play_sound` | server.go:231-281 | `serial`, multipart file `sound` | empty body | none; file errors return without writing anything | 200 with no content type, because zero bytes are written | `ExternalAudioStreamPlayback` at 8 kHz, 1024-byte chunks, 60 ms between chunks | deferred (P4) |
| `get_battery` | server.go:283-300 | `serial` | `json.Marshal` of the `BatteryStateResponse` | `error: <err>` for the RPC and for a marshal failure | 200, text | `BatteryState` on a 15 s deadline parented on `r.Context()` | deferred (P4) |
| `time_format_12` | server.go:301-304 | `serial` | `done` | preamble only | 200, text | `update_settings` `clock_24_hour: false` | deferred (P4) |
| `time_format_24` | server.go:305-308 | `serial` | `done` | preamble only | 200, text | `update_settings` `clock_24_hour: true` | deferred (P4) |
| `temp_c` | server.go:309-312 | `serial` | `done` | preamble only | 200, text | `update_settings` `temp_is_fahrenheit: false` | deferred (P4) |
| `temp_f` | server.go:313-316 | `serial` | `done` | preamble only | 200, text | `update_settings` `temp_is_fahrenheit: true` | deferred (P4) |
| `button_hey_vector` | server.go:317-320 | `serial` | `done` | preamble only | 200, text | `update_settings` `button_wakeword: 0` | deferred (P4) |
| `button_alexa` | server.go:321-324 | `serial` | `done` | preamble only | 200, text | `update_settings` `button_wakeword: 1` | deferred (P4) |
| `assume_behavior_control` | server.go:325-328 | `serial`, `priority` | `success`, written **before** the call is made | preamble only | 200, text | spawns a `BehaviorControl` bidirectional stream goroutine and sets `BcAssumption` unguarded; `priority=high` means `OVERRIDE_BEHAVIORS`, anything else means `DEFAULT` (bcassume.go:10-32) | deferred (P4) |
| `release_behavior_control` | server.go:329-332 | `serial` | `success` | preamble only | 200, text | unguarded `robots[robotIndex].BcAssumption = false` | deferred (P4) |
| `say_text` | server.go:333-346 | `serial`, `text` | `success` | when the text is 600 runes or more it writes `error: text is too long` and then, with no `return`, still says the text and appends `success`, so the body is the two strings concatenated | 200, text | `SayText(DurationScalar: 1, UseVectorVoice: true)` | deferred (P4) |
| `move_wheels` | server.go:347-359 | `serial`, `lw`, `rw` | empty body | preamble only | 200 with no content type | `DriveWheels`; `Atoi` errors discarded, so a bad value means 0 | deferred (P4) |
| `move_lift` | server.go:360-369 | `serial`, `speed` | empty body | preamble only | 200 with no content type | `MoveLift` | deferred (P4) |
| `move_head` | server.go:370-379 | `serial`, `speed` | empty body | preamble only | 200 with no content type | `MoveHead` | deferred (P4) |
| `get_faces` | server.go:380-390 | `serial` | `json.Marshal(resp.Faces)` | the bare `err.Error()` with **no** `error: ` prefix | 200, text | `RequestEnrolledNames` | deferred (P4) |
| `rename_face` | server.go:391-409 | `serial`, `id`, `oldname`, `newname` | `success` | bare `err.Error()`, no prefix | 200, text | `UpdateEnrolledFaceByID`; `Atoi` error discarded | deferred (P4) |
| `delete_face` | server.go:410-424 | `serial`, `id` | `success` | bare `err.Error()`, no prefix | 200, text | `EraseEnrolledFaceByID` | deferred (P4) |
| `add_face` | server.go:425-439 | `serial`, `name` | `success` | bare `err.Error()`, no prefix | 200, text | `AppIntent(Intent: "intent_meet_victor", Param: name)` | deferred (P4) |
| `mirror_mode` | server.go:440-466 | `serial`, `enable` | `success` | `fmt.Fprint(w, err)`, the error value rather than `err.Error()`, and with no prefix | 200, text | `EnableMirrorMode(Enable: enable == "true")` | deferred (P4) |
| `begin_event_stream` | server.go:467-505 | `serial` | `done`, always and immediately, including when a stream is already owned | preamble only; stream setup errors go to the log and never to the body | 200, text | claims event ownership and spawns a receiver on `EventStream` with whitelist `["stimulation_info"]` and `connection_id: "wirepod"`, on a cancellable child of the robot context | in slice |
| `stop_event_stream` | server.go:506-509 | `serial` | `done`, always, even for an ESN with no stream | preamble only | 200, text | `stopEventStream`: deletes the registry entry, clears the flag and zeroes stim in one critical section, then cancels after unlocking | in slice |
| `get_stim_status` | server.go:510-516 | `serial` | the `float32` through Go's `%v`, that is shortest round-trip 32-bit formatting: `0`, `0.1`, `0.75`, `1`; no quotes and no newline | `error: must start event stream`, which must stay non-JSON because the client's breaker relies on `response.json()` rejecting | 200, text | none; the endpoint makes no RPC | in slice |
| `begin_cam_stream` | server.go:517-520 | `serial` | `done` | preamble only | 200, text | none; the only statement in the arm is commented out | in slice |
| `stop_cam_stream` | server.go:521-524 | `serial` | `done` | preamble only | 200, text | `stopCamStream`: clears the flag and cancels the owner's context, but leaves the registry entry for that owner to delete | in slice |
| `get_image_ids` | server.go:525-536 | `serial` | `json.Marshal([]uint32)`, which is `null` rather than `[]` when the robot has no photos; the web UI compares against the literal `null` | none; the RPC error is discarded and the next line dereferences a nil response, so an RPC failure panics the handler goroutine | 200, text | `PhotosInfo` on the undeadlined robot context | deferred (P4) |
| `get_image` | server.go:537-554 | `serial`, `id` | the raw `resp.Image` bytes | `error: <atoi err>` for a bad or missing `id`, `error: <rpc err>` for the RPC | 200, sniffed `image/jpeg` | `Photo(PhotoId: uint32(id))`; `resp.Success` is never checked | deferred (P4) |
| `get_image_thumb` | server.go:555-572 | `serial`, `id` | the raw `resp.Image` bytes | same two `error: ` forms | 200, sniffed `image/jpeg` | `Thumbnail(PhotoId: uint32(id))` | deferred (P4) |
| `delete_image` | server.go:573-590 | `serial`, `id` | `done` | `error: <atoi err>`, `error: <rpc err>` | 200, text | `DeletePhoto`; the response and its `success` flag are discarded | deferred (P4) |
| `get_robot_stats` | server.go:591-601 | `serial` | the raw lifetime-stats `json_doc` string | `error: <err>` | 200, text | `PullJdocs(ROBOT_LIFETIME_STATS)`; no caching and no disk write; panics on an empty `named_jdocs` | deferred (P4) |
| `print_robot_info` | server.go:602-604 | `serial` | `fmt.Fprint(w, robot)`, the default formatting of the `*vector.Vector` value | preamble only | 200, text | none | deferred (P4) |
| `disconnect` | server.go:605-608 | `serial` | `done`, written after the call returns, so the request blocks for at least 3 s | preamble only | 200, text | `removeRobot(esn, "server")`: stops both streams, clears `BcAssumption`, sleeps 3 s, then swaps the robots slice; the camera meter survives | in slice |
| `trigger_wake_word` | server.go:609-630 | `serial` | `success` | `Failed to trigger wake word: <err>\n` at 500; `Consolevars returned error\n` at whatever status the robot returned | 200 on success; 500 or the upstream status on failure, text plus nosniff from `http.Error` | plain HTTP GET to `http://<ip>:8889/consolevarset?key=FakeButtonPressType&value=singlePressDetected` with a 10 s client timeout | deferred (P4) |

### `/api-sdk/debug` and unknown paths

`/api-sdk/debug` is one of the two paths named in the preamble exemption (`server.go:60`) but it has no `case` arm, so it always falls to `default` and answers `not found\n` at 404 with `Content-Type: text/plain; charset=utf-8` and `X-Content-Type-Options: nosniff` (`server.go:68-70`, the newline supplied by `http.Error`). Because it is exempt, a bad or missing serial does not change that: the preamble neither writes an error nor resets the timer, and the request still reaches the 404. It is in the early Rust slice for exactly this reason, since it pins the ordering between the preamble and the fallback.

Any other unknown path under `/api-sdk/` is **not** exempt. The preamble runs first, so a bad serial produces `error: error: robot not found in SDK info file` at 200 and the request never reaches the switch. Only with a good serial does an unknown path receive the 404. Both halves are confirmed live: `curl -si "http://localhost:8080/api-sdk/debug?serial=bogus"` answers 404 with `Content-Length: 10` and no `Cache-Control`, while `curl -si http://localhost:8080/api-sdk/does_not_exist` answers 200 with `Content-Length: 46`, which is exactly the length of the doubled error string. Routes that exist in Go but are deferred by the early Rust slice answer that same 404 as a stub; they are listed by name in `deviations.md` and no test asserts their status, so the stub can be replaced without changing a test.

## `/api/*` route table

`grep -n 'case "' chipper/pkg/wirepod/config-ws/webserver.go` returns **25** lines, but only **22** of them are route arms in `apiHandler` (`webserver.go:32-74`). The remaining three are the `level` switch inside `handleGetLogsJSON` (`webserver.go:292`, `:294`, `:296`), which selects a log level and is not a route. The plan's figure of 25 counts those three; the number of `/api/*` routes is 22.

Every row below carries `Access-Control-Allow-Origin: *` and `Access-Control-Allow-Headers: *` (`webserver.go:28-29`), and an unmatched `/api/*` path answers `not found\n` at 404 with those same CORS headers (`webserver.go:76-77`).

| Route | Go | Summary |
|---|---|---|
| `add_custom_intent` | webserver.go:32, handler :81 | Decodes a `CustomIntent` from the JSON body, rejects missing fields and invalid Lua with 400, appends it, saves, and answers `Intent added successfully.` |
| `edit_custom_intent` | webserver.go:34, handler :104 | Decodes a body carrying a 1-based `number` plus the fields to change, applies only the non-empty ones, saves, and answers `Intent edited successfully.` |
| `get_custom_intents_json` | webserver.go:36, handler :154 | Answers the raw custom-intents file as `application/json`, or 400 when no intent exists yet, or 500 when the file cannot be read |
| `remove_custom_intent` | webserver.go:38, handler :169 | Deletes the intent at the 1-based `number` from the JSON body, saves, and answers `Intent removed successfully.` |
| `set_weather_api` | webserver.go:40, handler :186 | Sets or disables the weather provider and key from the JSON body, writes the config to disk, and answers `Changes successfully applied.` |
| `get_weather_api` | webserver.go:42, handler :206 | Answers `vars.APIConfig.Weather` as `application/json`, including the key in plaintext |
| `set_kg_api` | webserver.go:44, handler :211 | Decodes the JSON body straight into `vars.APIConfig.Knowledge`, writes the config to disk, and answers `Changes successfully applied.` |
| `get_kg_api` | webserver.go:46, handler :221 | Answers `vars.APIConfig.Knowledge` as `application/json`. The plaintext API key stays because the web UI depends on it |
| `set_stt_info` | webserver.go:48, handler :226 | Validates the requested language against the engine, starts a model download when one is missing, otherwise switches the language, writes the config, reloads the voice processor, and answers `Language switched successfully.` |
| `get_download_status` | webserver.go:50, handler :261 | Answers the current download status as `text/plain` and then resets it to `not downloading` when it read `success` or contained `error`. This is the one route in scope that mutates on read |
| `get_stt_info` | webserver.go:52, handler :269 | Answers `vars.APIConfig.STT` as `application/json` |
| `get_config` | webserver.go:54, handler :274 | Answers the whole `vars.APIConfig` as `application/json` |
| `get_logs` | webserver.go:56, handler :279 | Answers `logger.LogList` as `text/plain`, the legacy formatted tail |
| `get_debug_logs` | webserver.go:58, handler :284 | Answers `logger.LogTrayList` as `text/plain`, the tray's larger tail |
| `get_logs_json` | webserver.go:60, handler :289-304 | Full detail below |
| `get_bot_status` | webserver.go:62, handler :306-309 | Full detail below |
| `is_running` | webserver.go:64, handler :311 | Answers the literal `true` as `text/plain`. This is the health probe used throughout the runbooks |
| `delete_chats` | webserver.go:66, handler :316 | Clears `vars.RememberedChats` in memory and answers `done` |
| `get_ota` | webserver.go:68, handler :321 | Reverse-proxies the third path segment to `https://archive.org/download/vector-pod-firmware/<name>`, copying request and response headers through, with 500s for parse, request, transport and copy failures |
| `get_version_info` | webserver.go:70, handler :356 | Reads the installed version file, queries GitHub for the latest release tag and commit, and answers a six-key JSON object `{fromsource, installedversion, installedcommit, currentversion, currentcommit, avail}`, or 500 when GitHub cannot be reached |
| `generate_certs` | webserver.go:72, handler :402 | Runs the certificate combo generator and answers `done`, or `error: <err>\n` at 500 |
| `is_api_v3` | webserver.go:74 | Answers the literal `it is!` inline from the switch arm, with no handler function |

### `/api/get_bot_status` in full

Registered at `webserver.go:62`, handled at `webserver.go:306-309`:

```go
func handleGetBotStatus(w http.ResponseWriter) {
    w.Header().Set("Content-Type", "application/json")
    json.NewEncoder(w).Encode(sdkapp.GetConnectionStatus())
}
```

The handler takes only the response writer, so **no parameter of any kind is read**, not even from the query string. It makes no RPC; it reports the jdocs pinger's view of the world. `json.Encoder.Encode` appends a trailing newline, so the body always ends with `\n`. Headers are `Content-Type: application/json` plus the two CORS headers, status 200 always.

The body is a JSON array with one element per entry in `vars.BotInfo.Robots`, in file order (`jdocspinger.go:39-69`). Each element has exactly four keys in declaration order: `esn`, `ip`, `status`, `timesince` (`jdocspinger.go:32-37`), none with `omitempty`. The status vocabulary is exactly `online`, `offline` and `disconnected`. Defaults before any pinger match are `status: "disconnected"` and `timesince: -1`, so a robot the pinger has never heard from reports `-1`.

When no robots are known the body is `[]\n`, not `null\n`. That is the whole of the Go change in this file: `statuses := []BotStatus{}` replaced `var statuses []BotStatus` (`jdocspinger.go:42-45`). The asymmetry with `/api-sdk/get_image_ids`, which still answers the literal `null` when empty, is deliberate and preserved.

Thresholds and the inner match are documented in `sdkapp-state.md`.

### `/api/get_logs_json` in full

Registered at `webserver.go:60`, handled at `webserver.go:289-304`. Parameters are read from `r.URL.Query()` only, so a body parameter is ignored.

`level` accepts `info`, `warn` and `error`. Anything else falls to the `default` arm and yields `DEBUG`, the minimum, which means everything. That includes the absent parameter and, notably, the literal string `debug` itself, which matches no case and reaches the same default. `since` is parsed with `strconv.ParseInt` base 10 into an `int64` and the parse error is swallowed, so a bad or absent value becomes `0`, which again means everything.

`logger.GetEntries(min, since)` (`logger.go:216-232`) walks a fixed 500-entry ring buffer in chronological order and keeps an entry when `parseLevel(e.Level) >= min && e.TimeMS > since`. The `since` comparison is **strictly greater than**, which is why the dashboard sends `logSince - 1`. The result is built with `make([]Entry, 0, ringCount)` and is therefore non-nil, so an empty result encodes as `[]` and never as `null`. Reading does not drain or reset the ring.

The response sets `Content-Type: application/json` (`webserver.go:302`) plus the two CORS headers, status 200, and the body is the JSON array plus the trailing newline from `Encode`. Each entry has five keys in declaration order (`logger.go:48-55`): `t` (Unix milliseconds, `int64`), `level`, `comp`, `bot` and `msg`. `level` is the string form `DEBUG`, `INFO`, `WARN` or `ERROR`, and `parseLevel` maps an unknown string back to `DEBUG`. `comp` is one of the twelve component constants at `logger.go:33-46` (`jdocs`, `token`, `stt`, `intent`, `llm`, `sdkapp`, `web`, `mdns`, `ble`, `lua`, `voice`, `conn`) or the empty string for `logger.Println` and `logger.LogUI` calls, both of which pass an empty `comp` and `bot`. Their levels differ: `Println` logs at `DEBUG` (`logger.go:237`) and `LogUI` at `INFO` (`logger.go:243`). ANSI escapes are stripped from `msg` before storage (`logger.go:139`).

The dashboard requires `t` to be numeric and relies on `[]` never being `null`, so both are part of the contract.

## `/ok` and `/ok:80`

Both are exact patterns on the default mux (`server.go:813-814`) and both reach `connCheck` in `jdocspinger.go:193-221`. The handler's only case is `strings.Contains(r.URL.Path, "/ok")`, with a `default` that answers `not found\n` at 404; since the mux only ever routes the two literal paths here, the substring test is belt and braces. `/ok:80` carries a literal colon in the path, which is the robot's liveness heartbeat, so the Rust router must be tested against it explicitly rather than assumed.

The handler reads one parameter, `runMDNS`, through `r.FormValue`. When it is exactly the string `true` the handler calls `RunMDNS("t")` and answers `ran`. Otherwise, when the pinger is enabled, it takes the peer IP from `r.RemoteAddr`, and if that IP appears anywhere in the JSON rendering of `vars.BotInfo` it calls `ShouldPingJdocs` and, on a stopped-to-running transition, `pingJdocs`. If the IP does not appear it spawns `RunMDNS` for that address. Either way it then answers `ok`. Both bodies are written with `fmt.Fprintf` and have no trailing newline, status 200, sniffed text.

The early Rust slice implements the bodies only. The jdocs ping and the mDNS side effects belong with P1's mDNS and jdocs work and are recorded as deferred.

## `/cam-stream`

Registered as an exact pattern at `server.go:818` and handled at `server.go:709-789`. It is not under `/api-sdk/`, so it runs its own copy of the preamble and, because it discards the robot index, it never resets the idle timer. The MJPEG framing, the ownership handoff, the metering and the Rust test strategy are specified in [camstream.md](camstream.md).

## The fallback 404

Anything that matches none of the patterns above reaches the root file server registered at `webserver.go:439`, wrapped in `DisableCachingAndSniffing` (`webserver.go:415-421`). For a path with no file behind it the response is:

```
HTTP/1.1 404 Not Found
Content-Type: text/plain; charset=utf-8
X-Content-Type-Options: nosniff
Pragma: no-cache
Expires: 0

404 page not found
```

with a trailing newline on the body and, importantly, **no `Cache-Control` header**. The middleware sets one, but Go's `serveError` in `net/http/fs.go` deletes `Cache-Control` before calling `http.Error`, which is why that one header and only that one goes missing. A successful static 200 from the same handler does carry `Cache-Control: no-cache, no-store, must-revalidate, max-age=0` in addition to the other three headers.

Both halves are observed rather than inferred, because the source alone does not settle which headers survive the error path. On the running Go server, `curl -si http://localhost:8080/no-such-path` returns exactly the header set above, with `Content-Length: 19` and no `Cache-Control`; `curl -si http://localhost:8080/css/style.css` returns 200 with `Cache-Control: no-cache, no-store, must-revalidate, max-age=0`, `Expires: 0`, `Pragma: no-cache` and `X-Content-Type-Options: nosniff` together. The Rust router reproduces the 404 exactly, including the absence of `Cache-Control`, and the routing test asserts it for a fixed list of paths the Go server genuinely lacks.
