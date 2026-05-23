# zerowire

**A wireless USB hub.** Plug a USB device into an Android phone; use it from any computer or phone on the same WiFi.

builtbyzero · MIT-spirited / Apache-2.0 licensed.

> **Status: v0 walking skeletons.** This repo currently contains the architecture, protocol skeleton, and the bare bones of an Android sender and a Linux receiver CLI. There is **no actual USB passthrough yet** — only discovery, handshake, and device-listing plumbing. See [`ARCHITECTURE.md`](./ARCHITECTURE.md) for the target system.

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

### Android sender
```bash
cd android-sender
./gradlew assembleDebug
# APK in app/build/outputs/apk/debug/
```

> Note: requires Android SDK 34, JDK 17, and the Gradle wrapper will fetch itself on first run.

## Roadmap

- **v0 (now):** architecture, protocol skeleton, walking skeletons that compile.
- **v0.1 next:** HID fast-lane end-to-end (Android sender → Linux receiver, mouse moves a real cursor).
- **v0.2:** USB/IP passthrough on Linux receiver against a real USB stick.
- **v0.3:** Windows receiver with bundled `usbip-win2`.
- **v0.4:** macOS HID-only receiver.
- **v0.5:** Android receiver via Accessibility Service.
- **v1.0:** all of the above + paid tier.

## License

Apache-2.0. See [`LICENSE`](./LICENSE).

---

**builtbyzero** — small, sharp tools.
