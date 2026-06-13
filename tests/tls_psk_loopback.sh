#!/usr/bin/env bash
# tests/tls_psk_loopback.sh — TLS 1.3 PSK end-to-end smoke test.
#
# This is the v0.2 sibling of hid_loopback.sh. It runs the mock sender
# wrapped in TLS (derived from a pairing code via HKDF-SHA256) and points
# the receiver at it, also wrapped in TLS.
#
# The test asserts:
#
#   1. wrong PSK on the receiver -> handshake fails, no reports flow,
#   2. right PSK on the receiver -> handshake succeeds and HID reports flow,
#      proving the TLS path doesn't break the v0.1 fast lane.
#
# No /dev/uinput, no kernel modules, no extra deps beyond cargo + bash.

set -euo pipefail

N="${1:-30}"
PSK="psk-${RANDOM}-pairing"
WRONG="psk-${RANDOM}-wrong"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT/desktop-linux"

LOG_DIR="$(mktemp -d)"
trap 'rm -rf "$LOG_DIR"' EXIT

echo "[tls_psk] building zerowire-cli + zerowire-mock-sender..."
cargo build --quiet --bin zerowire-cli --bin zerowire-mock-sender

# ---------- Phase 1: wrong PSK must fail ----------
PORT_BAD="$((30000 + RANDOM % 30000))"
MOCK_BAD="$LOG_DIR/mock-bad.log"
RX_BAD_LOG="$LOG_DIR/rx-bad.log"
RX_BAD_OUT="$LOG_DIR/rx-bad-out.log"

echo "[tls_psk] phase 1: launching mock with PSK; receiver with WRONG psk..."
RUST_LOG=info ./target/debug/zerowire-mock-sender \
    --listen "127.0.0.1:$PORT_BAD" \
    --psk "$PSK" \
    --reports "$N" \
    --interval-ms 5 > "$MOCK_BAD" 2>&1 &
MOCK_BAD_PID=$!
for _ in $(seq 1 30); do
    if ss -ltn 2>/dev/null | grep -q ":$PORT_BAD "; then break; fi
    sleep 0.1
done

set +e
RUST_LOG=info ./target/debug/zerowire-cli receive \
    --target "127.0.0.1:$PORT_BAD" \
    --psk "$WRONG" \
    --simulate-source "$RX_BAD_LOG" > "$RX_BAD_OUT" 2>&1
RX_BAD_EXIT=$?
set -e

# Kill the mock sender if it's still running (handshake might leave it
# waiting on accept's child loop).
kill "$MOCK_BAD_PID" 2>/dev/null || true
wait "$MOCK_BAD_PID" 2>/dev/null || true

if [ "$RX_BAD_EXIT" -eq 0 ]; then
    echo "[tls_psk] FAIL — receiver succeeded with wrong PSK!" >&2
    sed 's/^/    | /' "$RX_BAD_OUT" >&2
    exit 1
fi
if [ -s "$RX_BAD_LOG" ] && grep -q '^report ' "$RX_BAD_LOG"; then
    echo "[tls_psk] FAIL — receiver logged reports despite wrong PSK!" >&2
    sed 's/^/    | /' "$RX_BAD_LOG" >&2
    exit 1
fi
echo "[tls_psk] phase 1 OK — wrong PSK -> handshake rejected (exit=$RX_BAD_EXIT)"

# ---------- Phase 2: right PSK must succeed ----------
PORT_OK="$((30000 + RANDOM % 30000))"
MOCK_OK="$LOG_DIR/mock-ok.log"
RX_OK_LOG="$LOG_DIR/rx-ok.log"
RX_OK_OUT="$LOG_DIR/rx-ok-out.log"

echo "[tls_psk] phase 2: launching mock + receiver with matching PSK ($N reports)..."
RUST_LOG=info ./target/debug/zerowire-mock-sender \
    --listen "127.0.0.1:$PORT_OK" \
    --psk "$PSK" \
    --reports "$N" \
    --interval-ms 5 > "$MOCK_OK" 2>&1 &
MOCK_OK_PID=$!
for _ in $(seq 1 30); do
    if ss -ltn 2>/dev/null | grep -q ":$PORT_OK "; then break; fi
    sleep 0.1
done

set +e
RUST_LOG=info ./target/debug/zerowire-cli receive \
    --target "127.0.0.1:$PORT_OK" \
    --psk "$PSK" \
    --simulate-source "$RX_OK_LOG" > "$RX_OK_OUT" 2>&1
RX_OK_EXIT=$?
set -e

wait "$MOCK_OK_PID" 2>/dev/null || true

echo "[tls_psk] mock sender log:";    sed 's/^/    | /' "$MOCK_OK"
echo "[tls_psk] receiver stdout/err:"; sed 's/^/    | /' "$RX_OK_OUT"
echo "[tls_psk] receiver report log:"; sed 's/^/    | /' "$RX_OK_LOG"

if [ "$RX_OK_EXIT" -ne 0 ]; then
    echo "[tls_psk] FAIL — receiver exited $RX_OK_EXIT with right PSK" >&2
    exit 1
fi
if ! grep -q '^bind_ack' "$RX_OK_LOG"; then
    echo "[tls_psk] FAIL — no bind_ack over TLS" >&2
    exit 1
fi
REPORTS="$(grep -c '^report ' "$RX_OK_LOG" || true)"
if [ "$REPORTS" -lt "$N" ]; then
    echo "[tls_psk] FAIL — saw $REPORTS reports, wanted $N" >&2
    exit 1
fi
echo "[tls_psk] PASS — wrong PSK rejected; right PSK -> bind_ack + $REPORTS HID reports over TLS"
