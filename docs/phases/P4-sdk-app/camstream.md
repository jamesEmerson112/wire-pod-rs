# P4 camera stream contract: `/cam-stream`

Read from `C:/Users/voan2/Documents/GitHub/wire-pod` at `origin/main` (`81fa3b3`). The handler is `camStreamHandler` at `chipper/pkg/wirepod/sdkapp/server.go:709-789`, with its helpers `startCamStream` (`server.go:684-695`), `finishCamStream` (`server.go:700-707`) and `enableImageStreaming` (`server.go:670-679`), and the ownership registry in `chipper/pkg/wirepod/sdkapp/robot.go:99-144`.

## Route and parameters

`/cam-stream` is registered as an exact pattern with no trailing slash (`server.go:818`), so only that literal path reaches the handler. It is **not** under `/api-sdk/`, so it does not share `SdkapiHandler`'s preamble; it runs its own copy at `server.go:710`. Like every other route in the package it checks no HTTP method.

One parameter is read, `serial`, through `r.FormValue` (`server.go:710`). The dashboard builds the URL as `"/cam-stream?serial=" + encodeURIComponent(vbEsn) + "&_=" + Date.now()` and assigns it to an image element, so every retry is a fresh GET with a fresh cache-buster (`chipper/webroot/sdkapp/js/vectorbrain.js:389-392`). **The `_` parameter is never read and must stay ignored.** A Rust handler that rejects unknown query parameters would break the retry path.

## The idle timer is not reset

The handler calls `getRobot` and discards the robot index (`robotObj, _, err := getRobot(...)` at `server.go:710`). The only statement that resets `ConnTimer` is in `SdkapiHandler`'s preamble (`server.go:65`), so **`/cam-stream` never resets the 300 second idle timer**. A page showing only the camera is dropped by `connTimer` after five minutes even while frames are still arriving. The comment at `robot.go:36-38` records this deliberately. The Rust port keeps the asymmetry and has a test that asserts `/cam-stream` does not touch the timer while an `/api-sdk/*` call does.

## Lifecycle before the first frame

1. On a `getRobot` error the handler writes `"error: " + err.Error()` and returns (`server.go:711-714`). Status 200, no content type set, so it sniffs to `text/plain; charset=utf-8`. A browser image element rejects that body, which is how the dashboard detects the failure.
2. `ctx, cancel := context.WithCancel(robotObj.Ctx)` with a deferred cancel (`server.go:721-722`). The feed hangs off a cancellable child of the robot's background context rather than off the request context. Built on `robotObj.Ctx` alone it would outlive the request, because `Recv` blocks until a frame arrives and a docked or sleeping robot sends none, so the loop never reaches its request-done case and the goroutine plus its gRPC stream leak on every browser disconnect (`server.go:716-720`).
3. A watcher goroutine `<-r.Context().Done(); cancel()` is started **before** the claim (`server.go:727-730`), so a browser that goes away during the settle sleep is noticed rather than leaving the handler to open a feed nobody is reading. Cancel is idempotent and safe on a stream nobody has claimed.
4. `gen := startCamStream(robotObj, cancel)` (`server.go:733`) takes the per-ESN operation lock, claims ownership (displacing and cancelling any incumbent), sleeps 500 ms **only if it displaced someone**, and then calls `EnableImageStreaming(true)` on a five second deadline.
5. `defer finishCamStream(robotObj, gen)` (`server.go:737`) is registered immediately, so every exit path releases ownership under the same lock and issues `EnableImageStreaming(false)` only when the generation still matches.
6. `CameraFeed(ctx, &vectorpb.CameraFeedRequest{})` on the cancellable context (`server.go:739-742`). On error the handler writes `"error: " + err.Error()` and returns (`server.go:743-746`). The content type has not been set at this point, so this error is also sniffed as text.
7. Only after the feed opens does the handler set the response content type (`server.go:747`).

## Wire format

The handler sets exactly one response header, and Go supplies the rest of the response line and framing itself:

```
Content-Type: multipart/x-mixed-replace; boundary=--boundary
```

`server.go:747`. Note that the declared boundary token is the literal string `--boundary`, which itself begins with two dashes.

Each part is written as

```
--boundary\r\nContent-Type: image/jpeg\r\n\r\n
```

`server.go:783`, followed immediately by the JPEG payload. There are three consequences a port must reproduce rather than correct.

**The separator does not match the declared boundary.** For a boundary token of `--boundary` the strict delimiter would be `----boundary`. Go writes `--boundary`. Browsers tolerate the mismatch, and every shipped copy of the dashboard renders against it, so a port that fixes this changes observed behavior.

**There is no trailing CRLF after the payload.** The next part's separator follows the last JPEG byte directly.

**There is no closing boundary and no terminator.** The stream simply ends when the handler returns. There is no `Content-Length`, no cache header, and no explicit flush; delivery relies on Go's response buffering.

The payload is not the bytes the robot sent. Every frame is decoded and then re-encoded with `jpeg.Encode(multi, img, &jpeg.Options{Quality: 50})` (`server.go:784-786`). The quality is exactly 50 and the encode error is discarded. `multi := io.MultiWriter(w)` (`server.go:748`) is a single-writer pass-through with no other effect.

