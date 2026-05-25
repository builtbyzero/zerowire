#!/usr/bin/env bash
# Build a USB HID-mouse gadget on top of usbip-vudc, then attach it via
# vhci-hcd so the host kernel enumerates it as a real USB device.
#
# Result: /dev/hidg0 (gadget side: write reports here) AND a /dev/hidrawN
# (host side: zerowire-linux-sender reads from here, the same way the
# Android sender reads from a real USB HID device).
#
# All of this is in-kernel, no physical hardware required. See
# docs/synthetic-hw-verify.md for the full story.

set -euo pipefail

NAME="${NAME:-zw_mouse}"
VID="${VID:-0xBADD}"
PID="${PID:-0xC0DE}"
GADGET_DIR="/sys/kernel/config/usb_gadget/${NAME}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

log() { printf '[setup-gadget] %s\n' "$*" >&2; }

require_root() {
    if [ "$(id -u)" -ne 0 ]; then
        log "re-execing under sudo"
        exec sudo -E "$0" "$@"
    fi
}

load_modules() {
    log "loading kernel modules"
    modprobe libcomposite
    modprobe usb_f_hid
    modprobe usbip-core
    modprobe usbip-vudc
    modprobe vhci-hcd
}

ensure_configfs() {
    if ! mountpoint -q /sys/kernel/config; then
        log "mounting configfs"
        mount -t configfs none /sys/kernel/config
    fi
}

teardown() {
    if [ -d "${GADGET_DIR}" ]; then
        log "tearing down existing gadget ${NAME}"
        if [ -f "${GADGET_DIR}/UDC" ] && [ -s "${GADGET_DIR}/UDC" ]; then
            echo "" > "${GADGET_DIR}/UDC" || true
        fi
        # Remove function links from configs.
        for c in "${GADGET_DIR}/configs/"*/; do
            [ -d "$c" ] || continue
            for f in "$c"*; do
                if [ -L "$f" ]; then
                    rm -f "$f"
                fi
            done
            # remove config strings
            for s in "$c"strings/*; do
                [ -d "$s" ] && rmdir "$s" 2>/dev/null || true
            done
            rmdir "$c" 2>/dev/null || true
        done
        # Remove functions.
        for f in "${GADGET_DIR}/functions/"*; do
            [ -d "$f" ] && rmdir "$f" 2>/dev/null || true
        done
        # Remove gadget strings.
        for s in "${GADGET_DIR}/strings/"*; do
            [ -d "$s" ] && rmdir "$s" 2>/dev/null || true
        done
        rmdir "${GADGET_DIR}" 2>/dev/null || true
    fi
}

build_gadget() {
    log "creating gadget ${NAME} (VID=${VID} PID=${PID})"
    mkdir -p "${GADGET_DIR}"
    cd "${GADGET_DIR}"

    echo "${VID}"   > idVendor
    echo "${PID}"   > idProduct
    echo 0x0100     > bcdDevice
    echo 0x0200     > bcdUSB

    mkdir -p strings/0x409
    echo "ZW00000001"               > strings/0x409/serialnumber
    echo "builtbyzero"              > strings/0x409/manufacturer
    echo "zerowire synthetic mouse" > strings/0x409/product

    mkdir -p functions/hid.usb0
    # protocol: 0=none, 1=keyboard, 2=mouse (boot protocol)
    echo 2 > functions/hid.usb0/protocol
    # subclass: 0=none, 1=boot
    echo 1 > functions/hid.usb0/subclass
    # report_length is the size of a single input report in bytes.
    # Our 4-byte boot mouse report is [buttons, dx, dy, wheel].
    echo 4 > functions/hid.usb0/report_length

    # 4-byte boot-mouse report descriptor with wheel (matches what the
    # zerowire receiver decodes in apply_mouse_report).
    "${SCRIPT_DIR}/write-report-desc.py" \
        > functions/hid.usb0/report_desc

    mkdir -p configs/c.1
    mkdir -p configs/c.1/strings/0x409
    echo "zerowire HID config" > configs/c.1/strings/0x409/configuration
    echo 250                   > configs/c.1/MaxPower

    ln -sf functions/hid.usb0 configs/c.1/hid.usb0

    log "binding to UDC usbip-vudc.0"
    echo "usbip-vudc.0" > UDC

    sleep 0.3
}

show_state() {
    log "UDC state: $(cat /sys/class/udc/usbip-vudc.0/state 2>/dev/null || echo unknown)"
    log "/dev/hidg* devices:"
    ls -la /dev/hidg* 2>&1 | sed 's/^/    /' >&2 || true
}

main() {
    require_root "$@"
    load_modules
    ensure_configfs
    teardown
    build_gadget
    show_state
    log "gadget built. next: tools/synthetic-hw/attach-vhci.sh"
}

main "$@"
