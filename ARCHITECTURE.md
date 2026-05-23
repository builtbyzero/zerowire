# zerowire — Architecture

> Status: **v0 walking skeleton**. This document describes the target system. Code in this repo implements only the framing/handshake/discovery layers so far; actual USB passthrough is not wired yet.

## 1. Product summary

zerowire turns an Android phone into a wireless USB hub. Any USB device plugged into the phone (mouse, keyboard, gamepad, mass storage, MIDI controller, microcontroller, label printer, etc.) appears to other computers and phones on the same WiFi LAN as if it were locally plugged in.

- **Sender** = the Android phone holding the physical USB device.
- **Receiver** = a Linux / Windows / macOS / Android / iOS device consuming it.

zerowire is two protocols carried over one authenticated TCP (and later QUIC) connection:

1. **USB/IP** — the kernel.org-standard remote-USB protocol. Used for arbitrary USB device classes on platforms that can host a real USB stack (Linux via `vhci-hcd`, Windows via `usbip-win2`, macOS via a DriverKit sysext).
2. **HID fast lane** — a small custom sub-protocol for HID input devices (mouse, keyboard, gamepad). Optimized for end-to-end latency. Used on platforms that cannot inject a virtual USB device (iOS, Android receiver) and as a low-latency shortcut even on platforms that could go through USB/IP.

## 2. Component diagram

```
                       ┌──────────────────────────────┐
                       │       ANDROID SENDER         │
                       │  ┌────────────────────────┐  │
   physical USB ──────▶│  │ USB Host API           │  │
                       │  │  • enumerate devices   │  │
                       │  │  • claim interfaces    │  │
                       │  │  • bulk/intr/ctrl xfer │  │
                       │  └─────────┬──────────────┘  │
                       │            │                 │
                       │  ┌─────────▼──────────────┐  │
                       │  │ Session / Multiplexer  │  │
                       │  │  • USB/IP framing      │  │
                       │  │  • HID fast lane       │  │
                       │  │  • Auth / TLS / PSK    │  │
                       │  └─────────┬──────────────┘  │
                       │            │                 │
                       │  ┌─────────▼──────────────┐  │
                       │  │ mDNS / Pairing UI      │  │
                       │  │  _zerowire._tcp        │  │
                       │  └─────────┬──────────────┘  │
                       └────────────┼─────────────────┘
                                    │  WiFi LAN
        ┌───────────────────────────┼───────────────────────────┐
        │                           │                           │
┌───────▼────────┐         ┌────────▼────────┐         ┌────────▼──────────┐
│ LINUX RECEIVER │         │ WINDOWS         │         │ macOS             │
│ vhci-hcd       │         │ usbip-win2      │         │ DriverKit sysext  │
│ + tray daemon  │         │ + tray + signed │         │ IOUserUSBHostDev. │
└────────────────┘         └─────────────────┘         └───────────────────┘

        ┌─────────────────┐                   ┌─────────────────┐
        │ ANDROID         │                   │ iOS             │
        │ HID via         │                   │ Bluetooth HID   │
        │ Accessibility   │                   │ (phone advertises│
        │ + companion API │                   │  as BT kbd/mouse)│
        └─────────────────┘                   └─────────────────┘
```

## 3. Wire protocol

A zerowire connection is a single TCP stream after a TLS 1.3 handshake (with PSK and pinned cert). Framing is a thin envelope so we can multiplex USB/IP and HID fast-lane on the same socket.

### 3.1 Envelope

```
 0               1               2               3
 +---------------+---------------+---------------+---------------+
 | magic 'Z' 'W' | version (u8)  | channel (u8)                  |
 +---------------+---------------+---------------+---------------+
 | length (u32 big-endian, payload bytes)                         |
 +---------------------------------------------------------------+
 | payload ...                                                    |
 +---------------------------------------------------------------+
```

- `version` — currently `1`.
- `channel`:
  - `0x01` CONTROL — handshake, capability negotiation, device list, pairing.
  - `0x02` USBIP — USB/IP protocol bytes verbatim (see §3.3).
  - `0x03` HID — HID fast lane (see §3.4).
  - `0x7F` KEEPALIVE — empty payload, every 5s idle.

The envelope lets us interleave USB/IP transfers and HID input reports on the same socket without head-of-line blocking at the application layer. We may move to QUIC in v2 to get per-stream flow control for free.

