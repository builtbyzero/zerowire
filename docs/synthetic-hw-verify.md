# Synthetic hardware verification — Linux/hidraw

> **What this is.** A kernel-only loopback that proves the v0.1 HID
> fast-lane stack (sender → wire → receiver → uinput) end-to-end on a
> single Linux machine, **without a real USB device or phone in the
> room.** Companion document to `docs/hardware-verify.md` (which is the
> phone-and-real-mouse runbook); this one is for builders, CI, and
> regression checks.
>
> **What it isn't.** A substitute for Stage 6 of the real
> hardware-verify runbook. The Android sender code path is *not*
> exercised — the Linux receiver is. But the bytes the receiver decodes
> have travelled through the kernel's USB HID layer (interrupt
> endpoint, hidraw, the works), not through a hand-rolled mock that
> just emits HidFrame::ReportIn bodies. That's the bar we're clearing.
>
> **Provenance.** Implemented in PR `feat/synthetic-hw-verify`.
> Captured `--diagnose` trace from a passing run is checked in at
> `docs/synthetic-hw-verify-trace.jsonl` so reviewers can compare.

## The picture

```
 ┌─────────────────────┐
 │ drive-mouse.py      │ writes 4-byte boot-mouse reports …
 │ → /dev/hidg0        │
 └──────────┬──────────┘
            │
        gadget side
            │   (libcomposite + usb_f_hid bound to a configfs gadget)
            ▼
 ┌─────────────────────┐    ┌──────────────────────┐
 │ usbip-vudc.0        │ ←→ │ usbipd -e (loopback) │
 │ (virtual UDC)       │    └──────────────────────┘
 └──────────┬──────────┘
            │   `usbip attach -r 127.0.0.1 -b usbip-vudc.0`
            ▼
 ┌─────────────────────┐
 │ vhci_hcd.0          │ host kernel enumerates the gadget as
 │ (virtual HCD)       │ a real USB device — same code path as
 └──────────┬──────────┘ a plugged-in mouse on USB bus 5.
            │
            ▼
 /dev/hidraw2        ← linux-sender reads here, exposes via the
                       same control + HID-fastlane protocol the
                       Android sender uses.
            │   TCP 127.0.0.1:47823, zerowire envelopes
            ▼
 zerowire-cli receive --diagnose …
            │
            ▼
 /dev/uinput → /dev/input/eventN   ← evtest grabs this, captures
                                     EV_REL motion + EV_KEY clicks.
```

Everything between `drive-mouse.py` and `evtest` is the **real
kernel**, not a mock. The only userspace code on the gadget side is a
9-line Python script that turns "press button 1" into 4 bytes; on the
host side it's the same `zerowire-cli receive` you'd run against a
phone.

## Prerequisites — one-time

```bash
# Kernel modules. All ship with linux-modules-extra on Ubuntu HWE 6.x.
sudo modprobe libcomposite usb_f_hid usbip-core usbip-vudc vhci-hcd

# Userspace.
sudo apt install -y usbip linux-tools-generic evtest

# Build everything.
cd desktop-linux
cargo build --bin zerowire-cli --bin zerowire-linux-sender
```

If `modprobe dummy_hcd` fails (it will on Ubuntu — Canonical doesn't
build it), don't worry. We don't use it. `usbip-vudc` + `vhci-hcd`
together do the same job: a virtual UDC for the gadget, a virtual host
controller for the host, with usbip in between. The kernel can't tell
the difference between this and a real device.

### What does NOT work, and why

| Approach | Status | Notes |
|---|---|---|
| `dummy_hcd` + `g_hid` | ✗ not shipped | Canonical doesn't include `dummy_hcd.ko` in `linux-modules-extra`. Building it requires source-building the kernel or the `dummy_hcd` out-of-tree module. **Use `usbip-vudc` instead.** |
| Android emulator USB passthrough | ✗ limited | `emulator -accel … -no-snapshot` doesn't expose `/dev/hidg*` style HID injection. `usbredir` works but adds qemu to the dep list and doesn't run the *Android sender* code anyway — we'd still be testing the Linux receiver against a fake. Not worth the complexity given `usbip-vudc` already gives us a real `/dev/hidrawN`. |
| Pure `uhid` userspace HID | ✗ doesn't prove what we want | `uhid` skips the USB stack entirely — the kernel routes reports directly without enumeration, descriptor parsing, hid-generic binding. A real bug in any of those layers would not surface. |

