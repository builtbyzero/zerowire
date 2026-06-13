#!/usr/bin/env bash
# tests/usbip_loopback.sh — general-passthrough loopback (simulated vhci).
#
# Spawns the mock sender in `--mode usbip` and points the Linux receiver
# at it in `--mode usbip --simulate-usbip <log>`. The receiver writes a
# transcript of what a real vhci-hcd attach would have done plus one line
# per relayed URB. Asserts:
#
#   * mock sender exits 0,
#   * transcript contains a `sim-attach` line,
#   * transcript contains >= N `sim-urb` lines.
#
# Run with PSK if `--psk` is given as the second arg.

set -euo pipefail

N="${1:-25}"
PSK_FLAG=()
PSK_ARG=""
if [ "${2:-}" = "--psk" ]; then
    PSK_ARG="psk-usbip-$RANDOM"
    PSK_FLAG=(--psk "$PSK_ARG")
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT/desktop-linux"

PORT="$((30000 + RANDOM % 30000))"
LOG_DIR="$(mktemp -d)"
trap 'rm -rf "$LOG_DIR"' EXIT

echo "[usbip_loopback] building..."
cargo build --quiet --bin zerowire-cli --bin zerowire-mock-sender

MOCK_LOG="$LOG_DIR/mock.log"
RX_OUT="$LOG_DIR/rx.log"
TRANSCRIPT="$LOG_DIR/transcript.log"

if [ -n "$PSK_ARG" ]; then
    echo "[usbip_loopback] launching mock --mode usbip --psk ... reports=$N"
    RUST_LOG=info ./target/debug/zerowire-mock-sender \
        --listen "127.0.0.1:$PORT" \
        --mode usbip \
        --psk "$PSK_ARG" \
        --reports "$N" \
        --interval-ms 5 > "$MOCK_LOG" 2>&1 &
else
    echo "[usbip_loopback] launching mock --mode usbip reports=$N"
    RUST_LOG=info ./target/debug/zerowire-mock-sender \
        --listen "127.0.0.1:$PORT" \
        --mode usbip \
        --reports "$N" \
        --interval-ms 5 > "$MOCK_LOG" 2>&1 &
fi
MOCK_PID=$!
for _ in $(seq 1 30); do
    if ss -ltn 2>/dev/null | grep -q ":$PORT "; then break; fi
    sleep 0.1
done

set +e
RUST_LOG=info ./target/debug/zerowire-cli receive \
    --mode usbip \
    --target "127.0.0.1:$PORT" \
    "${PSK_FLAG[@]}" \
    --simulate-usbip "$TRANSCRIPT" > "$RX_OUT" 2>&1
RX_EXIT=$?
set -e

wait "$MOCK_PID" || true

echo "[usbip_loopback] mock log:";    sed 's/^/    | /' "$MOCK_LOG"
echo "[usbip_loopback] receiver log:"; sed 's/^/    | /' "$RX_OUT"
echo "[usbip_loopback] transcript:";   sed 's/^/    | /' "$TRANSCRIPT"

if [ "$RX_EXIT" -ne 0 ]; then
    echo "[usbip_loopback] FAIL — receiver exited $RX_EXIT" >&2
    exit 1
fi
if ! grep -q '^sim-attach' "$TRANSCRIPT"; then
    echo "[usbip_loopback] FAIL — no sim-attach line in transcript" >&2
    exit 1
fi
URBS="$(grep -c '^sim-urb' "$TRANSCRIPT" || true)"
if [ "$URBS" -lt "$N" ]; then
    echo "[usbip_loopback] FAIL — saw $URBS URBs, wanted $N" >&2
    exit 1
fi
echo "[usbip_loopback] PASS — sim-attach + $URBS URBs relayed (wanted $N, psk=${PSK_ARG:-no})"
