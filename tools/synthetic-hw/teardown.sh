#!/usr/bin/env bash
# Unwind everything setup-gadget.sh + attach-vhci.sh did.
# Safe to run even when nothing is set up — it skips missing pieces.

set -uo pipefail

NAME="${NAME:-zw_mouse}"
GADGET_DIR="/sys/kernel/config/usb_gadget/${NAME}"

log() { printf '[teardown] %s\n' "$*" >&2; }
require_root() {
    if [ "$(id -u)" -ne 0 ]; then
        log "re-execing under sudo"
        exec sudo -E "$0" "$@"
    fi
}

main() {
    require_root "$@"

    # 1. detach all vhci ports we own (just one, but be defensive)
    if command -v usbip >/dev/null; then
        while read -r portline; do
            port="$(echo "$portline" | awk '{print $2}' | tr -d ':')"
            if [ -n "$port" ]; then
                log "detaching vhci port $port"
                usbip detach -p "$port" || true
            fi
        done < <(usbip port 2>/dev/null | grep '^Port ' || true)
    fi

    # 2. stop usbipd
    if pgrep -x usbipd >/dev/null; then
        log "stopping usbipd"
        pkill -x usbipd || true
        sleep 0.2
    fi

    # 3. unbind UDC, then dismantle the configfs gadget.
    if [ -d "$GADGET_DIR" ]; then
        if [ -s "$GADGET_DIR/UDC" ]; then
            log "unbinding UDC"
            echo "" > "$GADGET_DIR/UDC" 2>/dev/null || true
        fi
        for c in "$GADGET_DIR/configs/"*/; do
            [ -d "$c" ] || continue
            for f in "$c"*; do
                if [ -L "$f" ]; then rm -f "$f"; fi
            done
            for s in "$c"strings/*; do
                [ -d "$s" ] && rmdir "$s" 2>/dev/null || true
            done
            rmdir "$c" 2>/dev/null || true
        done
        for f in "$GADGET_DIR/functions/"*; do
            [ -d "$f" ] && rmdir "$f" 2>/dev/null || true
        done
        for s in "$GADGET_DIR/strings/"*; do
            [ -d "$s" ] && rmdir "$s" 2>/dev/null || true
        done
        rmdir "$GADGET_DIR" 2>/dev/null || true
        log "removed $GADGET_DIR"
    fi

    log "done"
}

main "$@"