## Per-run — the happy path

There are five scripts, all in `tools/synthetic-hw/`. Run them in this
order:

```bash
# 1. Build the gadget on the virtual UDC. Creates /dev/hidg0.
sudo tools/synthetic-hw/setup-gadget.sh

# 2. Bring it across to the host side via usbip. Creates /dev/hidrawN
#    plus an entry in /proc/bus/input/devices.
sudo tools/synthetic-hw/attach-vhci.sh

# 3. Single command — drives the sender, the receiver, and evtest,
#    then asserts the five pass criteria. Exits non-zero on failure.
tools/synthetic-hw/run-e2e.sh

# 4. (optional) tear it all down when you're done.
sudo tools/synthetic-hw/teardown.sh
```

Expected output of step 3:

```
[run-e2e] synthetic hidraw: /dev/hidraw2
[run-e2e] starting linux-sender on 127.0.0.1:47823
[run-e2e] starting receiver (sudo for /dev/uinput) with --diagnose …
[run-e2e] uinput device: /dev/input/event10
[run-e2e] capturing evtest on /dev/input/event10 for ~6s
[run-e2e] driving 9 mouse reports through /dev/hidg0
PASS: bind_ack present in diagnose JSONL
PASS: received 5 report events
PASS: uinput device 'zerowire: ...' visible in /proc/bus/input/devices
PASS: evtest saw EV_REL motion + BTN_LEFT (real uinput event delivery)
PASS: bind_ack carried a non-empty report descriptor
OVERALL:      PASS
```

If you don't see `OVERALL: PASS`, the artefacts are in `/tmp/zw-synth/`:
look at `diagnose.jsonl` first, then `receiver.log`, then `sender.log`.

## Pass/fail criteria

Same shape as the real-hardware matrix. All five must be ☑ for PASS.

