# RUNBOOK — SDK-app trial against the real robot

Goal: run the ported SDK-app HTTP surface beside the production Go server, send
the same requests to both, and diff the answers. Nothing is stopped and nothing
is taken offline. The trial binds 18080, which nothing else uses, and the Go
server keeps serving 80, 443, 8080 and 8084 throughout.

This is the first time the ported handlers see a real socket and a real robot.
Every test in `wirepod-server` drives the router through `tower`'s `oneshot`
against a `FakeConnFactory`, so three of the questions in
`docs/phases/P4-sdk-app/deviations.md` can only be answered here.

## What this proves, and what it does not

It proves, at the level of curl:

- The router answers over HTTP/1.1 on a bound socket, and not only through a
  `oneshot` call.
- The connect preamble dials the real robot at 192.168.8.203:443 with the GUID
  from the bot-info file, and the four routes behind a connected robot work
  against real gRPC rather than against a fake.
- Status, the nine headers this surface carries, and the body bytes match the Go
  server for every low-side-effect route in the slice.
- The event stream opens against the robot, publishes real stim readings, and
  stops.
- `disconnect` pays its three second settle and a `conn_test` straight after
  dials again.

It does not prove:

- Anything about the dashboard. No page is loaded; the trial serves no static
  files, because the file server is P4 work and the fallback answers every path
  with the 404 a missing file gets.
- `/cam-stream`. The route does not exist yet. `begin_cam_stream` and
  `stop_cam_stream` are in the diff, but they are the two cheap routes either
  side of a feed that is not there.
- TLS, port 80, mDNS, the jdocs pinger, the tonic services, or the restart
  supervisor. All of those are P1, and `RUNBOOK-S1.md` is the procedure for the
  TLS and mDNS half.

## Prerequisites

1. The Go server is up and healthy: `curl -s http://localhost:8080/api/is_running`
   prints `true`. If it is not, launch the WirePod tray app and wait about
   thirty seconds.
2. The robot is awake, on this LAN, and connected to the Go server:
   `curl -s http://localhost:8080/api/get_bot_status` shows `"status":"online"`
   for ESN 00303f28.
3. The bot-info file exists at
   `%APPDATA%\wire-pod\jdocs\botSdkInfo.json`. The Go server writes it as robots
   authenticate, so a robot that has connected at least once has one.
4. The repo builds: `cargo build -p wirepod-app`.

## Running it

Two Git Bash windows. The first serves:

```bash
cargo run -p wirepod-app -- sdk-trial
```

It prints one line naming the address, the number of robots in the file, and the
file, and then logs. No GUID is ever printed, by the binary or by the crates
under it. The defaults are loopback, port 18080, the bot-info file under
`%APPDATA%`, and no connect-time liveness deadline, which is what the Go server
does. `--bind`, `--port`, `--bot-info` and `--liveness-deadline-ms` override
them, and `RUST_LOG` overrides the log filter, which defaults to
`info,wirepod_core=debug,wirepod_vector=debug,wirepod_server=debug`.

The second window diffs:

```bash
bash scripts/sdk-trial-diff.sh
```

`GO_BASE`, `RS_BASE` and `SERIAL` override the defaults
(`http://localhost:8080`, `http://127.0.0.1:18080`, `00303f28`). The whole run
takes about fifteen seconds, five of which are the stim window and three the
disconnect settle.

To test the script itself without the Rust server, point both bases at the Go
server:

```bash
GO_BASE=http://localhost:8080 RS_BASE=http://localhost:8080 bash scripts/sdk-trial-diff.sh
```

Everything should then report `match`, because both sides are the same process.

## The stim window

Partway through, the script prints

```
>>> PET THE ROBOT ON ITS BACK FOR THE NEXT FIVE SECONDS <<<
```

and then reads `get_stim_status` five times, a second apart. Do it: put a hand
on the robot's back and keep it there for the five seconds. Stim is the robot's
own stimulation level and it sits at or near zero when nothing is happening, so
a run of five zeroes is a weaker result than a run of rising numbers. It says
the readings arrived; it does not say the numbers render correctly.

What to look for in those five lines:

- A bare JSON number with no quotes and no newline: `0`, `0.1`, `0.75`, `1`.
  That is Go's `%v` on a `float32`, which `go_format_f32` reproduces.
- Never `0.0`, never `1.0`, never a long fraction. Any of those is a formatting
  bug and the dashboard's stim graph would show it.
