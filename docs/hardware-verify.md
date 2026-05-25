# Hardware verification — Android sender vs. Linux receiver

> **Purpose.** Take the Android sender from "compiles" to "verified working
> with a real USB device against the Linux receiver on this laptop." This
> is the runbook that Nilesh executes physically; success or failure of
> each stage is unambiguous and recorded in the matrix at the bottom.
>
> **Scope.** v0.1 HID fast lane only. We are *not* testing USB/IP, TLS, or
> non-HID devices here — those are v0.2+ work.
>
> **Canonical test rig.** Pixel-class Android phone + a USB-C OTG cable +
> a boot-protocol USB mouse + this Linux laptop on the same WiFi.

## 0. Pre-flight — collect the gear

| Item | Why | Notes |
|---|---|---|
| Pixel-class Android phone, USB-C, Android 13+ | Sender host | Phone must support USB Host mode. Some carrier-locked phones disable it; verify by plugging any USB device through OTG and confirming Android shows a system prompt. |
| USB-C **OTG** cable or USB-C-to-USB-A adapter | OTG host link | A normal charging cable will NOT enumerate devices. Tag it physically — easy to mix up later. |
| Boot-protocol USB mouse | Stable known-good HID device | Logitech M90/M100, Dell MS116, any cheap office mouse. **Avoid gaming mice** (high-DPI 16-bit deltas don't decode as boot reports — see `docs/hid-demo.md` §6). |
| (Optional) Boot-protocol USB keyboard | Regression check | Same caveat: avoid NKRO mechanical keyboards for the first pass. |
| WiFi AP that both devices share | Transport | Same SSID, same /24. Verify with `ping` from the laptop to the phone's IP (you'll get the IP from the sender app's diagnostic line). |
| Laptop running this repo on Linux | Receiver host | Needs `/dev/uinput` access — see Stage 1. |
| `adb` installed on the laptop | Sender logging | `sudo apt install android-tools-adb` on Debian/Ubuntu. |

## 1. Laptop receiver: smoke-test that uinput works

We confirm the **receiver half** is healthy *before* introducing the phone.
If this stage fails, no amount of Android debugging will help.

```bash
cd desktop-linux
cargo build --bin zerowire-cli --bin zerowire-mock-sender

# Either:                  (one-time root install — see desktop-linux/udev/README.md for a daemon-friendly udev rule)
sudo modprobe uinput
sudo ./target/debug/zerowire-cli receive --simulate
# …or, if you already followed the udev/group setup, no sudo:
./target/debug/zerowire-cli receive --simulate
```

**Expect.** A new virtual mouse appears and your cursor starts moving in
a slow Lissajous-shaped arc. In another terminal:

```bash
sudo libinput list-devices | grep -B1 -A2 "zerowire: simulated mouse"
```

**Pass criterion.** `libinput list-devices` lists `zerowire: simulated
mouse`, and your cursor visibly moves. Ctrl-C to stop.

**If it fails:** see `desktop-linux/udev/README.md`. Don't proceed until
this works — every later stage builds on it.

## 2. Build and install the Android sender

```bash
cd android-sender

# One-time: tell Gradle where the SDK lives. Replace with your path.
echo "sdk.dir=$HOME/Android/Sdk" > local.properties

./gradlew assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

**Expect.** `BUILD SUCCESSFUL` from Gradle and `Success` from adb.

**Pass criterion.** `adb shell pm list packages | grep zerowire` prints
`package:zero.builtby.zerowire.sender`.

**Common failure:** missing `compileSdk 34` platform → install via
`sdkmanager 'platforms;android-34' 'build-tools;34.0.0'`.

## 3. Launch the sender, confirm it advertises on the LAN

Open the **zerowire** app on the phone (do not plug in the mouse yet).

**On the phone, expect to see:**

- Title: `zerowire`
- Status: `Advertising on the LAN.`
- A **diagnostic line** in monospace: `listen=192.168.x.y:47823  mDNS✓  sessions=0`
  - `listen=...` shows the WiFi IP and bound port. **Write this IP down** —
    you'll need it if mDNS fails (Stage 5).
  - `mDNS✓` means `_zerowire._tcp.local.` was registered with `NsdManager`.
    `mDNS…` means still pending (give it ~2s); `mDNS✗` would mean failure
    and you should check `adb logcat` (Stage 7).
  - `sessions=0` — no receivers connected yet.
- The pairing code (informational only in v0.1).
- "Plugged-in USB devices: None" (nothing plugged yet).

**On the laptop, run:**

```bash
./target/debug/zerowire-cli discover
```

**Expect.** One line listing the phone, e.g.:

```
Pixel 8 @ 192.168.1.42:47823  id=<uuid>  v=1  caps=usbip,hid
```

(If you have multiple Androids on the LAN, the name comes from
`Build.MODEL` — `Pixel 8`, `SM-G998B`, etc.)

**Cross-check with `avahi-browse`** (more verbose, useful if `discover`
returns empty):

```bash
avahi-browse -rt _zerowire._tcp
```

**Pass criterion.** Either `zerowire-cli discover` or `avahi-browse` lists
the phone with port 47823 and the TXT records `v=1`, `caps=usbip,hid`.

**If both fail:**
1. Are they on the same SSID? Many home routers isolate IoT / guest VLANs.
2. Does mDNS work *at all* on this LAN? Try `dns-sd -B _services._dns-sd._udp local.`
3. Use the IP from the phone's diagnostic line for `--target` (Stage 5)
   — mDNS is a convenience, the rest of the protocol doesn't need it.

## 4. Plug in the USB mouse, confirm Android sees it

Connect the OTG cable to the phone, then the mouse to the cable.

**On the phone, expect:**

- A system dialog: **"Allow zerowire to access USB device?"** Tap **OK**
  (tick "Use by default for this USB device" to skip on reconnect).
- The "Plugged-in USB devices" section now lists the mouse, e.g.:

```
002          046d:c077  [HID]  OK   USB Optical Mouse
```

The columns are: busid suffix · vendor:product · `[HID]` if any
interface is HID class 0x03 · permission state (`OK` once granted,
`NEEDS_PERM` if not).

**Cross-check via `adb`:**

```bash
adb logcat -s zerowire/Main:V zerowire/SenderService:V
# expect lines like:
#   zerowire/Main: requesting USB permission for /dev/bus/usb/001/002 046d:c077
#   zerowire/Main: USB permission broadcast received; refreshing device list
```

**Pass criterion.** The mouse appears in the on-screen list with `[HID]`
and `OK`. If it shows `NEEDS_PERM` permanently, the system dialog was
dismissed — unplug + replug to retrigger.

## 5. Receiver connects, lists devices

Two options — use whichever is convenient. Discovery is the happy path;
direct IP is the fallback for hostile networks.

### Option A — via mDNS discovery

```bash
./target/debug/zerowire-cli list 192.168.1.42:47823
# … or, by sender name:
./target/debug/zerowire-cli receive --sender "Pixel 8"   # blocks; advance to Stage 6
```

### Option B — direct IP

```bash
# Use the IP printed in the phone's diagnostic line:
./target/debug/zerowire-cli list 192.168.1.42:47823
```

**Expect.** A table:

```
busid       vid:pid     hid   name
----------  ----------  ----  ------------------------------
1-2         046d:c077   yes   USB Optical Mouse
```

**On the phone, expect the diagnostic line to update:**

```
listen=192.168.1.42:47823  mDNS✓  sessions=1
```

(`sessions` ticks up the moment the receiver connects, and back to 0 when
the receiver's `list` command exits.)

**Pass criterion.** The mouse appears in the table with `hid=yes` and the
phone's `sessions` count incremented at least once.

**If the receiver hangs on connect:** firewall, almost certainly. Confirm
with `nc -vz <phone-ip> 47823` from the laptop; if that hangs, your
router or phone's hotspot is blocking client-to-client TCP.

## 6. The big one — bind HID and verify the cursor moves

Start the receiver in HID-bind mode, with the diagnostic trace turned on
so we can prove what happened later:

```bash
./target/debug/zerowire-cli receive \
    --target 192.168.1.42:47823 \
    --diagnose /tmp/zw-diag.jsonl
```

In another terminal:

```bash
tail -f /tmp/zw-diag.jsonl
```

**On the phone, the diagnostic line should now show `sessions=1` and the
foreground notification should read `Connected: 1 session(s)`.**

**On the laptop, in the `tail -f` window**, expect (one JSON line per
event, abbreviated):

```json
{"event":"start", "data":{ "direct_target":"192.168.1.42:47823" }}
{"event":"dial_ok"}
{"event":"hello_ack", "data":{ "name":"Pixel 8", ... }}
{"event":"device_list", "data":{ "count":1, "devices":[{"busid":"1-2","is_hid":true,...}] }}
{"event":"attach_request", "data":{ "busid":"1-2", "mode":"hid" }}
{"event":"attach_ok"}
{"event":"bind_sent"}
{"event":"bind_ack", "data":{ "kind":"Mouse", "report_descriptor_len":68 }}
{"event":"uinput_created", "data":{ "name":"zerowire: USB Optical Mouse" }}
{"event":"report", "data":{ "seq":1, "bytes":[0,1,0,0] }}
…
{"event":"report_rate", "data":{ "reports_per_s":125.3, ... }}
```

**Now move the mouse.** Cursor should move on the laptop.

**Cross-check the kernel side:**

```bash
sudo libinput list-devices | grep -A2 "zerowire: USB Optical"
# … or, for a sub-millisecond trace of each event:
sudo evtest                 # pick "zerowire: USB Optical Mouse" from the list
```

`evtest` should show `EV_REL REL_X` / `REL_Y` events firing while you
move the mouse, and `EV_KEY BTN_LEFT 1` / `0` when you click.

**Pass criterion.** All four must hold:
1. `bind_ack` event appears in `/tmp/zw-diag.jsonl`.
2. `report_rate.reports_per_s > 0` while you're actually moving the mouse.
3. The OS-level cursor visibly moves on the laptop screen.
4. Clicks register in `evtest` as `BTN_LEFT/RIGHT/MIDDLE` events.

## 7. Regression checks (do them while still connected)

Test each in turn. Each is a row in the matrix below.

| Action | Expect |
|---|---|
| Left-click | `EV_KEY BTN_LEFT 1` then `0` in evtest; cursor responds in your DE. |
| Right-click | `EV_KEY BTN_RIGHT`. Context menu in your DE. |
| Middle-click | `EV_KEY BTN_MIDDLE`. |
| Scroll wheel up | `EV_REL REL_WHEEL +1`. Page scrolls. |
| Scroll wheel down | `EV_REL REL_WHEEL -1`. |
| Plug a USB keyboard instead | New `bind_ack` with `kind:"Keyboard"`. Typing letters appears in a focused terminal. |

If you swap the mouse for a keyboard mid-session, you have to **restart
the receiver** — v0.1 binds one device per session.

## 8. Tear-down checks

1. Unplug the mouse from the phone.
2. The receiver should print `peer gone:` (broken pipe) within ~1s and
   exit cleanly. Cursor stops moving on the laptop.
3. The phone's diagnostic line returns to `sessions=0`. Notification
   returns to `Idle — advertising`.
4. `sudo libinput list-devices` no longer lists `zerowire: USB Optical
   Mouse` (the receiver destroys the uinput device on shutdown).

## 9. Capturing evidence for the matrix

After a full pass, attach the following to the PR / issue:

```bash
# Diagnostic trace from the receiver:
cp /tmp/zw-diag.jsonl ~/zw-verify-$(date +%Y%m%d).jsonl

# Android logs (last 5 minutes):
adb logcat -d -t 300 -s zerowire/Main:V zerowire/SenderService:V zerowire/Session:V zerowire/HidEndpoint:V \
    > ~/zw-verify-$(date +%Y%m%d).logcat

# Receiver log:
./target/debug/zerowire-cli receive --target … --diagnose … 2>&1 | tee ~/zw-verify-$(date +%Y%m%d).rx.log

# Packet capture (optional but gold for debugging):
sudo tcpdump -i any -w ~/zw-verify-$(date +%Y%m%d).pcap "host <phone-ip> and port 47823"
```

## Pass/fail matrix

Fill this in as you go. One row per stage. "N/A" = couldn't run; "skip" =
explicitly out of scope for this pass.

| # | Stage | Result | Evidence path | Notes |
|---|---|:-:|---|---|
| 1 | `--simulate` cursor jitter | ☐ | | |
| 2 | APK builds + installs | ☐ | | |
| 3 | mDNS broadcast observed | ☐ | | (Or note: used direct IP) |
| 4 | Mouse enumerated on phone + permission granted | ☐ | | |
| 5 | Receiver `list` shows the mouse | ☐ | | |
| 6 | `bind_ack` + cursor moves | ☐ | | |
| 7a | Left-click | ☐ | | |
| 7b | Right-click | ☐ | | |
| 7c | Middle-click | ☐ | | |
| 7d | Scroll wheel | ☐ | | |
| 7e | Keyboard (regression) | ☐ | | (Only if a keyboard was on hand.) |
| 8 | Clean tear-down on unplug | ☐ | | |

**Verdict** (circle one): **PASS** — v0.1 HID fast lane is hardware-verified.
**PASS-WITH-NOTES** — works but caveats apply.
**FAIL** — at least one row is ☒; file a bug, do not ship.

## Troubleshooting

### Receiver hangs at "discovering sender…"

mDNS isn't reaching the laptop. Try direct-IP:

```bash
./target/debug/zerowire-cli receive --target <phone-ip>:47823 --diagnose /tmp/zw-diag.jsonl
```

The phone's diagnostic line always shows its current LAN IP, even if
mDNS fails.

### `openDevice returned null (missing permission?)` in logcat

The user dismissed the USB permission dialog. Re-launch the app, or
unplug + replug the mouse to retrigger.

### `failed to claim HID interface`

Another process on the phone has the device claimed. Common culprits:
Android's built-in mouse driver (yes — Android binds USB mice as native
pointers automatically). Workarounds:
- Toggle "Disable USB host stack" in Developer Options and back — usually
  releases the claim.
- In ARCHITECTURE.md §3.4 we describe a planned `USB Accessory` carve-out
  to bypass this; not yet implemented in v0.1.

### `report_descriptor_len: 0` in `bind_ack`

The HID class control transfer (`GET_DESCRIPTOR(Report)`) was rejected by
the phone's USB host stack. The receiver still works for boot-protocol
mice (because it doesn't strictly need the descriptor on v0.1's
boot-format decoder), but log this as a known bug — some Android OEMs
strip class-specific descriptor requests. We'll fall back to parsing the
raw config descriptor in a follow-up.

### Cursor moves but the wrong way / wrong distance

Likely a non-boot mouse (16-bit deltas, packed differently). The
receiver's `apply_mouse_report` only handles the 3- or 4-byte boot
report. Try a cheaper mouse for the v0.1 pass; sender-side report
translation is on the v0.2 list.

### `seq jump: 1234 -> 1240 (drops or reorder)` in the diag log

WiFi packet loss. If the rate stays high (`report_rate.reports_per_s` ≈
device rate, e.g. 125 for boot, 1000 for high-rate), you're fine — TCP
will reorder/recover, the gap was just the trace catching a retransmit.
If the rate collapses, the AP is congested.

## Blockers — physical actions Nilesh must take

Before the first run-through, these are non-negotiable and require
hands-on the hardware:

1. **Have an Android device with USB Host enabled.** Most Pixels qualify;
   some carrier Samsung builds disable it. Confirm by plugging *any* USB
   device through OTG and watching for the system "USB device attached"
   prompt.
2. **A USB-C OTG cable.** A charging-only cable will not enumerate. Buy
   the cheapest "USB-C to USB-A female" adapter you can find.
3. **A boot-protocol USB mouse.** Logitech M90, Dell MS116, Microsoft
   Wired 600 — any office-class HID mouse. Avoid: high-DPI gaming mice
   (Razer, Logitech G-series), trackballs, anything that ships with its
   own driver software.
4. **Both devices on the same WiFi /24.** Not just the same SSID — also
   verify with `ping` that the laptop can reach the phone. Many home
   routers segregate "Guest" and "IoT" networks.
5. **One-time:** install the `/dev/uinput` udev rule so the receiver can
   run as your normal user. See `desktop-linux/udev/README.md`. Five
   minutes of one-time setup, then it stays.
6. **One-time:** `adb` installed and the phone in USB debugging mode
   (Developer Options → USB debugging) so logcat tags are accessible. Not
   strictly required to *use* the app, but required to debug if anything
   goes wrong.

## What we explicitly didn't test (out of v0.1 scope)

- High-DPI / NKRO / gaming HID devices — report-descriptor parsing is
  v0.2 work.
- Roaming / reconnect — current v0.1 assumes the session lives or dies
  with the TCP connection.
- Pairing UI: the 6-digit code is informational, not enforced.
- Non-HID USB devices (mass storage, MIDI, label printers): USB/IP fast
  lane is v0.2 (`ARCHITECTURE.md` §6.3).
- Multi-receiver: only one receiver session at a time is tested. The
  service supports more but it's not part of v0.1 SLA.
- TLS / PSK: still plaintext on the control channel. Anybody on the LAN
  can connect. v0.2.