## The frame loop

`server.go:752-788`. Each iteration is a `select` whose first case returns on `r.Context().Done()` and whose `default` branch does the work:

1. `if !isCamStreaming(esn) { return }` (`server.go:759-761`). The flag is false once `stop_cam_stream`, `removeRobot` or a newer handler has taken the feed away. Note the subtlety: a **newer** handler sets the flag back to true, so this check is not what stops a superseded handler. Context cancellation is.
2. `client.Recv()`; **on error the loop returns** (`server.go:762-768`). Before the September work a `Recv` error was silently ignored and the loop re-entered, which busy-spun on a persistent error for as long as the request was alive. Returning ends that.
3. `atomic.AddUint64(&meter.bytes, uint64(len(imageBytes)))` and `atomic.AddUint64(&meter.frames, 1)` (`server.go:775-776`), **before** the decode. A frame that fails to decode still crossed the wire, and the measurement is of the link rather than of the picture. The re-encoded outbound bytes are deliberately not what is counted (`server.go:770-774`). The meter pointer is resolved once before the loop (`server.go:751`) because the lookup needs the package mutex and the loop runs at the robot's frame rate.
4. `image.Decode`; **on error `continue`**, skipping the frame (`server.go:777-782`). A truncated or empty frame used to reach `jpeg.Encode` as a nil image, which panics and takes the whole process with it. This is a crash fix, not a cosmetic one, and the skip is part of the contract: an undecodable frame is counted by the meter and produces no part on the wire.
5. Write the part header, then encode the frame.

## Ownership handoff

Camera ownership is preemptive, which is the deliberate opposite of the event stream's exclusive policy. `claimCamStream` (`robot.go:104-117`) replaces the registry entry unconditionally, bumps the generation, sets the streaming flag, and then cancels the previous owner outside the lock, returning a boolean that says whether there was one.

What the old handler experiences: its context is cancelled, so `Recv` returns an error and the loop returns; its deferred `finishCamStream` takes the operation lock, finds a generation mismatch in `releaseCamStream` (`robot.go:121-131`), and therefore issues **no** disable; its connection closes.

What the new handler experiences: it sleeps 500 ms to let the robot drop the cancelled `CameraFeed`, enables image streaming, and begins writing.

The net effect is that a reload, a second tab or a retry replaces the feed rather than stacking a second one, and **the camera is never left off underneath a live owner**. That invariant, phrased as "whenever a live owner exists, the last thing said to the robot must have been on", is what the Go test `TestCamStreamHandoffKeepsCameraOn` pins, and it is why both the registry update and the enable and disable RPCs are held under the per-ESN operation lock.

What a browser sees on handoff is worth stating separately: a `/cam-stream` response that ends cleanly, which is exactly what the displaced handler produces, fires **no error event** on the image element, so the browser keeps painting the last frame it received. The dashboard therefore treats arriving frames as the only liveness signal and guards on the frame counter having advanced past one (`vectorbrain.js:426-429`), which is another reason the meter has to keep counting correctly across a handoff.

`stop_cam_stream` reaches the same end by a different route. `stopCamStream` (`robot.go:136-144`) clears the flag and cancels the owner but **leaves the registry entry in place**, so the disable is still performed later by that owner's `finishCamStream`, under the operation lock. `removeRobot` calls the same function (`robot.go:467`).

## Why the body is not byte-comparable, and what the Rust tests do instead

Go re-encodes every frame through `image/jpeg` at quality 50. No Rust JPEG encoder produces the same bytes for the same input image: quantization table selection, Huffman table choice, chroma subsampling defaults, restart markers and the encoder's own rounding all differ between implementations, and none of that is specified by the quality number. The payload bytes are therefore **outside the byte-exact parity contract**, and no test may diff them against a Go capture.

Everything around the payload is inside the contract and is what the Rust tests assert:

- The exact response header, `Content-Type: multipart/x-mixed-replace; boundary=--boundary`, and that it is set only after the feed opens, so both pre-stream error bodies stay sniffed text.
- The exact part header bytes `--boundary\r\nContent-Type: image/jpeg\r\n\r\n`, one per emitted frame, with no trailing CRLF and no closing boundary at the end of the stream.
- That the payload between two part headers parses as a JPEG, rather than that it equals any particular byte string.
- Frame accounting through a fake frame source: bytes and frames are counted before the decode, so an undecodable frame advances both counters and emits no part; a decodable frame advances both and emits exactly one part.
- That a receive error ends the pump rather than looping, and that cancellation and a closed sink also end it, each with its own exit reason.
- That the counters are monotone across a stop and a restart, and that they survive eviction and reconnect.
- That ownership handoff issues enable exactly once per successful claim and disable exactly once per owning release, in that order, with the superseded owner issuing neither.

The camera route itself is Tier C in the early Rust slice, deferred until P4 proper because it needs a JPEG codec that is not yet in `Cargo.lock`. The frame pump underneath it is in the slice and is tested against a fake sink, so the framing and counter assertions above land before the HTTP route does.
