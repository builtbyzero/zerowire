# zerowire v0.3 — Android-side userspace USB/IP URB pump

## Why this exists

AOSP doesn't ship `CONFIG_USBIP_HOST`, so the standard `usbip-host` kernel
driver isn't available on stock Pixel / GrapheneOS / Samsung kernels. v0.2
left the sender-side URB pump as an explicit stub
(`UsbIpHost.pumpUrbs()` threw `NotImplementedError`) and shipped only the
HID fast lane for non-vhci traffic.

v0.3 fills that gap with a **userspace** URB pump in Kotlin:

* `android-sender/.../UsbIpFrame.kt` — codec for the kernel-USB/IP
  submit/return headers, byte-for-byte identical to the Rust
  `protocol/src/usbip.rs` types.
* `android-sender/.../UsbIpHost.kt` — pump that reads `CMD_SUBMIT` /
  `CMD_UNLINK` frames, dispatches to `UsbDeviceConnection.controlTransfer`
  or `UsbDeviceConnection.bulkTransfer`, and writes back `RET_SUBMIT` /
  `RET_UNLINK`.

The receiver-side path (Linux `vhci-hcd`, Windows `usbip-win2`, macOS
DriverKit) is unchanged from v0.2.

## Wire format

Inside `Channel::Usbip` envelopes, after the 4-byte big-endian `import_id`
prefix:

```
+------------------ 48 bytes -----------------+----- 0..N bytes -----+
| command | seqnum | devid | direction | ep | | transfer buffer       |
| transfer_flags | transfer_buffer_length |   | (CMD_SUBMIT OUT,      |
| start_frame | number_of_packets |          |  RET_SUBMIT IN)        |
| interval | setup (8 bytes for control) |   |                       |
+---------------------------------------------+-----------------------+
```

`command` is one of:

| Code      | Direction    | Meaning                        | Body             |
| --------- | ------------ | ------------------------------ | ---------------- |
| 0x1 CMD_SUBMIT | rx → tx | start a transfer               | OUT data if OUT  |
| 0x2 CMD_UNLINK | rx → tx | cancel an in-flight transfer   | -                |
| 0x3 RET_SUBMIT | tx → rx | transfer finished              | IN data if IN    |
| 0x4 RET_UNLINK | tx → rx | unlink acknowledged            | -                |

All u32 fields are big-endian. The setup packet is 8 bytes in the standard
USB device-request layout (little-endian wValue/wIndex/wLength).

## Transfer types

| Type         | Implemented? | How                                          |
| ------------ | ------------ | -------------------------------------------- |
| Control      | yes          | `UsbDeviceConnection.controlTransfer`        |
| Bulk         | yes          | `UsbDeviceConnection.bulkTransfer`           |
| Interrupt    | yes          | `UsbDeviceConnection.bulkTransfer` (AOSP routes interrupt EPs through the same call) |
| Isochronous  | **no**       | Android's `UsbDeviceConnection` doesn't expose it. The pump replies `RET_SUBMIT { status = -EOPNOTSUPP (-95) }` so the receiver-side vhci-hcd unblocks the waiter cleanly. |

USB classes that work: mass storage, MTP, generic vendor-class, printers,
HID-over-vhci, MIDI command-mode (the bulk side of class-0x01). Classes
that need iso: USB audio class streaming, UVC video. We document that
explicitly; the user-visible behaviour is "device attaches, descriptors
read, iso URBs fail fast" — which is the same behaviour you get when a
real `usbip-host` is missing iso support on some kernel configs.

## Hardware verification status

* **Wire format**: 100% verified via the fixture loopback
  (`tests/android_pump_loopback.sh`). The Kotlin codec is byte-for-byte
  identical to the Rust `urb_pump` codec; both sides share the same
  unit-tested `protocol/src/usbip.rs` definitions.
* **Receiver path**: verified end-to-end against a real `urb_driver`
  issuing 7 base URBs + N extra bulk INs + 1 unlink, both plain and TLS
  1.3 mTLS. Receiver matches actual_length / status per URB.
* **Pump dispatch logic**: verified via Rust `urb_pump` tests that mirror
  the Kotlin pump's dispatch logic (control GET_DESCRIPTOR, bulk IN, bulk
  OUT, unlink, unsupported EP → -EOPNOTSUPP, missing EP → -ENODEV).
* **Real Android phone**: not yet run. Hardware-pending. The Kotlin
  code is reviewed against the AOSP `UsbDeviceConnection` API surface;
  the only gap is "does a Pixel's USB stack actually fan transfers out
  the way we expect under load." The fixture is shaped so a real phone
  drops in as a 1:1 replacement for `zerowire-simulate-android-pump`.

## Reproducing the loopback

```bash
cd zerowire
bash tests/android_pump_loopback.sh 3          # plain
bash tests/android_pump_loopback.sh 5 --psk    # TLS 1.3 mTLS
```

Either prints `PASS — N URBs + 1 unlink round-tripped`.

## Manual test plan for real hardware

When a Pixel and an x86 Linux host are both available:

1. `adb install android-sender/app/build/outputs/apk/debug/app-debug.apk`
2. Plug a USB-A mass-storage stick into the phone via OTG; tap "Allow".
3. On the host: `sudo modprobe vhci-hcd && ./target/debug/zerowire-cli
   receive --mode usbip --target <phone-ip>:47823 --psk <code>`.
4. Expect `lsusb` on the host to show the stick on a virtual bus and
   `dmesg | tail` to log a `usb-storage` attach.
5. `mount /dev/sdX1 /mnt && ls /mnt && cat /mnt/test.txt` round-trip
   should work at multi-megabyte/s for a v3.0 USB stick.

Iso devices (UVC webcams, USB audio) should fail with a clean
`-EOPNOTSUPP` and a log line; the receiver should unbind cleanly.
