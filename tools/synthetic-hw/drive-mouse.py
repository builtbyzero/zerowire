#!/usr/bin/env python3
"""Drive the gadget's /dev/hidg0 with synthetic mouse reports.

This is the "physical hand on the mouse" half of the synthetic
verification: we generate the report bytes that a real boot-protocol
mouse would emit (4 bytes: buttons, dx, dy, wheel), write them to the
gadget's hidg endpoint, and let the kernel deliver them to the host
side. The linux-sender shim then reads the resulting /dev/hidrawN and
forwards them through the zerowire stack.

Run pattern: write a short, deterministic sequence so the receiver's
--diagnose log captures a known shape (5 motion reports + 1 click + 1
release + 1 wheel + 1 release = 9 reports). That gives us crisp
pass/fail criteria.

Requires write access to /dev/hidg0 (root or chowned).
"""
from __future__ import annotations

import argparse
import os
import sys
import time


def report(buttons: int, dx: int, dy: int, wheel: int) -> bytes:
    """Build a 4-byte boot-mouse report.

    dx/dy/wheel are signed 8-bit; we clamp and convert. Anything outside
    [-127, 127] is a user error — fail loudly rather than silently truncating.
    """
    for axis, value in ("dx", dx), ("dy", dy), ("wheel", wheel):
        if value < -127 or value > 127:
            raise ValueError(f"{axis}={value} out of i8 range")
    if buttons < 0 or buttons > 0x1F:
        raise ValueError(f"buttons={buttons:#x} out of 5-bit range")
    return bytes([
        buttons & 0xFF,
        dx & 0xFF,
        dy & 0xFF,
        wheel & 0xFF,
    ])


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--device", default="/dev/hidg0")
    ap.add_argument(
        "--interval-ms",
        type=int,
        default=8,  # boot mouse polls at 125Hz → 8ms
    )
    ap.add_argument(
        "--scenario",
        choices=["smoke", "extended"],
        default="smoke",
        help="smoke: 9 deterministic reports; extended: 100 motion reports too",
    )
    args = ap.parse_args()

    smoke = [
        # 5x motion: right then down
        report(0, 5, 0, 0),
        report(0, 5, 0, 0),
        report(0, 5, 0, 0),
        report(0, 0, 5, 0),
        report(0, 0, 5, 0),
        # left click press + release
        report(0b00001, 0, 0, 0),
        report(0b00000, 0, 0, 0),
        # wheel up + reset
        report(0, 0, 0, 1),
        report(0, 0, 0, 0),
    ]
    plan = list(smoke)
    if args.scenario == "extended":
        # 100 more 1px-right motion frames to exercise the report_rate path.
        plan.extend(report(0, 1, 0, 0) for _ in range(100))

    try:
        with open(args.device, "wb", buffering=0) as f:
            for i, r in enumerate(plan):
                f.write(r)
                # Make sure the kernel doesn't coalesce; small sleep matches
                # interrupt-endpoint cadence so the host generates one input
                # event per report.
                time.sleep(args.interval_ms / 1000.0)
            print(f"[drive-mouse] wrote {len(plan)} reports to {args.device}",
                  file=sys.stderr)
    except PermissionError as e:
        print(f"[drive-mouse] permission denied opening {args.device}: {e}",
              file=sys.stderr)
        print("[drive-mouse] try: sudo chmod a+w /dev/hidg0", file=sys.stderr)
        return 2
    except FileNotFoundError:
        print(f"[drive-mouse] {args.device} not found; run setup-gadget.sh first",
              file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
