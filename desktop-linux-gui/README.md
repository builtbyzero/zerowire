# zerowire-gui

Linux desktop GUI receiver for [zerowire](../README.md). Wraps the existing
`zerowire-cli` daemon with a real window: QR pairing, live status, the list
of USB devices the phone is sharing, and a Stop button.

## Status

v0.4 — first cut. Lands alongside the Android sender's device-acquisition
layer (PR #5). The CLI receiver still ships unchanged for headless boxes
and CI; the GUI is purely additive.

## Building

```bash
cd desktop-linux-gui
cargo build --release
./target/release/zerowire-gui
```

System dependencies for `eframe` on Linux:

```bash
sudo apt install libxkbcommon-dev libwayland-dev libx11-dev libxcb1-dev
# optional: for the system-tray feature
sudo apt install libayatana-appindicator3-dev
```

Optional tray-icon support is gated behind the `tray` feature, off by
default so a clean checkout builds on minimal Linuxes:

```bash
cargo build --release --features tray
```

The GUI execs `zerowire-cli` from `$PATH`. Set `ZEROWIRE_CLI=/path/to/zerowire-cli`
to override (useful when running from a workspace build directory).

## Architecture

```
+-----------------------+        +----------------------+
| zerowire-gui (egui)   |        |  zerowire-cli child  |
|  - QR generation      | stdout |  (existing daemon)   |
|  - window + status    | <----- |  - mDNS discovery    |
|  - device list        |        |  - TLS PSK handshake |
|  - Stop button        |        |  - URB pump          |
+-----------+-----------+        +----------------------+
            ^
            | scan QR
            v
+-----------------------+
|  Android sender app   |
+-----------------------+
```

The GUI is **unprivileged**: it never opens `/dev/uinput` or touches
`/sys/bus/usb`. The child CLI inherits whatever privileges its `udev`
rule grants (see `desktop-linux/udev/`).

## Wire format

The pairing QR encodes a `zerowire://pair?host=…&port=…&code=…` URI,
identical to the Kotlin `PairingPayload` in
`android-sender/.../QrPairing.kt`. The two implementations are kept in
lockstep by `tests/pair_qr_roundtrip.sh` in PR #5.

## Hardware verification

Unverified against a real Android sender — see
[../docs/v0.4-hardware-test.md](../docs/v0.4-hardware-test.md) for the
end-to-end plan.
