# zerowire — Android sender

Walking skeleton of the Android app that shares plugged-in USB devices over WiFi.

## What works today

- `MainActivity` lists every currently-plugged USB device with vid/pid and HID-or-not.
- `SenderService` runs as a foreground service:
  - binds a TCP listening socket on port **47823** (the zerowire default),
  - registers `_zerowire._tcp.` with `NsdManager` and publishes the TXT records that match `protocol::discovery` (`v`, `id`, `n`, `port`, `caps`).
- `UsbInventory` produces the JSON payload that matches `ControlMessage::DeviceList` in the shared protocol crate.
- `PairingCodes.sixDigit()` generates a `SecureRandom`-backed 6-digit code, displayed on `MainActivity`.

## What's stubbed

- No `accept()` loop yet — the listening socket exists but the session handler isn't wired. Next milestone.
- No TLS yet (the receiver also doesn't speak TLS yet — both sides plaintext for the v0 handshake).
- No per-device authorization UI — that's planned (see `ARCHITECTURE.md` §5.3).

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
