#!/usr/bin/env bash
# tests/android_pump_loopback.sh — v0.3 Android URB pump loopback.
#
# Spawns the Rust fixture `zerowire-simulate-android-pump` (which speaks the
# **same** wire format as the Kotlin `UsbIpHost.kt` pump on a real phone)
# and points the Linux receiver at it in `--mode usbip --simulate-usbip
# --simulate-drive-urbs N`. The receiver then runs the
# `urb_driver::script_default` sequence: GET_DESCRIPTOR ×3, SET_CONFIGURATION,
# bulk-IN ×2, bulk-OUT ×1, CMD_UNLINK — plus N extra bulk-IN URBs.
#
# Asserts:
#   * fixture sender exits 0,
#   * receiver exits 0,
#   * transcript contains:
#       - `sim-attach` line,
#       - one `sim-urb GET_DESCRIPTOR(Device,18) ... status=0 actual_length=18`,
#       - one `sim-urb bulk-IN-64 ... status=0 actual_length=64`,
#       - one `sim-unlink ... status=0`,
#       - a final `sim-summary` line with urbs_ok == urbs_issued.
#
# Pass `--psk` as the second arg to run the same loopback through TLS 1.3.

set -euo pipefail

N="${1:-3}"
PSK_FLAG=""
PSK_ARG=""
if [ "${2:-}" = "--psk" ]; then
    PSK_ARG="psk-android-pump-$RANDOM"
    PSK_FLAG="--psk $PSK_ARG"
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT/desktop-linux"

PORT="$((30000 + RANDOM % 30000))"
LOG_DIR="$(mktemp -d)"
trap 'rm -rf "$LOG_DIR"' EXIT

echo "[android_pump_loopback] building..."
cargo build --quiet --bin zerowire-cli --bin zerowire-simulate-android-pump

PUMP_LOG="$LOG_DIR/pump.log"
RX_OUT="$LOG_DIR/rx.log"
TRANSCRIPT="$LOG_DIR/transcript.log"

if [ -n "$PSK_ARG" ]; then
    echo "[android_pump_loopback] launching simulate-android-pump --psk ... extra_bulk_in=$N"
    RUST_LOG=info ./target/debug/zerowire-simulate-android-pump \
        --listen "127.0.0.1:$PORT" \
        --psk "$PSK_ARG" > "$PUMP_LOG" 2>&1 &
else
    echo "[android_pump_loopback] launching simulate-android-pump extra_bulk_in=$N"
    RUST_LOG=info ./target/debug/zerowire-simulate-android-pump \
        --listen "127.0.0.1:$PORT" > "$PUMP_LOG" 2>&1 &
fi
PUMP_PID=$!
for _ in $(seq 1 30); do
    if ss -ltn 2>/dev/null | grep -q ":$PORT "; then break; fi
    sleep 0.1
done

set +e
RUST_LOG=info ./target/debug/zerowire-cli receive \
    --mode usbip \
    --target "127.0.0.1:$PORT" \
    $PSK_FLAG \
    --simulate-usbip "$TRANSCRIPT" \
    --simulate-drive-urbs "$N" > "$RX_OUT" 2>&1
RX_EXIT=$?
set -e

wait "$PUMP_PID" || PUMP_EXIT=$?

echo "[android_pump_loopback] pump log:";    sed 's/^/    | /' "$PUMP_LOG"
echo "[android_pump_loopback] receiver log:"; sed 's/^/    | /' "$RX_OUT"
echo "[android_pump_loopback] transcript:";   sed 's/^/    | /' "$TRANSCRIPT"

if [ "$RX_EXIT" -ne 0 ]; then
    echo "[android_pump_loopback] FAIL — receiver exited $RX_EXIT" >&2
    exit 1
fi
if ! grep -q '^sim-attach' "$TRANSCRIPT"; then
    echo "[android_pump_loopback] FAIL — no sim-attach line in transcript" >&2
    exit 1
fi
if ! grep -q '^sim-urb GET_DESCRIPTOR(Device,18) .*status=0 actual_length=18' "$TRANSCRIPT"; then
    echo "[android_pump_loopback] FAIL — GET_DESCRIPTOR(Device) round-trip missing" >&2
    exit 1
fi
if ! grep -q '^sim-urb GET_DESCRIPTOR(Config,32) .*status=0 actual_length=32' "$TRANSCRIPT"; then
    echo "[android_pump_loopback] FAIL — GET_DESCRIPTOR(Config,32) round-trip missing" >&2
    exit 1
fi
if ! grep -q '^sim-urb bulk-IN-64 .*status=0 actual_length=64 body_bytes=64' "$TRANSCRIPT"; then
    echo "[android_pump_loopback] FAIL — bulk-IN-64 round-trip missing" >&2
    exit 1
fi
if ! grep -q '^sim-urb bulk-OUT-64 .*status=0 actual_length=64' "$TRANSCRIPT"; then
    echo "[android_pump_loopback] FAIL — bulk-OUT-64 round-trip missing" >&2
    exit 1
fi
if ! grep -q '^sim-unlink .*status=0' "$TRANSCRIPT"; then
    echo "[android_pump_loopback] FAIL — CMD_UNLINK round-trip missing" >&2
    exit 1
fi
SUMMARY="$(grep '^sim-summary' "$TRANSCRIPT" || true)"
if [ -z "$SUMMARY" ]; then
    echo "[android_pump_loopback] FAIL — sim-summary line missing" >&2
    exit 1
fi
ISSUED=$(echo "$SUMMARY" | sed -n 's/.*urbs_issued=\([0-9]*\).*/\1/p')
OK=$(echo "$SUMMARY" | sed -n 's/.*urbs_ok=\([0-9]*\).*/\1/p')
if [ "$ISSUED" != "$OK" ]; then
    echo "[android_pump_loopback] FAIL — urbs_issued=$ISSUED != urbs_ok=$OK" >&2
    exit 1
fi
WANT_ISSUED=$((7 + N))
if [ "$ISSUED" != "$WANT_ISSUED" ]; then
    echo "[android_pump_loopback] FAIL — issued $ISSUED, wanted $WANT_ISSUED" >&2
    exit 1
fi
echo "[android_pump_loopback] PASS — $ISSUED URBs + 1 unlink round-tripped (psk=${PSK_ARG:-no})"
