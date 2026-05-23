# zerowire — Android sender

Walking skeleton of the Android app that shares plugged-in USB devices over WiFi.

## What works today

- `MainActivity` lists every currently-plugged USB device with vid/pid and HID-or-not, requests USB permission for them, and shows the `zerowire-cli receive --sender <name>` command-line to run on the laptop.
- `SenderService` runs as a foreground service:
  - binds a TCP listening socket on port **47823** (the zerowire default),
  - registers `_zerowire._tcp.` with `NsdManager` and publishes the TXT records that match `protocol::discovery` (`v`, `id`, `n`, `port`, `caps`),
  - **accepts incoming receivers** and runs each through `ReceiverSession`.
- `ReceiverSession` walks the full protocol state machine:
  HELLO → HELLO_ACK, LIST_DEVICES → DEVICE_LIST, ATTACH → ATTACH_OK,
  HID Bind → BindAck, then streams `HidOp::ReportIn` envelopes for every
  HID input report read off the device.
- `HidEndpoint` claims a USB HID interface, fetches the report descriptor
  via the standard HID class control transfer, and reads input reports
  off the IN interrupt endpoint.
- `WireProtocol` is the Kotlin port of `protocol/src/envelope.rs` and
  `protocol/src/hid.rs` — same bytes, same semantics.

## What's still stubbed in v0.1

- **Pairing is plaintext.** The 6-digit code displayed in the UI is
  informational; the receiver does not have to enter it. TLS + PSK proof
  on the control channel is v0.2 work (ARCHITECTURE.md §6.2).
- **Per-device authorization UI** is implicit — if Android already granted
  USB permission, we claim. Otherwise the user gets the system permission
  prompt at `MainActivity` startup. The ARCHITECTURE.md §5.3 "explicit
  authorize each receiver-side attach" flow is not yet built.
- **High-DPI or NKRO devices** stream their reports as-is; the receiver's
  boot-format decoder may misinterpret them. Sender-side translation to
  boot format is a follow-up.

## Hardware verification status

The Kotlin code in this checkout was developed without an Android SDK on
the build host. It compiles cleanly in code review against the AOSP source
for `UsbDeviceConnection`/`UsbHostManager`, but **has not been run against
a real phone**. See `docs/hid-demo.md` for the manual test plan.

## Build prerequisites

This is a stock AGP 8.5 / Kotlin 1.9 / `compileSdk 34` project. To build the APK locally:

1. JDK 17 (`sudo apt install openjdk-17-jdk`).
2. Android SDK with `platforms;android-34` and `build-tools;34.0.0`. Install via `cmdline-tools` or Android Studio.
3. Tell Gradle where the SDK lives:
   ```bash
   echo "sdk.dir=$HOME/Android/Sdk" > local.properties
   ```
4. Build:
   ```bash
   ./gradlew assembleDebug
   # APK at app/build/outputs/apk/debug/app-debug.apk
   ```

The Gradle wrapper (`./gradlew`) will self-bootstrap Gradle 8.7 on first run; you do **not** need a system Gradle install.

## Layout

```
android-sender/
├── settings.gradle.kts
├── build.gradle.kts
├── gradle.properties
├── gradle/wrapper/
│   ├── gradle-wrapper.jar
│   └── gradle-wrapper.properties
├── gradlew, gradlew.bat
└── app/
    ├── build.gradle.kts
    ├── proguard-rules.pro
    └── src/main/
        ├── AndroidManifest.xml
        ├── kotlin/zero/builtby/zerowire/sender/
        │   ├── MainActivity.kt
        │   ├── PairingCodes.kt
        │   ├── SenderService.kt
        │   └── UsbInventory.kt
        └── res/
            ├── layout/activity_main.xml
            ├── values/strings.xml
            └── xml/device_filter.xml
```

## Permissions / capabilities

| Permission / feature | Reason |
|---|---|
| `android.hardware.usb.host` | Enumerate plugged USB devices |
| `INTERNET`, `ACCESS_NETWORK_STATE`, `ACCESS_WIFI_STATE` | TCP socket + LAN check |
| `CHANGE_WIFI_MULTICAST_STATE` | Receive mDNS multicasts when scanning peers |
| `FOREGROUND_SERVICE`, `FOREGROUND_SERVICE_CONNECTED_DEVICE` | Persistent service while devices are shared |
| `POST_NOTIFICATIONS` | Required on Android 13+ for the foreground-service notification |
| `<intent-filter>` on `USB_DEVICE_ATTACHED` + `device_filter.xml` | Auto-launch on plug-in |