- Five copies of `error: must start event stream` mean the stream never opened.
  Read the trial window: `stim::begin` logs `event stream: <err>` when the open
  fails, and that log is the only place the reason appears.

## Reading the output

Each request prints a block:

```
[07] /api-sdk/get_stim_status?serial=00303f28   (compare)
  field                          GO                    | RS
  date                           <masked>              | <masked>
  status                         200                   | 200
  ...
  VERDICT: match
```

`date` is printed masked and never compared. Every other row is compared; when
the two sides differ the full values are printed under the row prefixed `!= GO:`
and `!= RS:`, so nothing is hidden by the column width.

The four kinds in brackets:

- `compare` — a difference is a failure and sets the exit status to 1.
- `expected` — compared and printed, and a difference is explained in the note
  rather than counted as a failure. There is one: `/api/get_bot_status`.
- `observe` — the value is live, so the two sides are printed and not compared.
  These are the five stim samples and `get_sdk_info`.
- `go-only` — `/api/is_running`, which the ported slice does not serve.

The summary counts all four and the script exits 0 only when every `compare`
request matched.

## The three open questions this closes

`deviations.md` ends with a list of things nothing in the slice can settle.
This run settles three of them, and the answers belong in that list afterwards.

1. **Whether `ProtocolVersion(client_version = 5, min_host_version = 0)` answers
   `SUCCESS` or `UNSUPPORTED` on this robot** (open question 2). No response
   body can tell you: every caller discards the verdict on purpose, which is the
   whole point of using this RPC for the probe. It is logged instead. In the
   trial window, each `net_probe` produces a line of the form

   ```
   DEBUG wirepod_vector::conn: protocol version verdict, which every caller discards result=Unsupported host_version=...
   ```

   Read `result` and `host_version` and write both down. This is why the default
   filter turns `wirepod_vector` up to debug.

2. **The real round trip** (open question 3). The roughly 14 millisecond figure
   that motivated choosing `ProtocolVersion` over `BatteryState` comes from a Go
   source comment, not from a measurement. The script runs `net_probe` three
   times and prints the raw `rttMs` from both servers under each one:

   ```
   raw rttMs   GO: 13.482   RS: 14.1
   ```

   Three samples from each side is enough to say whether the figure is real and
   whether the two implementations time the same thing.

3. **Whether a non-zero stim reading renders the way the dashboard needs**
   (implied by the stim work, and untestable without a robot being touched).
   The five samples above are the answer.

## Expected differences

These are not failures. Everything else is.

- **`rttMs` digits.** Two separate processes time two separate round trips.
  The script rewrites the digits to `<n>` before comparing the body and prints
  the raw figures separately.
- **`Date`.** Different by construction. Printed as `<masked>` and never
  compared.
- **`/api/get_bot_status`.** The Rust side reports `"status":"disconnected"`
  and `"timesince":-1` where Go reports a live status, because nothing drives
  the Rust jdocs pinger yet. That is deviation 2: `/ok` does the pinger
  bookkeeping in Go and the port defers it to P1. This is the one request marked
  `expected`. Any difference in it beyond `status` and `timesince` is real.
- **The wording after `desc = ` in a dial failure, and only when the robot is
  off or unreachable.** That is deviation 23. Both sides answer HTTP 200 with
  `error: rpc error: code = Unavailable desc = ` and then diverge: grpc-go
  writes `connection error: desc = "transport: Error while dialing: ..."` and
  tonic writes a `tcp connect error` chain. The Windows sentence sits inside
  both. With the robot on, neither side dials anything that fails and the
  difference does not appear, which is why the script does not special-case it.

## Stopping

Ctrl-C in the trial window. The binary logs `sdk-trial: Ctrl-C, shutting down`,
stops accepting, lets any request already in flight finish, and exits 0. A
`disconnect` parked in its three second settle is one of those, so a Ctrl-C
during one takes up to three seconds.

Nothing needs restarting afterwards. The Go server was never touched.

## A note on two SDK connections

While the trial runs, the robot holds two SDK connections at once: the Go
server's and the Rust process's. Both authenticate with the same GUID from the
same bot-info file, and both open an event stream during the stim window. The
robot allows this; it is the same thing that happens when a Python SDK script
runs beside wire-pod.

Two consequences are worth knowing before they look like bugs. The one
`disconnect` near the end of the script drops the Go server's connection as well
as the Rust one, and the Go server dials again on its next request, which the
script makes immediately. And an event stream claimed on one server is invisible
to the other, so the ownership state machine each side runs is its own; two
`begin_event_stream` calls are two streams, not one contested claim.
