# zerowire — macOS receiver (placeholder)

Not implemented yet. Plan:

- DriverKit System Extension implementing `IOUserUSBHostDevice` so the OS treats remote USB devices as if they were locally plugged in.
- Companion app (SwiftUI) for pairing + tray UI.
- Code-signed with Apple Developer ID, **notarized**, and (for the sysext) requires the `com.apple.developer.driverkit.transport.usb` entitlement from Apple. Lead time on the entitlement is weeks — we request it as soon as the design is locked.
- **Fallback if entitlement is delayed:** HID-only mode using a virtual HID provider (same approach as Karabiner-Elements). Driven by the HID fast lane from the protocol crate; no USB stack needed.

See [`../ARCHITECTURE.md`](../ARCHITECTURE.md) §6.4 for design notes and risks (Risk R1).
