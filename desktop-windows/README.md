# zerowire — Windows receiver (placeholder)

Not implemented yet. Plan:

- Bundle [`usbip-win2`](https://github.com/vadimgrn/usbip-win2) (BSD-2-Clause). Their signed kernel driver does the USB-side work.
- Windows Service `zerowired.exe` owns the TCP/TLS session and shovels USB/IP bytes into the `usbip-win2` ioctl.
- Tray UI in WinUI 3 (or plain Win32) for pairing + active-device list.
- Installer: MSIX or WiX.

See [`../ARCHITECTURE.md`](../ARCHITECTURE.md) §6.3 for design notes and risks.
