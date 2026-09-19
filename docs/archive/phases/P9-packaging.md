# P9: Packaging

## Goal

Turn the server into something installable. The phase is sized L in the master plan, and it is larger than it looks because the Windows shell that makes wire-pod a shippable application is not in the `chipper` codebase at all. It lives in a separate Go repository, roughly nine hundred lines across `cross/podapp` and `cross/win`, so porting `chipper` alone yields no Windows application. That was established during planning and this phase is where the gap is closed.

## Scope

- A Windows application shell inside `wirepod-app`: the tray icon, a named-mutex single-instance guard, a crash dump, the windowed subsystem, and the icon and manifest resources, replacing the separate Go shell.
- The existing Go installer is kept at first and pointed at the Rust executable, since all it does is copy files and write registry and firewall entries.
- A firewall rule for UDP 5353, which the Go installer never added and which mDNS needs.
- An install layout matching `C:\Program Files\wire-pod\chipper`. Because libopus is statically vendored, roughly 29 MB of shipped DLLs, `libopus-0.dll`, libogg, and the MinGW runtime, can be dropped.
- A Linux systemd unit and an aarch64 build for the Jetson.
- A Docker image.

## Exit criteria

- Installing on a snapshot or spare machine works: the tray appears, autostart works, and `is_running` answers within thirty seconds.
- The systemd unit works on Linux.
- The aarch64 build boots on the Jetson.

## Dependencies

P1 through P8, since packaging ships whatever the server does. It also has a documentation dependency worth noting: this is the phase that is expected to choose a cross-platform line-ending normalization for the vendored assets, which until then are stored with the CRLF bytes of the Windows Go checkout.

## Status

Not started.
