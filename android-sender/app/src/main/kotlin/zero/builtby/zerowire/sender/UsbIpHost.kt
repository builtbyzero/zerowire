package zero.builtby.zerowire.sender

import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbDeviceConnection
import android.util.Log
import java.io.OutputStream

/**
 * Sender-side USB/IP host-export — stub.
 *
 * # What this would do on a "real" Linux box
 *
 * The kernel ships a `usbip-host` driver: you load it, `usbip bind -b 1-2`
 * detaches the device from its native driver and rebinds it to
 * `usbip-host`, and `usbip-host` then accepts an inbound TCP socket and
 * pumps URBs in both directions. Companion userspace (`usbipd`) handles
 * the protocol-level handshake (OP_REQ_DEVLIST / OP_REQ_IMPORT) before the
 * kernel takes the socket.
 *
 * # Why this is a stub on Android
 *
 * AOSP **does not** ship `usbip-host` in any released kernel config:
 *
 * | Kernel knob                | Android upstream status         |
 * | -------------------------- | ------------------------------- |
 * | `CONFIG_USBIP_VHCI_HCD`    | not enabled                     |
 * | `CONFIG_USBIP_HOST`        | not enabled                     |
 * | `CONFIG_USBIP_CORE`        | not enabled                     |
 *
 * Even on rooted devices, the modules aren't built and the
 * `/sys/devices/platform/usbip_host` interface doesn't exist. Approaches
 * to bridge this gap:
 *
 * 1. **Userspace USB/IP server**, talking to Android's `UsbDeviceConnection`
 *    via `bulkTransfer` / `controlTransfer`. We re-implement the URB
 *    multiplexing in Kotlin/native. This is what we will pursue in v0.3.
 *    Bandwidth is fine for mass-storage and audio; latency is unsuitable
 *    for time-sensitive HID, which is why v0.2 keeps the HID fast lane.
 * 2. **Custom kernel build** with `CONFIG_USBIP_HOST=y`. Possible on a
 *    GrapheneOS Pixel or similar; would require a custom OEM build that
 *    we don't ship.
 *
 * # What this stub provides
 *
 * Just enough scaffolding so the v0.2 protocol message
 * (`ATTACH_OK_USBIP`) can be emitted from a real Android session: the
 * device descriptor is real (read off the device); the URB pump is the
 * `UnsupportedOperationException` path. The receiver-side mock + tests
 * exercise the wire format end-to-end already; the gap is purely the
 * sender-side URB pump.
 *
 * **Hardware verification status:** the descriptor read path is
 * code-reviewed against `UsbDeviceConnection.getRawDescriptors()`; not
 * yet run on a real phone. The URB pump is unbuilt by design (see above).
 */
class UsbIpHost(
    private val device: UsbDevice,
    private val conn: UsbDeviceConnection,
) {
    /**
     * Read the 18-byte `usb_device_descriptor` for `device`. Layout
     * matches USB 2.0 §9.6.1 — bus-order is little-endian, exactly what
     * the receiver wants in `UsbipAttachInfo.descriptor_hex`.
     *
     * `UsbDeviceConnection.getRawDescriptors()` returns the whole
     * descriptor chain (device + config + interface + endpoint); the
     * first 18 bytes are the device descriptor.
     */
    fun deviceDescriptor(): ByteArray {
        val raw = conn.rawDescriptors ?: throw IllegalStateException(
            "no descriptors — device disconnected?"
        )
        if (raw.size < 18) {
            throw IllegalStateException("raw descriptors too short: ${raw.size}")
        }
        return raw.copyOfRange(0, 18)
    }

    /**
     * USB/IP speed code, per `linux/usb/ch9.h:usb_device_speed`. Best
     * effort on Android: `UsbDevice` doesn't expose speed directly so we
     * fall back to High (3) which is the right answer for the vast
     * majority of USB-A and USB-C devices on a phone. The receiver's
     * vhci-hcd uses the speed only as a hint to pick an internal port
     * descriptor.
     */
    fun speed(): Int = 3 // High

    /**
     * v0.3 entry point. Today: explicit `NotImplementedError`.
     *
     * What this _will_ do once built: continuously read URBs off `socketOut`,
     * dispatch each to the right endpoint via
     * `UsbDeviceConnection.bulkTransfer/controlTransfer/requestWait`, and
     * push the completion back through `socketOut` framed as a
     * `UsbipFrame`. The receiver's vhci-hcd treats this stream as a
     * standard usbip socket.
     */
    fun pumpUrbs(@Suppress("unused") socketOut: OutputStream): Nothing {
        Log.w(TAG, "pumpUrbs called on stub — see UsbIpHost.kt doc for status")
        throw NotImplementedError(
            "Sender-side USB/IP URB pump not implemented in v0.2. " +
                "AOSP doesn't ship the usbip-host kernel driver; the userspace " +
                "URB-pump shim is tracked for v0.3. The receiver + protocol + " +
                "TLS path are all verified via the desktop mock; only the " +
                "Android-side URB shovel is missing."
        )
    }

    companion object {
        private const val TAG = "ZerowireUsbIp"
    }
}