### 3.2 Handshake (CONTROL channel)

All control messages are length-prefixed JSON (UTF-8), one per envelope. We chose JSON for the control plane because it's tiny in volume and we want easy debuggability; the bulk traffic is binary.

1. `HELLO` (receiver → sender)
   ```json
   { "op": "HELLO", "version": 1, "client": "linux-cli/0.1.0",
     "supports": ["usbip/1.1.1", "hid-fastlane/1"] }
   ```
2. `HELLO_ACK` (sender → receiver)
   ```json
   { "op": "HELLO_ACK", "sender_id": "uuid", "name": "Pixel 8",
     "supports": ["usbip/1.1.1", "hid-fastlane/1"] }
   ```
3. `AUTH` — PSK proof (HMAC-SHA256 of the TLS exporter secret with the PSK).
4. `LIST_DEVICES` / `DEVICE_LIST` — JSON inventory of attached USB devices.
5. `ATTACH { busid }` — claim a specific device. Server replies with either a USB/IP `OP_REP_IMPORT` on the USBIP channel, or a `HID_BIND` ack on the HID channel, depending on what the receiver requested.

### 3.3 USB/IP layer

zerowire speaks the kernel.org USB/IP protocol verbatim (see `Documentation/usb/usbip_protocol.rst`). Once a device is attached, the receiver speaks `USBIP_CMD_SUBMIT` / `USBIP_CMD_UNLINK` and gets `USBIP_RET_SUBMIT` / `USBIP_RET_UNLINK` in return. The op codes we care about:

| Op | Value | Direction | Purpose |
|----|-------|-----------|---------|
| `OP_REQ_DEVLIST` | 0x8005 | rx→tx | List exportable devices |
| `OP_REP_DEVLIST` | 0x0005 | tx→rx | Reply with device list |
| `OP_REQ_IMPORT`  | 0x8003 | rx→tx | Claim a device |
| `OP_REP_IMPORT`  | 0x0003 | tx→rx | Import reply |
| `USBIP_CMD_SUBMIT` | 0x00000001 | rx→tx | Submit a URB |
| `USBIP_RET_SUBMIT` | 0x00000003 | tx→rx | URB complete |
| `USBIP_CMD_UNLINK` | 0x00000002 | rx→tx | Cancel a URB |
| `USBIP_RET_UNLINK` | 0x00000004 | tx→rx | Cancel ack |

