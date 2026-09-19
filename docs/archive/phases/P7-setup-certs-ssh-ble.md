# P7: Setup: certs, SSH, BLE

## Goal

Port the onboarding path, which is how a robot that is not yet talking to wire-pod is brought onto it. The phase is sized M in the master plan. It is separated out and placed late because the live installation is already onboarded, so nothing here is on the path to cutover; it matters for a second robot or a factory reset.

## Scope

- Certificate generation, producing 2048-bit keys from now on. The `ring` backend refuses anything below 2048, so a preflight check logs an error for a legacy sub-2048 key rather than failing obscurely at handshake time. The existing escape-pod key is already RSA-2048 and is unaffected.
- The `server_config.json` writer, byte-exact against what Go produces.
- SSH onboarding over russh, uploading the vendored `pod-bot-install.sh` and using the existing GitHub-download fallback for the `vic-cloud` binary, with a `--vic-cloud-path` override.
- The `/api-ssh/*` routes and `/api/generate_certs`.
- Bluetooth Low Energy onboarding behind `--features ble`, Linux first and off by default.

## Exit criteria

- A factory-reset or spare robot is onboarded end to end.
- The generated `server_config.json` matches the Go server's byte for byte.

## Dependencies

P0 and P1. Onboarding writes the same state files P1 reads, and the certificates it generates are what P1's listener serves.

## Status

Not started. The BLE default is deliberate parity with the live installation, which was verified during planning: BLE sits behind Go's `inbuiltble` build tag and is not in the packaged Windows build, so the seventeen `/api-ble/*` routes are stubs there today.
