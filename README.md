# zerowire

**A wireless USB hub.** Plug a USB device into an Android phone; use it from any computer or phone on the same WiFi.

builtbyzero · MIT-spirited / Apache-2.0 licensed.

> **Status: v0.2 — TLS 1.3 PSK auth + USB/IP general passthrough (simulated).** v0.1 HID fast-lane is wired end-to-end on Linux (mock-sender ⇄ Linux receiver loopback; see [`docs/hid-demo.md`](./docs/hid-demo.md) and [`tests/hid_loopback.sh`](./tests/hid_loopback.sh)). Android sender hardware verification runbook + pass/fail matrix in [`docs/hardware-verify.md`](./docs/hardware-verify.md); synthetic kernel-loopback harness in [`docs/synthetic-hw-verify.md`](./docs/synthetic-hw-verify.md). v0.2 adds: TLS 1.3 mutual auth keyed by a pairing-code PSK ([`docs/tls-psk.md`](./docs/tls-psk.md)) and a USB/IP receive path that targets `vhci-hcd` on real hardware or a transcript file on CI ([`docs/usbip-passthrough.md`](./docs/usbip-passthrough.md)). Loopback tests cover all three paths. The Android sender side of USB/IP host-export remains stubbed — AOSP doesn't ship `usbip-host`; see [`android-sender/app/src/main/kotlin/.../UsbIpHost.kt`](./android-sender/app/src/main/kotlin/zero/builtby/zerowire/sender/UsbIpHost.kt) for the gap and the v0.3 plan. Full target system: [`ARCHITECTURE.md`](./ARCHITECTURE.md).

## What it does (when finished)

- 📱 **Sender** runs on an Android phone. Anything you plug into the phone's USB-C port — mouse, keyboard, gamepad, MIDI controller, label printer, microcontroller, mass-storage stick — gets shared on the LAN.
- 💻 **Receivers** are Linux / Windows / macOS / Android / iOS apps. They see the shared device as if it were plugged into them.
- 🛰️ **No cables, no cloud.** Just WiFi. mDNS discovery, TLS + PSK pairing, runs entirely on your local network.

## Repo layout

```
zerowire/
├── ARCHITECTURE.md       ← read this first
├── README.md             ← you are here
├── LICENSE               ← Apache-2.0
├── protocol/             ← shared protocol definitions (Rust)
├── android-sender/       ← Android app (Kotlin + Gradle)
├── desktop-linux/        ← Linux receiver CLI + daemon (Rust)
├── desktop-windows/      ← Windows receiver (placeholder)
├── desktop-macos/        ← macOS DriverKit sysext (placeholder)
├── android-receiver/     ← Android receiver app (placeholder)
└── ios-receiver/         ← iOS receiver app (placeholder)
```

## Platform support (target)

| Platform | What you get | How |
|---|---|---|
| **Linux** | Any USB device | Built-in `vhci-hcd` + our userspace daemon |
| **Windows** | Any USB device | Bundled signed `usbip-win2` driver + installer |
| **macOS** | Any USB device | DriverKit System Extension (HID-only fallback if Apple delays the entitlement) |
| **Android receiver** | HID + companion API | Accessibility Service injection; AIDL for apps |
| **iOS receiver** | HID only | Sender phone exposes Bluetooth HID alongside WiFi handshake |

Honest limits:

- iOS is **HID-only**, ever. Apple does not allow third-party USB injection.
- Some Android phones disable USB host. We detect and refuse.
- Isochronous endpoints (USB audio, webcams) may not work on every Android device.

## Building (today)

### Protocol (Rust)
```bash
cd protocol
cargo test
```

### Linux receiver CLI
```bash
cd desktop-linux
cargo build
./target/debug/zerowire-cli discover
```

### HID demo (Linux receiver + mock sender)

Fastest way to see the wire actually move bytes:

```bash
cd desktop-linux
cargo build --bin zerowire-cli --bin zerowire-mock-sender

# Terminal 1: pretend to be the Android sender on loopback.
./target/debug/zerowire-mock-sender --reports 200 --interval-ms 20

# Terminal 2: real receiver. Needs /dev/uinput access.
# (`sudo modprobe uinput` once; for unprivileged use install the udev
# rule in `desktop-linux/udev/`. See that dir's README for the one-shot
# setup.)
./target/debug/zerowire-cli receive --target 127.0.0.1:47823
```

A virtual mouse appears (visible to `libinput list-devices` and to your
desktop session) and the cursor twitches right ~200 times. Full demo doc:
[`docs/hid-demo.md`](./docs/hid-demo.md).

No permission to open `/dev/uinput`? Run the integration test, which
logs report bytes to a file instead:

```bash
./tests/hid_loopback.sh        # v0.1 HID fast lane — passes locally; ~1s wall time
./tests/tls_psk_loopback.sh    # v0.2 TLS 1.3 PSK end-to-end (right + wrong PSK)
./tests/usbip_loopback.sh      # v0.2 USB/IP simulated vhci attach
./tests/usbip_loopback.sh 25 --psk   # v0.2 USB/IP over TLS PSK
```

### Android sender
```bash
cd android-sender
./gradlew assembleDebug
# APK in app/build/outputs/apk/debug/
```

> Note: requires Android SDK 34, JDK 17, and the Gradle wrapper will fetch itself on first run.
> **The Android sender code in this checkout is not yet hardware-verified.**
> Manual test plan: [`docs/hid-demo.md`](./docs/hid-demo.md).

## Roadmap

- **v0 (done):** architecture, protocol skeleton, walking skeletons that compile.
- **v0.1 (done):** HID fast-lane end-to-end. Linux receiver + mock sender loopback verified.
- **v0.2 (this branch):**
  - TLS 1.3 mutual auth keyed by a pairing-code PSK (HKDF-SHA256 → deterministic Ed25519 cert; both sides pin). Wrong PSK → handshake fails. Loopback-verified.
  - USB/IP receive path for non-HID devices: real `vhci-hcd` attach on hardware; `--simulate-usbip <log>` transcript path for CI. Loopback-verified.
  - Android side: PSK derivation Kotlin port + TLS-server scaffolding shipped. URB-pump and deterministic-cert builder are stubs (see [`UsbIpHost.kt`](./android-sender/app/src/main/kotlin/zero/builtby/zerowire/sender/UsbIpHost.kt) and [`TlsServer.kt`](./android-sender/app/src/main/kotlin/zero/builtby/zerowire/sender/TlsServer.kt) — both honestly mark what's left).
- **v0.3:** Android-side USB/IP URB-pump shim + Windows receiver with bundled `usbip-win2`.
- **v0.4:** macOS HID-only receiver.
- **v0.5:** Android receiver via Accessibility Service.
- **v1.0:** all of the above + paid tier.

## License

Apache-2.0. See [`LICENSE`](./LICENSE).

---

**builtbyzero** — small, sharp tools.
