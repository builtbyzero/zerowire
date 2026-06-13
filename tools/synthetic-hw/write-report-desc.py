#!/usr/bin/env python3
"""Emit a 4-byte boot-mouse HID report descriptor on stdout.

This is the SAME shape as desktop-linux/src/hid_descriptor.rs::mouse_descriptor:
5-button + dx (i8) + dy (i8) + wheel (i8). Keeping them in lockstep is
deliberate: the receiver's apply_mouse_report() and this descriptor have
to agree about layout, otherwise the synthetic test exercises the wrong
code path.

Stdout (binary) is consumed by setup-gadget.sh.
"""

import sys

DESCRIPTOR = bytes([
    0x05, 0x01,  # Usage Page (Generic Desktop)
    0x09, 0x02,  # Usage (Mouse)
    0xA1, 0x01,  # Collection (Application)
    0x09, 0x01,  #   Usage (Pointer)
    0xA1, 0x00,  #   Collection (Physical)
    0x05, 0x09,  #     Usage Page (Button)
    0x19, 0x01,  #     Usage Minimum (1)
    0x29, 0x05,  #     Usage Maximum (5)
    0x15, 0x00,  #     Logical Minimum (0)
    0x25, 0x01,  #     Logical Maximum (1)
    0x95, 0x05,  #     Report Count (5)
    0x75, 0x01,  #     Report Size (1)
    0x81, 0x02,  #     Input (Data,Var,Abs)
    0x95, 0x01,  #     Report Count (1)
    0x75, 0x03,  #     Report Size (3)        ; padding
    0x81, 0x03,  #     Input (Cnst,Var,Abs)
    0x05, 0x01,  #     Usage Page (Generic Desktop)
    0x09, 0x30,  #     Usage (X)
    0x09, 0x31,  #     Usage (Y)
    0x09, 0x38,  #     Usage (Wheel)
    0x15, 0x81,  #     Logical Minimum (-127)
    0x25, 0x7F,  #     Logical Maximum (127)
    0x75, 0x08,  #     Report Size (8)
    0x95, 0x03,  #     Report Count (3)
    0x81, 0x06,  #     Input (Data,Var,Rel)
    0xC0,        #   End Collection
    0xC0,        # End Collection
])

if __name__ == "__main__":
    sys.stdout.buffer.write(DESCRIPTOR)
