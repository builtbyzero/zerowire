# zerowire — Android receiver (placeholder)

Not implemented yet. Plan:

- **HID injection** via Accessibility Service. We parse `REPORT_IN` payloads against the HID report descriptor delivered at `BIND_ACK` and synthesize pointer / key events with `dispatchGesture` and `performGlobalAction`.
- **Non-HID devices**: companion API (AIDL service) so third-party Android apps can claim a remote USB device and speak USB/IP through us. No system-level virtual USB host is possible on Android without root.

See [`../ARCHITECTURE.md`](../ARCHITECTURE.md) §6.5.
