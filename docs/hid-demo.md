# HID fast-lane demo

> **What this is:** the first wire that actually does something useful.
> A USB mouse plugged into an Android phone moves the cursor on a Linux box
> over WiFi. Same for keyboard keystrokes.
>
> **What this isn't yet:** general USB passthrough, TLS-secured pairing,
> a slick UI. Those are tracked in the v0.2+ roadmap in the top-level README.

## Architecture (this slice only)

```
┌────────────────────────────┐                ┌────────────────────────────┐
│ Android phone (sender)     │                │ Linux machine (receiver)   │
│                            │                │                            │
│  USB-C mouse ─► UsbManager │                │                            │
│           │                │                │  zerowire-cli receive      │
│           ▼                │                │           │                │
│  HID report bytes          │                │           ▼                │
│           │                │                │  /dev/uinput               │
│           ▼                │                │     virtual mouse          │
│  HidOp::ReportIn frame ────┼── envelope ───►│           │                │
│           on Channel::Hid  │   over TCP/WiFi│           ▼                │
│                            │                │  X / Wayland sees a real   │
│                            │                │   mouse, cursor moves      │
└────────────────────────────┘                └────────────────────────────┘
```

Multiplexing: every byte after handshake is wrapped in a 8-byte
**zerowire envelope** (`ZW` + version + channel + length). HID traffic
rides on channel `0x03`; control JSON rides on `0x01`. Same socket. See
`protocol/src/envelope.rs` and `protocol/src/hid.rs`.

## What's verified today

| Path                                                 | Status |
| ---------------------------------------------------- | :----: |
| Protocol crate (envelope, HID frame, BindAckMeta)    |   ✅   |
| `zerowire-mock-sender` <-> `zerowire-cli receive`    |   ✅   |
| `zerowire-cli receive --simulate` opens uinput       |   ⚠️   |
| Android USB-host claim → HID reports on the wire     |   ⛔   |
| Phone-to-laptop **with real hardware**               |   ⛔   |

* ✅ = covered by `cargo test` + `tests/hid_loopback.sh` and passes on a
  clean checkout.
* ⚠️ = code path exercised, but the `/dev/uinput` open itself requires
  permissions this build host doesn't have. Returns a clean, actionable
  error message; the surrounding code is unit-tested.
* ⛔ = code shipped, untested. We don't have an Android dev environment on
  this build host. Manual test plan is below.

## Try it on Linux (no phone needed)

You'll need: a Linux box with the `uinput` kernel module, a recent Rust
toolchain, and (one-time) udev permission for `/dev/uinput`. See
[`desktop-linux/udev/README.md`](../desktop-linux/udev/README.md).

```bash
git clone https://github.com/builtbyzero/zerowire
cd zerowire/desktop-linux
cargo build --bin zerowire-cli --bin zerowire-mock-sender

# Terminal 1: pretend to be the Android sender on the loopback port.
./target/debug/zerowire-mock-sender --reports 200 --interval-ms 30

# Terminal 2: real receiver, pulls reports through /dev/uinput.
sudo modprobe uinput   # once per boot
./target/debug/zerowire-cli receive --target 127.0.0.1:47823
```

If `/dev/uinput` is writable, you'll see a virtual mouse appear:

```bash
# Terminal 3: confirm the kernel sees us.
sudo libinput list-devices | grep -A2 zerowire
# Or: cat /proc/bus/input/devices | grep -A4 zerowire
```

The cursor on your screen will twitch right ~200 times over ~6 seconds.

### Without `/dev/uinput`

Don't have permission? Skip the kernel side, just verify the wire path:

```bash
./target/debug/zerowire-cli receive \
    --target 127.0.0.1:47823 \
    --simulate-source /tmp/zerowire-reports.log
cat /tmp/zerowire-reports.log
# bind_ack kind=Mouse name="zerowire-mock mouse"
# report seq=1 bytes=[0, 3, 0, 0]
# report seq=2 bytes=[0, 3, 0, 0]
# ...
# summary bind_ack=true reports=200
```

That's what `tests/hid_loopback.sh` does in CI.

### `--simulate` (no sender, no phone)

```bash
./target/debug/zerowire-cli receive --simulate
```

Synthesizes mouse jitter directly into `/dev/uinput`. Use this when you
want to confirm uinput permissions before bothering with the sender side.

## Try it with a real Android phone (manual test plan)

> **Honesty:** the Android side compiled and was code-reviewed but **was
> not run against real hardware on this build host**. Treat this section as
> a smoke-test plan, not a confirmation.

1. Build the sender:

   ```bash
   cd android-sender
   ./gradlew assembleDebug
   adb install -r app/build/outputs/apk/debug/app-debug.apk
   ```

2. Plug a USB OTG cable into the phone's USB-C port and connect a mouse.
   Android should prompt: *"Allow zerowire to access USB device?"* — say
   yes (tick "always" to skip on reconnect).

3. The sender's notification should switch from "Idle" to
   *"1 device shared (Logitech …)"*.

4. On Linux:

   ```bash
   zerowire-cli discover           # should list the phone by name
   zerowire-cli receive --sender "Pixel 8"
   ```

5. **Expected**: cursor moves on Linux when you move the mouse plugged
   into the phone. Buttons click. Wheel scrolls.

6. **Known v0.1 limitations**:
   - **Pairing is plaintext.** No TLS, no PSK proof. Anybody on the same
     LAN can connect. Real auth lands in v0.2.
   - **No reconnect.** If WiFi blips, you have to re-run `receive`.
   - **High-DPI gaming mice may misbehave.** We assume the *boot* mouse
     report format (3- or 4-byte: buttons, dx, dy, wheel). Mice that
     ship 16-bit deltas need their report descriptor parsed, which is
     a planned follow-up.
   - **Multimedia keys won't fire.** Only the standard 104-key set is
     mapped today (`hid_usage_to_linux_key` in `receiver.rs`).
   - **The Android pairing UI is ugly.** It's a placeholder text screen;
     the 6-digit code / QR flow is sketched in `PairingCodes.kt` but not
     yet wired to the session loop.

## What's next

- TLS + PSK pairing on the control channel (ARCHITECTURE.md §6.2).
- Parse the report descriptor on the receiver side so non-boot devices
  work without sender-side translation.
- Wire up the `zerowire-sender` accessory APK to do real claim + report
  reading once we have a test phone.
- USB/IP fast lane on channel `0x02` for non-HID devices on Linux.
