#!/usr/bin/env bash
# The build that ships: the Vosk engine and the Windows tray.
#
# CI cannot run this, because the libvosk import library is not in the
# repository. Run it before any push that touches the voice path, and always
# before scripts/deploy-windows.ps1. The binary lands at
# $CARGO_TARGET_DIR/release/chipper.exe.
set -euo pipefail

cd "$(dirname "$0")/.."

export VOSK_LIB_DIR="${VOSK_LIB_DIR:-$PWD/spikes/s2-vosk/vendor/vosk-win64-0.3.45}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-E:/GitHub/wire-pod-rs-target-gate}"
export CARGO_INCREMENTAL=0

if [ ! -f "$VOSK_LIB_DIR/libvosk.lib" ]; then
    echo "gate-packaged: no libvosk.lib under $VOSK_LIB_DIR; set VOSK_LIB_DIR" >&2
    exit 1
fi

cargo clippy -j 4 -p wirepod-app --all-targets --features stt-vosk,tray -- -D warnings
cargo build -j 4 --release -p wirepod-app --features stt-vosk,tray
echo "gate-packaged: built $CARGO_TARGET_DIR/release/chipper.exe"
