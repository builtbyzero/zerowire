#!/usr/bin/env bash
# tests/hid_loopback.sh — end-to-end smoke test for the HID fast lane.
#
# Spawns the mock sender (zerowire-mock-sender) and points the Linux
# receiver at it in `--simulate-source` mode (which logs reports to a
# file instead of pushing them through /dev/uinput, so this test does
# not need root or the `uinput` group).
#
# Passes when:
#   * the mock sender exits 0,
#   * the receiver's log shows a bind_ack line, AND
#   * the receiver's log shows at least N report lines.
#
# Usage: tests/hid_loopback.sh [N=50]
#
# Wire into CI: this script must be runnable from a bare checkout with
# only cargo + a working network stack. No /dev/uinput, no extra deps.

set -euo pipefail

N="${1:-50}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT/desktop-linux"

PORT="$((30000 + RANDOM % 30000))"
LOG_DIR="$(mktemp -d)"
trap 'rm -rf "$LOG_DIR"' EXIT

echo "[hid_loopback] building zerowire-cli + zerowire-mock-sender..."
cargo build --quiet --bin zerowire-cli --bin zerowire-mock-sender

MOCK_LOG="$LOG_DIR/mock.log"
RX_LOG="$LOG_DIR/rx-reports.log"
RX_OUT="$LOG_DIR/rx.log"

echo "[hid_loopback] starting mock sender on 127.0.0.1:$PORT (reports=$N)..."
RUST_LOG=info ./target/debug/zerowire-mock-sender \
    --listen "127.0.0.1:$PORT" \
    --reports "$N" \
    --interval-ms 5 > "$MOCK_LOG" 2>&1 &
MOCK_PID=$!
# Give the listener a moment to bind.
for _ in $(seq 1 30); do
    if ss -ltn 2>/dev/null | grep -q ":$PORT "; then
        break
    fi
    sleep 0.1
done

echo "[hid_loopback] starting receiver (simulate-source -> $RX_LOG)..."
set +e
RUST_LOG=info ./target/debug/zerowire-cli receive \
    --target "127.0.0.1:$PORT" \
    --simulate-source "$RX_LOG" > "$RX_OUT" 2>&1
RX_EXIT=$?
set -e

wait "$MOCK_PID" || true

echo "[hid_loopback] mock sender log:"; sed 's/^/    | /' "$MOCK_LOG"
echo "[hid_loopback] receiver stdout/stderr:"; sed 's/^/    | /' "$RX_OUT"
echo "[hid_loopback] receiver report log:";   sed 's/^/    | /' "$RX_LOG"

if [ "$RX_EXIT" -ne 0 ]; then
    echo "[hid_loopback] FAIL — receiver exited $RX_EXIT" >&2
    exit 1
fi

if ! grep -q '^bind_ack' "$RX_LOG"; then
    echo "[hid_loopback] FAIL — receiver never saw bind_ack" >&2
    exit 1
fi

REPORTS_SEEN="$(grep -c '^report ' "$RX_LOG" || true)"
if [ "$REPORTS_SEEN" -lt "$N" ]; then
    echo "[hid_loopback] FAIL — saw $REPORTS_SEEN reports, expected $N" >&2
    exit 1
fi

echo "[hid_loopback] PASS — bind_ack + $REPORTS_SEEN reports (wanted $N)"