zerowire ships its own implementation of this protocol on the sender (the official `usbipd` is a Linux daemon that needs kernel modules we don't have on Android). On the receiver we lean on:

- **Linux:** `vhci-hcd` kernel module; we drive `/sys/devices/platform/vhci_hcd.0/attach` from userspace (same as the in-tree `usbip attach` tool). Sender stream is plumbed through a TCP socket the kernel reads from.
- **Windows:** `usbip-win2` userspace + signed driver, bundled with our installer.
- **macOS:** A DriverKit System Extension implementing `IOUserUSBHostDevice` that translates URBs to/from our TCP stream.

#### Wire-format extensions we add

The base USB/IP protocol is fine for LAN passthrough but lacks two things we need:

- **Multiplexing.** Multiple devices share one socket. We wrap each USB/IP packet in a zerowire envelope on channel `0x02` and add an `import_id` (u32) into the envelope's payload header so the receiver can demux per-device URBs from one TCP connection. Concretely: bytes 0..3 of the USBIP-channel payload are `import_id (u32 BE)`, followed by the raw USB/IP packet.
- **Cancel-with-reason.** `USBIP_CMD_UNLINK` is silent on why. We add an optional trailer (`u8 reason`) on the sender side, ignored by stock clients, used by our clients to surface "device unplugged" vs "timeout" vs "policy".

### 3.4 HID fast lane

For pointer/keyboard/gamepad we don't want to round-trip a full USB stack — we want input reports delivered with the lowest possible jitter. The HID lane runs on envelope channel `0x03`:

```
 0               1               2               3
 +---------------+---------------+---------------+---------------+
 | hid_op (u8)   | bind_id (u8)  | seq (u16 BE)                  |
 +---------------+---------------+---------------+---------------+
 | payload ...                                                    |
 +---------------------------------------------------------------+
```

`hid_op`:

| Op | Value | Direction | Payload |
|----|-------|-----------|---------|
| `BIND`        | 0x01 | rx→tx | JSON: `{ busid, report_descriptor_sha256, want: "input" }` |
| `BIND_ACK`    | 0x02 | tx→rx | `bind_id` (u8) + full HID report descriptor bytes |
| `REPORT_IN`   | 0x10 | tx→rx | raw HID input report bytes (incl. report id if used) |
| `REPORT_OUT`  | 0x11 | rx→tx | raw HID output report (host→device, e.g. caps-lock LED) |
| `REPORT_FEAT` | 0x12 | both | feature report (get/set) |
| `UNBIND`      | 0x20 | both | device gone / user revoked |

Design notes:

- `seq` lets us detect drops and is also used by the receiver to apply optional smoothing on pointer deltas if RTT spikes.
- We coalesce HID writes with `TCP_NODELAY` set; per-report Nagle delay is too costly for mice.
- v2 candidate: a parallel UDP/DTLS lane for `REPORT_IN` only, with at-most-once semantics; falls back to the TCP HID lane on loss.

## 4. Discovery

mDNS service type `_zerowire._tcp.local.` Advertised TXT records:

- `v=1` — protocol version
- `id=<uuid>` — stable sender id (rotated on factory reset)
- `n=<utf8 name>` — user-visible device name (defaults to Android device model)
- `caps=usbip,hid` — comma-separated capability tags
- `port=<u16>` — TCP port (also in SRV record; duplicated for clients with broken SRV parsers)

The receiver displays discovered senders; the user picks one and is prompted to enter the pairing code shown on the sender phone.

## 5. Pairing & security

### 5.1 First pairing

Sender displays a 6-digit code AND a QR (same payload). QR encodes:

```
zerowire://pair?host=<ip>&port=<u16>&id=<sender_id>&fp=<spki_sha256>&psk=<base32>
```

- `fp` — SHA-256 of the sender's TLS SPKI. The receiver pins this for future connections.
- `psk` — 20-byte random pre-shared key, base32-encoded. Burned into receiver storage after first use.

If the receiver can't scan a QR (e.g. terminal client), the 6-digit code is the short authenticator for an SRP-style exchange — but v1 implementation: the 6-digit code is an HMAC of a freshly-generated PSK keyed by a code-derived secret; the receiver enters the code, the sender verifies, both sides commit the PSK. Good enough against passive LAN attackers; the QR path is preferred.

### 5.2 Steady-state

Every connection is TLS 1.3. The sender's cert is self-signed and pinned via SPKI hash. After the TLS handshake, the receiver sends `AUTH` with `HMAC-SHA256(psk, tls_exporter("EXPORTER-zerowire-auth", "", 32))`. The sender verifies.

### 5.3 Per-device authorization

Even after pairing, each new USB device must be authorized on the sender once. The sender shows a "Receiver `<name>` wants to use `<USB device>`. Allow? [once / always / never]". `always` is stored per (receiver_id, vendor_id, product_id, serial).

### 5.4 Threat model

- ✅ Passive WiFi attacker — defeated by TLS.
- ✅ Active LAN attacker — defeated by SPKI pin + PSK.
- ✅ Malicious paired receiver — limited by per-device authorization; can't drive a device the user didn't approve.
- ⚠️ Malicious USB device on sender — out of scope. zerowire trusts the USB device the user plugged in.
- ⚠️ Compromised sender — out of scope, but receivers SHOULD treat zerowire-injected HID as untrusted (no autoadmin escalation).

## 6. Per-platform notes

### 6.1 Sender — Android

- Min SDK 26 (Android 8) for USB host APIs we rely on.
- `USB_PERMISSION` broadcast per device on first plug.
- `UsbDeviceConnection.bulkTransfer` for non-HID; for HID, we read interrupt endpoints directly.
- mDNS via `NsdManager` initially; consider `jmdns` if we need finer control.
- Foreground service while at least one receiver is connected. Persistent notification with a kill switch.
- Battery: with ~10 receivers idle, target <2% drain/hour. Background scanning is off by default.

**Risks:** Some Android OEMs disable USB host on certain SKUs; we detect and refuse to start with a clear error. Some kernels don't expose isochronous endpoints to userspace — webcams/audio will not work on those devices and we surface that in the device list.

### 6.2 Receiver — Linux

- Userspace daemon `zerowired` runs as the user; uses `pkexec`/`polkit` for the one privileged action: writing to `/sys/devices/platform/vhci_hcd.0/attach`.
- We require `vhci-hcd` available. `modprobe vhci-hcd` is performed on first run (with a polkit prompt) and a config snippet is dropped into `/etc/modules-load.d/zerowire.conf`.
- Tray UI: GTK (`gtk-rs`) initially; status, list of mounted devices, pairing wizard.
- Distribution: Flatpak (with the `--device=all` permission for vhci) + .deb + .rpm.

**Risks:** Flatpak's sandbox doesn't love `/sys` writes; we may end up shipping the daemon outside Flatpak and only sandbox the UI. .deb/.rpm first.

### 6.3 Receiver — Windows

- Bundle `usbip-win2` (BSD-2-Clause). Their signed driver is the unblocker; we do not ship our own driver in v1.
- Installer: MSIX or WiX. Drops a Windows Service `zerowired.exe` that owns the TCP session, plus the bundled `usbip-win2` ioctl userspace.
- Tray UI: a small WinUI 3 / Win32 app.

**Risks:** Microsoft can revoke driver signatures. We mitigate by tracking upstream `usbip-win2` releases and shipping their latest signed driver promptly.

### 6.4 Receiver — macOS

- DriverKit System Extension implementing `IOUserUSBHostDevice` to register a synthetic USB device with IOKit, then translate URBs to/from our TCP stream.
- Code-signed with a Developer ID + notarized + (for sysext) requires the `com.apple.developer.driverkit.transport.usb` entitlement granted by Apple. Lead time on that entitlement is weeks; we plan it now.
- Fallback if entitlement isn't granted by v1 launch: HID-only mode (we implement a virtual HID provider — same approach as Karabiner-Elements — and lean on the HID fast lane).

**Risks:** Notarization can stall a release. HID-only fallback shipped first is acceptable.

### 6.5 Receiver — Android

- HID injection via Accessibility Service is the only zero-root option. We `dispatchGesture`/synthesize key events from `REPORT_IN` payloads after parsing them against the HID report descriptor we got at `BIND_ACK`.
- Non-HID devices on Android receivers expose a **companion API** (AIDL service) — third-party apps can claim a remote USB device and talk USB/IP via our service. We do not try to inject a virtual USB host on Android in v1.

### 6.6 Receiver — iOS

- iOS does not let third-party apps inject pointer or keyboard input at the system level. The only realistic path is **Bluetooth HID emulation**: the iOS app advertises itself as a Bluetooth keyboard/mouse; the iPhone treats itself as the BT host of itself? — no. Correction:
- The **sender** (the Android phone) advertises over Bluetooth as a BT keyboard/mouse to the iPhone, after handshaking with the iOS app over WiFi to share configuration. The iOS app's role is paired configuration + status; the actual HID delivery is over BT classic HID profile, not WiFi.
- Result: iOS is HID-only AND requires the sender to also be in Bluetooth range. We are honest about this in the docs.

## 7. Open risks (tracked)

| # | Risk | Mitigation |
|---|------|-----------|
| R1 | Apple DriverKit entitlement denied | HID-only macOS fallback ready before launch |
| R2 | Android OEM disables USB host | Detect + refuse with clear error; documented unsupported list |
| R3 | Latency over crowded 2.4 GHz | HID fast lane + small frames + TCP_NODELAY; advertise 5 GHz/wired requirement for gaming |
| R4 | `usbip-win2` driver signature revoked | Track upstream; have a backup plan to fund a re-sign if needed |
| R5 | Isochronous endpoints (audio/video) unsupported on Android | Document; v2 explores `UsbDeviceConnection.requestWait` with explicit iso URBs where the kernel allows |
| R6 | iOS BT HID emulation needs sender BT — surprise for users | Surfaced in onboarding; not hidden in fine print |
| R7 | Per-device authorization UX fatigue | "Remember this device for this receiver" default; clear revoke screen |

## 8. Versioning & compatibility

- Wire protocol version is in the envelope. v1 senders refuse v2 receivers cleanly (CONTROL `HELLO_ACK` with `error: "version_mismatch"`).
- We maintain a compatibility matrix in `protocol/COMPAT.md` once we ship more than one minor version.

---

builtbyzero · zerowire · v0 architecture
