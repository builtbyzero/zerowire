#!/usr/bin/env bash
# Bring the usbip-vudc gadget across to the host side via vhci-hcd.
#
# Idempotent: re-running it after a clean teardown rebuilds state; if the
# device is already imported it leaves it alone.
#
# Pairs with setup-gadget.sh (which builds the gadget) and run-e2e.sh
# (which assumes /dev/hidrawN is ready).

set -euo pipefail

log() { printf '[attach-vhci] %s\n' "$*" >&2; }
require_root() {
    if [ "$(id -u)" -ne 0 ]; then
        log "re-execing under sudo"
        exec sudo -E "$0" "$@"
    fi
}

ensure_usbipd() {
    if pgrep -x usbipd >/dev/null; then
        log "usbipd already running"
        return
    fi
    log "starting usbipd in device mode"
    usbipd -e -D
    sleep 0.3
}

attach_if_needed() {
    if usbip port 2>/dev/null | grep -q usbip-vudc.0; then
        log "vhci already has usbip-vudc.0 imported; skipping"
        return
    fi
    log "attaching usbip-vudc.0 via vhci-hcd"
    usbip attach -r 127.0.0.1 -b usbip-vudc.0
}

main() {
    require_root "$@"
    if [ ! -d /sys/class/udc/usbip-vudc.0 ]; then
        log "usbip-vudc.0 UDC not present; run setup-gadget.sh first"
        exit 2
    fi
    # The gadget must have its UDC bound (i.e. setup-gadget.sh ran).
    if [ -z "$(cat /sys/kernel/config/usb_gadget/zw_mouse/UDC 2>/dev/null)" ]; then
        log "zw_mouse gadget has no UDC bound; run setup-gadget.sh first"
        exit 2
    fi
    ensure_usbipd
    attach_if_needed
    sleep 0.5
    log "imported devices:"
    usbip port 2>&1 | sed 's/^/    /'
    log "current /dev/hidrawN devices:"
    for h in /dev/hidraw*; do
        syspath="$(readlink -f /sys/class/hidraw/$(basename "$h")/device)"
        printf '    %s  -> %s\n' "$h" "$syspath"
    done
}

main "$@"