| # | Criterion | Evidence |
|---|-----------|----------|
| 1 | `--diagnose` JSONL contains a `bind_ack` event | `grep '"event":"bind_ack"' diagnose.jsonl` |
| 2 | `--diagnose` JSONL contains ≥1 `report` events | `grep -c '"event":"report"' diagnose.jsonl` |
| 3 | uinput device named `zerowire: …` was created | `/sys/class/input/eventN/device/name` |
| 4 | evtest sees `EV_REL` motion + `BTN_LEFT` events | `tools/synthetic-hw/evtest.log` |
| 5 | `bind_ack` carries a non-empty report descriptor (the receiver's `report_descriptor_len > 0`) | diagnose JSONL |

## How it works

### setup-gadget.sh

Mounts configfs, loads `libcomposite` / `usb_f_hid` / `usbip-vudc`,
then writes the standard configfs USB-gadget layout under
`/sys/kernel/config/usb_gadget/zw_mouse/`:

```text
zw_mouse/
├── idVendor                = 0xBADD
├── idProduct               = 0xC0DE
├── bcdDevice               = 0x0100
├── bcdUSB                  = 0x0200
├── strings/0x409/{manufacturer,product,serialnumber}
├── functions/hid.usb0/
│   ├── protocol            = 2          # boot mouse
│   ├── subclass            = 1          # boot subclass
│   ├── report_length       = 4
│   └── report_desc         = (52 bytes, written by write-report-desc.py)
├── configs/c.1/
│   ├── strings/0x409/configuration = "zerowire HID config"
│   ├── MaxPower            = 250
│   └── hid.usb0 -> ../../functions/hid.usb0
└── UDC                     = usbip-vudc.0   # binds the gadget
```

The descriptor it writes is the same 52-byte boot-mouse-with-wheel
descriptor exposed by `desktop-linux/src/hid_descriptor.rs` (5 buttons
+ X + Y + wheel, all 8-bit), so the receiver's `apply_mouse_report`
boot-mouse decode path is the one being exercised.

### attach-vhci.sh

Starts `usbipd -e -D` (device mode, daemonised), then `usbip attach -r
127.0.0.1 -b usbip-vudc.0`. The host kernel's vhci-hcd driver
enumerates the gadget as a normal USB device:

```text
/sys/devices/platform/vhci_hcd.0/usb5/5-1/
├── busnum                  = 5
├── devnum                  = 2
├── idVendor                = badd
├── idProduct               = c0de
├── product                 = "zerowire synthetic mouse"
├── manufacturer            = "builtbyzero"
├── serial                  = "ZW00000001"
└── 5-1:1.0/0003:BADD:C0DE.0003/   # the HID interface
    └── hidraw/hidraw2             # → /dev/hidraw2
```

The host's kernel HID layer also auto-binds `hid-generic` and creates
a kernel-side input device (the original `builtbyzero zerowire
synthetic mouse` you'll see in `/proc/bus/input/devices`). That's
*separate from* the receiver-side uinput device — the two coexist.
See §Caveats.

### zerowire-linux-sender

A new binary at `desktop-linux/src/bin/linux_sender.rs`. It mirrors
the Android sender (`SenderService` + `UsbInventory` +
`HidEndpoint`) closely:

- **Enumerate.** Reads `/sys/class/hidraw/hidraw*`, walks up to the
  `usb_device` ancestor, pulls `idVendor`/`idProduct`/`product`/
  `manufacturer`/`serial`/class triple/`bInterfaceClass` from sysfs.
  The result is a `DeviceSummary` exactly like Android's
  `UsbInventory.summarize` produces.
- **Listen.** Plain TCP on `127.0.0.1:47823` by default. Same wire
  protocol as the mock sender — control envelopes for HELLO /
  LIST_DEVICES / ATTACH, HID envelopes for Bind / BindAck / ReportIn /
  Unbind.
- **Pump.** On Bind it opens `/dev/hidrawN` non-blocking and forwards
  every read verbatim as `HidOp::ReportIn`. The bytes the receiver
  decodes are bit-for-bit identical to what the kernel emitted on the
  interrupt endpoint.

### drive-mouse.py

Writes 9 deterministic 4-byte boot-mouse reports to `/dev/hidg0`:

| seq | report (hex) | semantics |
|----:|--------------|-----------|
| 1–3 | `00 05 00 00` | dx=+5 (3×) |
| 4–5 | `00 00 05 00` | dy=+5 (2×) |
| 6   | `01 00 00 00` | BTN_LEFT press |
| 7   | `00 00 00 00` | release |
| 8   | `00 00 00 01` | wheel +1 |
| 9   | `00 00 00 00` | wheel reset |

Reports are spaced 30 ms apart so the kernel doesn't coalesce them and
the host's interrupt handler fires once per write.

### run-e2e.sh

Glue. Brings up the sender + receiver, waits for the uinput device,
starts `evtest --grab` on it, drives the reports, evaluates five
pass criteria, prints `OVERALL: PASS` or `FAIL`. CI-ready (exit
code 0 / non-zero).

## Captured evidence

A passing run produces these (checked in at `docs/`):

- **`synthetic-hw-verify-trace.jsonl`** — the receiver's full
  `--diagnose` JSONL. Includes the `start` → `dial_ok` →
  `hello_ack` → `device_list` → `attach_request` → `attach_ok` →
  `bind_sent` → `bind_ack` → `uinput_created` → 5× `report` →
  `report_rate` chain. **This is the ground truth referenced in the
  task spec ("Use the existing `--diagnose` JSON trace as ground
  truth for what counts as a pass")**.
- **`synthetic-hw-verify-evtest.log`** — evtest's full dump,
  including its initial introspection of the uinput device's
  capabilities (`EV_REL REL_X`, `EV_KEY BTN_LEFT`, etc.) and the
  six real events it captured during the `drive-mouse.py` run.

## Caveats — what this does and doesn't prove

1. **It proves**: the Linux receiver correctly handles real
   kernel-emitted HID reports — descriptor parsing, hidraw enumeration,
   bind/ack handshake, ReportIn decode, uinput plumbing.
2. **It does NOT prove**: the **Android sender** works. That's still
   the job of `docs/hardware-verify.md`. The pieces it shares with the
   Android path are the wire format (envelopes, control JSON, HID
   frames) — which is identical because both senders depend on the
   `zerowire-protocol` crate.
3. **It does NOT prove**: anything about TLS, mDNS discovery, PSK
   auth, or USB/IP mode. v0.1 scope was always HID fast lane only.
4. **Cursor side-effect.** Because the host kernel auto-binds the
   synthetic gadget as a real USB mouse, you'll see the laptop's
   actual cursor move while the test runs (the kernel-side device
   produces input events, separate from the receiver's uinput device).
   This is *evidence* that the kernel really treats the gadget as a
   USB mouse — but if you'd rather avoid it during testing, you can
   unbind hid-generic from the synthetic device first:

   ```bash
   echo 0003:BADD:C0DE.0003 | sudo tee /sys/bus/hid/drivers/hid-generic/unbind
   ```

   …though I'd argue *not* unbinding is the right default: it makes
   the whole "is this really a USB device?" question moot.

## CI hook (future work)

This is structured so that one day a CI job can run
`tools/synthetic-hw/run-e2e.sh` after a `cargo build`, gated on the
`linux-modules-extra` package being installed (which is the case on
GitHub Actions's `ubuntu-latest` runners). Until that's wired up, it
runs locally and produces the artefacts that get attached to PRs that
touch the receiver.

## Troubleshooting

### `modprobe usbip-vudc` fails: "Module not found"

Older kernels (<5.10) don't ship `usbip-vudc`. Either upgrade or fall
back to `dummy_hcd` — note `dummy_hcd` is not in stock Ubuntu, you'd
need to build it from kernel source.

### `usbipd: error: not running as root?` even with sudo

You probably already have a system `usbipd.service` running as a
different user. `sudo systemctl stop usbipd` then retry.

### `usbip list -r 127.0.0.1` returns "no exportable devices found"

You started `usbipd` **without** the `-e` flag — that's host-export
mode, which exposes attached USB devices, not virtual UDCs. Re-launch
with `usbipd -e -D`. (Our `attach-vhci.sh` does this for you.)

### `/dev/hidg0` write blocks indefinitely

The vhci side isn't attached yet — until a vhci port pulls the
gadget's frames, writes to `/dev/hidg0` queue but never drain. Run
`tools/synthetic-hw/attach-vhci.sh` first.

### No `zerowire:` uinput device appears

Check the receiver's diagnose log for an `uinput_create_failed` event.
Most likely cause: `/dev/uinput` permissions — the receiver needs to
either run as root or have the uinput udev rule installed
(`desktop-linux/udev/`).

### evtest reports `EV_REL REL_X value -5` not +5

Sanity check on host endianness — the report descriptor declares X as
i8, so the *gadget* and the *receiver* must agree about sign
interpretation. They do (Linux input layer treats X as signed). If
you see flipped axes, you're hitting a bug worth filing.

## Files in this branch

- `desktop-linux/src/bin/linux_sender.rs` — the sender shim.
- `desktop-linux/Cargo.toml` — adds the `zerowire-linux-sender` bin.
- `tools/synthetic-hw/setup-gadget.sh` — gadget build via configfs.
- `tools/synthetic-hw/write-report-desc.py` — emits the report descriptor.
- `tools/synthetic-hw/attach-vhci.sh` — usbipd + usbip attach.
- `tools/synthetic-hw/drive-mouse.py` — synthetic report driver.
- `tools/synthetic-hw/run-e2e.sh` — orchestrator + pass/fail oracle.
- `tools/synthetic-hw/teardown.sh` — unwinds everything.
- `docs/synthetic-hw-verify-trace.jsonl` — captured pass trace.
- `docs/synthetic-hw-verify-evtest.log` — captured evtest dump.
