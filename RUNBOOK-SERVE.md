# RUNBOOK: run the Rust server against the real robot

Goal: put Vector (ESN 00303f28, 192.168.8.203) on the Rust server instead of the Go one for a while, then put him back. This is the procedure that was used on 2026-09-19, when he first completed a TLS handshake with the Rust listener and read his jdocs from it.

The Go server and the Rust server cannot run at the same time in this mode, because both want ports 80, 443, 8080 and 8084 and the mDNS name `escapepod`. While the Rust server is up, voice commands do not work, because the speech pipeline is milestone M3.

## Switch to Rust

1. Quit the WirePod tray app from its tray icon. It supervises `chipper.exe`, so killing only the process is not enough. Check in Task Manager that `chipper.exe` is gone.
2. Once only, in an elevated PowerShell, allow the Rust binary through Windows Firewall. Windows allows inbound connections per program, and the installed rule covers only the Go binary.
   `netsh advfirewall firewall add rule name="wirepod-rs-test" dir=in action=allow program="E:\GitHub\wire-pod-rs\target\debug\chipper.exe" enable=yes`
3. Make a working copy of the state directory, so the live state stays untouched. From Git Bash:
   `cp -r "$APPDATA/wire-pod/." <scratch>/data/`
4. Build and start the server from the repo root:
   `cargo build -p wirepod-app`
   `./target/debug/chipper.exe serve --data-dir <scratch>/data --asset-dir assets --sdk-ini-dir <scratch>/ini`
   With `--packaged` instead of `--data-dir` it uses the live `%APPDATA%\wire-pod`, which is what a real cutover will do.

## What a healthy start looks like

Within a few seconds the log shows the four listeners, `Registering escapepod.local on network`, then from the robot:

```
tls connection from 192.168.8.203:<port>
192.168.8.203:<port> POST /jdocspb.Jdocs/ReadDocs
pulled jdocs bot=00303f28
```

`netstat -ano | findstr 192.168.8.203` shows his heartbeat connection to port 80 and the server's own connection to his port 443. `ping escapepod.local` from this PC answers with this machine's address.

The build output is locked while the server runs. To build or test in the meantime, use another output folder: `CARGO_TARGET_DIR=E:/GitHub/wire-pod-rs-target-gate cargo test`.

## Switch back to Go

1. Stop the Rust server with Ctrl-C, or `taskkill /F /IM chipper.exe`.
2. Launch WirePod from the Start Menu, wait about thirty seconds, and check `curl http://localhost:8080/api/is_running` answers `true`.
3. The firewall rule can stay for next time. To remove it: `netsh advfirewall firewall delete rule name="wirepod-rs-test"`.

## Web UI only, beside the Go server

To look at the Rust web UI without taking Vector off the Go server, serve HTTP only on other ports:

`./target/debug/chipper.exe serve --web-only --bind 127.0.0.1 --web-port 18080 --http-port 18082 --data-dir <scratch>/data --asset-dir assets --sdk-ini-dir <scratch>/ini`

`--web-only` starts no TLS listener, does not bind 8084 and registers nothing on mDNS.
