# zerowire — Linux receiver

The Linux receiver is a userspace daemon that drives the in-tree `vhci-hcd`
(Virtual Host Controller Interface) kernel module. We don't ship our own
kernel driver; we lean on what the kernel already has.

## Status

**v0.1 HID fast lane is live.** This crate ships:

* `zerowire-cli discover` — mDNS browse for `_zerowire._tcp.`.
* `zerowire-cli connect <host:port>` — TCP + HELLO/HELLO_ACK.
* `zerowire-cli list <host:port>` — + `LIST_DEVICES`.
* `zerowire-cli receive` — full HID fast-lane session: discover, attach,
  bind, push reports through `/dev/uinput`.
  * `--target host:port` — skip mDNS, dial directly.
  * `--sender <name>` — pick by mDNS TXT name.
  * `--busid <id>` — pick a specific device on the sender.
  * `--simulate` — fake mouse jitter into a real virtual device. No
    network. Use to validate `/dev/uinput` permissions.
  * `--simulate-source <log>` — dial a sender but log report bytes to
    a file instead of injecting them. Used by `tests/hid_loopback.sh`.
* `zerowire-mock-sender` — stand-in for the Android sender. Loops back
  to localhost for integration testing and ad-hoc dev.

No TLS yet (plaintext handshake, ARCHITECTURE.md §6.2 is unchanged).
USB/IP passthrough on channel `0x02` is still placeholder. The HID fast lane
on channel `0x03` is real and end-to-end.

## Build & run

```bash
cd desktop-linux
cargo build
./target/debug/zerowire-cli discover
```

For the HID demo, see [`../docs/hid-demo.md`](../docs/hid-demo.md). For
the one-time `/dev/uinput` permission setup, see [`udev/README.md`](./udev/README.md).

## `vhci-hcd` integration plan

Required at runtime:

```bash
sudo modprobe vhci-hcd
```

We will:

1. Ship `/etc/modules-load.d/zerowire.conf` containing `vhci-hcd` so the
   module loads on boot.
2. On first run, the daemon detects whether `vhci-hcd` is loaded by stat-ing
   `/sys/devices/platform/vhci_hcd.0/`. If missing, it shells out to
   `pkexec modprobe vhci-hcd` and waits for the sysfs entries to appear.
3. To attach a remote device, the daemon writes
   `"<port> <socketfd> <devid> <speed>"` to
   `/sys/devices/platform/vhci_hcd.0/attach`. The kernel takes ownership of
   the socket file descriptor and from then on drives the USB/IP protocol
   directly — userspace stays out of the per-URB hot path.
4. The socket we hand to the kernel is the **post-TLS** socket. Because the
   kernel speaks raw USB/IP, we use a small proxy: zerowire opens a Unix
   socketpair, gives one end to the kernel via the `attach` ioctl, and on
   the other end translates between zerowire envelopes (with the
   `import_id` prefix on channel 0x02) and plain USB/IP that the kernel
   expects.
5. Detach via writing the port number to `/sys/.../detach`.

We need polkit to authorize the sysfs writes; the Linux package will ship a
`org.builtbyzero.zerowire.policy` file that asks the user once per session.

## Distribution plan

* `.deb` and `.rpm` first, with a systemd user unit (`zerowired.service`).
* Tray UI later (GTK via `gtk-rs`).
* Flatpak last — the `--device=all` permission and `/sys` writes need
  manifest engineering we'll do once the daemon is real.
