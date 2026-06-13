#!/usr/bin/env bash
# End-to-end synthetic hardware verification.
#
# 1. Ensure the usbip-vudc gadget + vhci-hcd loopback is up.
# 2. Start zerowire-linux-sender (reads /dev/hidrawN).
# 3. Start zerowire-cli receive --diagnose (creates uinput device).
# 4. Inject a deterministic mouse-report sequence via /dev/hidg0.
# 5. Capture evtest output proving uinput events fired.
# 6. Print a verdict; exit non-zero on any missing pass criterion.
#
# Output artefacts (under $OUT_DIR, default /tmp/zw-synth):
#   diagnose.jsonl  zerowire's --diagnose trace
#   sender.log      linux-sender stderr
#   receiver.log    zerowire-cli stderr
#   evtest.log      evtest -t 6 dump from the synthetic uinput device
#   verdict         PASS / FAIL + reasons

set -uo pipefail

# Where we run from. Use the workspace root so cargo + tools paths resolve.
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

OUT_DIR="${OUT_DIR:-/tmp/zw-synth}"
mkdir -p "$OUT_DIR"
> "$OUT_DIR/diagnose.jsonl"

LISTEN_ADDR="${LISTEN_ADDR:-127.0.0.1:47823}"
SENDER_BIN="$REPO_ROOT/desktop-linux/target/debug/zerowire-linux-sender"
RECEIVER_BIN="$REPO_ROOT/desktop-linux/target/debug/zerowire-cli"

log() { printf '[run-e2e] %s\n' "$*" >&2; }
fail() { log "FAIL: $*"; echo "FAIL: $*" >> "$OUT_DIR/verdict"; exit 1; }

cleanup() {
    if [ -n "${RECEIVER_PID:-}" ]; then sudo -n kill "$RECEIVER_PID" 2>/dev/null || true; fi
    if [ -n "${SENDER_PID:-}" ]; then kill "$SENDER_PID" 2>/dev/null || true; fi
    wait 2>/dev/null
}
trap cleanup EXIT

# Pre-flight: binaries built?
[ -x "$SENDER_BIN" ]   || fail "sender binary missing — run: cd desktop-linux && cargo build"
[ -x "$RECEIVER_BIN" ] || fail "receiver binary missing — run: cd desktop-linux && cargo build"

# Pre-flight: gadget loopback up?
HIDRAW=""
for h in /dev/hidraw*; do
    syspath="$(readlink -f "/sys/class/hidraw/$(basename "$h")/device" 2>/dev/null)" || continue
    # The synthetic device's USB ID is badd:c0de (set by setup-gadget.sh).
    if [[ "$syspath" == *"BADD:C0DE"* ]]; then
        HIDRAW="$h"
        break
    fi
done
if [ -z "$HIDRAW" ]; then
    fail "no synthetic /dev/hidrawN found — run: sudo tools/synthetic-hw/setup-gadget.sh && sudo tools/synthetic-hw/attach-vhci.sh"
fi
log "synthetic hidraw: $HIDRAW"

# Make it readable for the sender (which runs as our user).
sudo -n chmod a+r "$HIDRAW"
# And /dev/hidg0 writable so drive-mouse.py works as our user.
sudo -n chmod a+w /dev/hidg0 2>/dev/null || true

# Start linux-sender.
log "starting linux-sender on $LISTEN_ADDR"
"$SENDER_BIN" \
    --filter-vidpid badd:c0de \
    --listen "$LISTEN_ADDR" \
    --once \
    > "$OUT_DIR/sender.log" 2>&1 &
SENDER_PID=$!
sleep 0.3
if ! kill -0 "$SENDER_PID" 2>/dev/null; then
    cat "$OUT_DIR/sender.log" >&2
    fail "linux-sender exited before receiver could connect"
fi

# Start the receiver. Needs /dev/uinput (root by default on this distro).
log "starting receiver (sudo for /dev/uinput) with --diagnose $OUT_DIR/diagnose.jsonl"
sudo -n -E "$RECEIVER_BIN" receive \
    --target "$LISTEN_ADDR" \
    --diagnose "$OUT_DIR/diagnose.jsonl" \
    > "$OUT_DIR/receiver.log" 2>&1 &
RECEIVER_PID=$!

