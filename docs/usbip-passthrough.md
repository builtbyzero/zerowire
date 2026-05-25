# USB/IP general passthrough — v0.2

> One paragraph summary: v0.1 only handled HID via uinput. v0.2 adds a
> second mode where the Linux receiver hands forwarded USB devices to the
> kernel's `vhci-hcd` driver — so mass-storage, MIDI, printers, generic
> USB show up as real devices to the rest of the OS. CI runs the same
> path against a transcript file via `--simulate-usbip` so we don't need
> the kernel module on the build host.

## Wire delta

```
HELLO              client → server      (same as v0.1)
HELLO_ACK          server → client      (same as v0.1)
LIST_DEVICES       client → server      (same as v0.1)
DEVICE_LIST        server → client      (same as v0.1)
ATTACH { mode: "usbip" }                client → server     (NEW mode value)
ATTACH_OK_USBIP { info: UsbipAttachInfo } server → client   (NEW message)
[binary URBs on Channel::Usbip, one per envelope]
```

`UsbipAttachInfo` lives in `protocol/src/usbip.rs` and serializes to:

```json
{
  "op": "ATTACH_OK_USBIP",
  "busid": "1-2",
  "import_id": 7,
  "devid": 65538,                  // (bus << 16) | dev
  "speed": 3,                       // 1=low 2=full 3=high 5=super
  "vendor_id": 1133,
  "product_id": 50475,
  "descriptor_hex": "12010002080…" // raw 18-byte usb_device_descriptor
}
```

URBs on `Channel::Usbip` are framed as `UsbipFrame { import_id: u32 }`
followed by the opaque kernel-format USB/IP packet. The receiver doesn't
parse the packet at the application layer — it hands the socket to
`vhci-hcd` and lets the kernel drive.

## Real attach path

```bash
sudo modprobe vhci-hcd                # if not already loaded
zerowire-cli receive --mode usbip --target <sender-host>:47823 --busid 1-2
```

The receiver writes
`<port> <socket_fd> <devid> <speed>` to
`/sys/devices/platform/vhci_hcd.0/attach`. After that the kernel owns
the FD; userspace returns and the device shows up in `lsusb`.

Caveats:

- Need root **or** a udev rule that grants rw on the vhci sysfs nodes.
  Sample rule: `desktop-linux/udev/99-zerowire-vhci.rules` (next commit).
- Detach is via `/sys/devices/platform/vhci_hcd.0/detach`. We do that on
  drop today only for the simulated path; real-attach detach is handed
  off to the kernel and survives the userspace process exit.

## Simulated attach path

For CI hosts (no `vhci-hcd` module, no `/sys/devices/platform/vhci_hcd.0`),
the same CLI flag set plus `--simulate-usbip <transcript>` instead of
touching sysfs:

```bash
zerowire-cli receive --mode usbip --target 127.0.0.1:47823 \
    --simulate-usbip /tmp/vhci.log
```

The receiver writes one `sim-attach` line, one `sim-urb` line per
relayed URB, and one `sim-detach` line on close. `tests/usbip_loopback.sh`
asserts on those lines.

## Honest hardware gap

The Android-side `usbip-host` driver does not exist on AOSP kernels
(see [`UsbIpHost.kt`](../android-sender/app/src/main/kotlin/zero/builtby/zerowire/sender/UsbIpHost.kt)
for the full table of which `CONFIG_USBIP_*` knobs are off and why).

The receiver, protocol, and TLS path are all verified via the desktop
mock sender. The Android URB-pump shim is **the** remaining piece for
hardware-end-to-end, and is tracked for v0.3. The sender stub today
reads the real device descriptor off the phone correctly and emits
`ATTACH_OK_USBIP` with real fields — only the actual URB-relay shovel
is missing.
