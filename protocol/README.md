# zerowire-protocol

Shared wire-protocol crate. No I/O — pure encode/decode + types.

- `envelope` — frame format on the wire (`ZW` magic, version, channel, length, payload).
- `usbip` — kernel.org USB/IP op codes + the zerowire `import_id` multiplexing wrapper.
- `hid` — the HID fast-lane sub-protocol.
- `control` — JSON message shapes for the handshake / device list / attach flow.
- `discovery` — mDNS service constants + TXT-record helpers.

## Build

```bash
cargo build
cargo test
```

19 unit tests cover round-trips and error paths. See [`../ARCHITECTURE.md`](../ARCHITECTURE.md) for the full protocol spec.