# Wait for the receiver to publish its uinput device. We scan
# /sys/class/input/eventN/device/name — the receiver-side device is named
# "zerowire: <product>" (prefix added by receiver.rs::open_device), which
# distinguishes it from the *kernel-side* gadget device, which is the
# original "builtbyzero zerowire synthetic mouse".
DEVNODE=""
for _ in $(seq 1 40); do
    sleep 0.1
    for f in /sys/class/input/event*/device/name; do
        [ -f "$f" ] || continue
        name="$(cat "$f" 2>/dev/null || true)"
        # Match the "zerowire: " prefix exactly — that's the receiver's
        # uinput device, not the kernel-side gadget mouse.
        if [[ "$name" == zerowire:* ]]; then
            DEVNODE="$(basename "$(dirname "$(dirname "$f")")")"
            break 2
        fi
    done
done
if [ -z "$DEVNODE" ]; then
    log "receiver log:"
    sed 's/^/  | /' "$OUT_DIR/receiver.log" >&2
    fail "no zerowire uinput device appeared within ~4s"
fi
log "uinput device: /dev/input/$DEVNODE"

# Start evtest in the background, capturing 6 seconds.
log "capturing evtest on /dev/input/$DEVNODE for ~6s"
( sudo -n timeout 6 evtest --grab "/dev/input/$DEVNODE" > "$OUT_DIR/evtest.log" 2>&1 ) &
EVTEST_PID=$!
sleep 0.6   # let evtest grab the device

# Drive the synthetic mouse.
log "driving 9 mouse reports through /dev/hidg0"
python3 tools/synthetic-hw/drive-mouse.py --scenario smoke --interval-ms 30 \
    >> "$OUT_DIR/sender.log" 2>&1

# Let evtest catch up.
wait $EVTEST_PID 2>/dev/null || true

# Stop receiver and sender gracefully.
sudo -n kill "$RECEIVER_PID" 2>/dev/null || true
kill "$SENDER_PID" 2>/dev/null || true
sleep 0.3

# ---------- pass/fail evaluation ----------

verdict_pass() { echo "PASS: $*"; echo "PASS: $*" >> "$OUT_DIR/verdict"; }
verdict_fail() { echo "FAIL: $*"; echo "FAIL: $*" >> "$OUT_DIR/verdict"; FAILED=1; }

FAILED=0
> "$OUT_DIR/verdict"

# 1. bind_ack appeared in diagnose log.
if grep -q '"event":"bind_ack"' "$OUT_DIR/diagnose.jsonl"; then
    verdict_pass "bind_ack present in diagnose JSONL"
else
    verdict_fail "bind_ack missing from diagnose JSONL"
fi

# 2. At least one HID report seen.
REPORT_LINES="$(grep -c '"event":"report"' "$OUT_DIR/diagnose.jsonl" || true)"
if [ "${REPORT_LINES:-0}" -ge 1 ]; then
    verdict_pass "received $REPORT_LINES report events"
else
    verdict_fail "no report events in diagnose JSONL"
fi

# 3. uinput device existed (we already checked, but reconfirm via /proc).
if grep -q 'zerowire' /proc/bus/input/devices 2>/dev/null; then
    verdict_pass "uinput device 'zerowire: ...' visible in /proc/bus/input/devices"
else
    # The receiver tears down its uinput device on exit, so this is "best-effort".
    # We already captured the device above.
    verdict_pass "uinput device existed during the run (captured by evtest)"
fi

# 4. evtest log contains relative motion + button events.
if grep -qE '(REL_X|REL_Y)' "$OUT_DIR/evtest.log" && grep -q BTN_LEFT "$OUT_DIR/evtest.log"; then
    verdict_pass "evtest saw EV_REL motion + BTN_LEFT (real uinput event delivery)"
else
    verdict_fail "evtest missing motion or button events (see $OUT_DIR/evtest.log)"
fi

# 5. report_descriptor non-empty in bind_ack.
if grep -q '"report_descriptor_len":[1-9]' "$OUT_DIR/diagnose.jsonl"; then
    verdict_pass "bind_ack carried a non-empty report descriptor"
else
    verdict_fail "bind_ack report_descriptor_len is 0"
fi

echo "---"
echo "Artefacts:    $OUT_DIR/"
echo "  diagnose:   $OUT_DIR/diagnose.jsonl"
echo "  evtest:     $OUT_DIR/evtest.log"
echo "  sender:     $OUT_DIR/sender.log"
echo "  receiver:   $OUT_DIR/receiver.log"
echo "Verdict:      $OUT_DIR/verdict"

if [ "$FAILED" -eq 0 ]; then
    echo "OVERALL:      PASS"
    exit 0
else
    echo "OVERALL:      FAIL"
    exit 1
fi
