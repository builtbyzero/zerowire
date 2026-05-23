# zerowire — iOS receiver (placeholder)

Not implemented yet, and honest about why.

iOS does not let third-party apps inject pointer or keyboard input at the system level. There is no "iOS USB/IP client" path. The only realistic option is to deliver input via the standard **Bluetooth HID** profile.

## Plan

- The sender phone (Android) advertises as a Bluetooth keyboard/mouse to the iPhone over BT Classic HID. Input reports go over BT, not WiFi.
- The **iOS app**'s role is: pairing UX, configuration of which sender device feeds which physical USB device, and a status view. It also runs the WiFi side of the zerowire handshake (TLS + PSK) for trust pinning.
- This means iOS use requires the sender to be in Bluetooth range, not just WiFi range. We will surface this honestly during onboarding — no fine-print surprises.

See [`../ARCHITECTURE.md`](../ARCHITECTURE.md) §6.6 (and Risk R6 in §7).
