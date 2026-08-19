# RUNBOOK — S1 live robot TLS handshake test

Goal: prove the real robot (ESN 00303f28, 192.168.8.203) completes its
conn-check against the Rust listener stack serving the escape-pod cert.
The robot is offline for ~5 minutes during this test.

Prereqs: elevated PowerShell (ports 80/443 + firewall), robot awake and on
the same LAN, this repo built (`cargo build -p s1-tls-listener`).

## Steps

1. **Exit the WirePod tray app** (system tray icon → Quit). The tray app
   supervises chipper.exe and may respawn it if you only kill the process.
2. In an **elevated** PowerShell, confirm the Go server is gone:
   `Get-Process chipper -ErrorAction SilentlyContinue` → nothing.
   If still running: `Stop-Process -Name chipper -Force`.
3. Windows Firewall: the spike needs inbound 80/443 TCP + 5353 UDP. The
   wire-pod install rules may already cover the ports for chipper.exe only —
   for the test, temporarily allow the spike binary when the firewall prompt
   appears, or add:
   `netsh advfirewall firewall add rule name="s1-spike" dir=in action=allow program="<repo>\target\debug\s1-tls-listener.exe" enable=yes`
4. From the repo root, in the elevated shell:
   `cargo run -p s1-tls-listener -- --tls-port 443 --http-port 80 --mdns --alpn on`
5. Watch the output for ~2 minutes. PASS criteria:
   - `[mdns] registered escapepod ...` at startup
   - `[hit] /ok` or `[hit] /ok:80` lines from the robot's IP (step-1 plain
     HTTP conncheck, may take up to a minute — the robot re-resolves
     escapepod.local via mDNS)
   - `[grpc] connection check from device 00303f28 ...` followed by
     `connection check done: Success`
   - `netstat -ano | findstr 192.168.8.203` shows an ESTABLISHED connection
   Nudge: putting the robot on/off the charger or rebooting it forces a
   fresh conn-check.
6. Ctrl+C the spike, rerun with `--alpn off`, repeat the checks (proves the
   preface-sniffing path the Go cmux uses today).
7. **Restart the Go server**: launch the WirePod app
   (`C:\Program Files\wire-pod\WirePod.exe` or the Start Menu entry), wait
   ~30 s, then verify `curl http://localhost:8080/api/is_running` → `true`
   and the robot reconnects (netstat shows the :80 heartbeat again).
8. Remove the temporary firewall rule if added:
   `netsh advfirewall firewall delete rule name="s1-spike"`

## FAIL handling

- Handshake failures logged as `[tls] handshake ... failed`: record the error;
  if both ALPN modes fail, the tls-native fallback plan (plan §risks R1)
  activates before Phase 1 proceeds.
- No mDNS hits: check UDP 5353 firewall, `PRINT_MDNS`-style debugging via
  `dns-sd -B _app-proto._tcp` from another machine.
