package zero.builtby.zerowire.sender

import android.hardware.usb.UsbConstants
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbDeviceConnection
import android.hardware.usb.UsbEndpoint
import android.hardware.usb.UsbInterface
import android.hardware.usb.UsbManager
import android.util.Log
import java.io.IOException

/**
 * Claim a HID interface on an Android-side [UsbDevice] and read HID input
 * reports off the IN interrupt endpoint.
 *
 * This is the piece that turns "Android phone with a mouse plugged in"
 * into "stream of bytes the receiver wants". The receiver doesn't care
 * which mouse — it just wants the raw boot-mouse / boot-keyboard reports.
 *
 * Lifecycle:
 *   1. [open] — claim interface, look up endpoint, fetch report descriptor
 *      via a control transfer (USB HID class GET_DESCRIPTOR).
 *   2. [readReport] — block up to `timeoutMs` for one input report.
 *   3. [close] — release interface, close the connection.
 *
 * This class is **synchronous**. Wrap it in your own thread/coroutine.
 *
 * **NOTE — hardware verification status:** This code compiled cleanly and
 * was code-reviewed against the AOSP UsbHostManager source, but it has not
 * been run against a real device on this build host. The control-transfer
 * constants below are the textbook HID 1.11 values (§7.1.1) — they're
 * stable, but if a phone's USB host stack rejects a particular request we
 * may need to retry with adjusted wValue/wIndex. Manual test plan in
 * `docs/hid-demo.md`.
 */
class HidEndpoint private constructor(
    private val device: UsbDevice,
    private val connection: UsbDeviceConnection,
    private val iface: UsbInterface,
    private val endpointIn: UsbEndpoint,
    val reportDescriptor: ByteArray,
) {
    /** HID kind hint, sniffed from the report descriptor's first top-level usage. */
    enum class Kind { MOUSE, KEYBOARD, GAMEPAD, OTHER }

    val kind: Kind by lazy { classifyDescriptor(reportDescriptor) }

    /**
     * Read one HID input report. Returns null on timeout, throws on
     * connection error. Buffer size = endpoint's wMaxPacketSize.
     */
    @Throws(IOException::class)
    fun readReport(timeoutMs: Int): ByteArray? {
        val buf = ByteArray(endpointIn.maxPacketSize)
        val n = connection.bulkTransfer(endpointIn, buf, buf.size, timeoutMs)
        return when {
            n > 0 -> buf.copyOf(n)
            n == 0 -> null   // zero-length packet; treat as no-op
            else -> null     // timeout or transient; caller can retry
        }
    }

    fun close() {
        try { connection.releaseInterface(iface) } catch (_: Throwable) { /* ignore */ }
        try { connection.close() } catch (_: Throwable) { /* ignore */ }
    }

    companion object {
        private const val TAG = "zerowire/HidEndpoint"

        // USB HID 1.11 §7.1.1 — Get_Descriptor (Report) control transfer.
        private const val REQUEST_TYPE_GET_DESCRIPTOR = 0x81 // device-to-host, std-iface
        private const val REQUEST_GET_DESCRIPTOR = 0x06
        private const val DESCRIPTOR_TYPE_HID_REPORT = 0x22

        /**
         * Open the first HID interface on [device]. Returns null if the
         * device has none, or if we couldn't claim it.
         */
        fun open(usbManager: UsbManager, device: UsbDevice): HidEndpoint? {
            val hidIface = (0 until device.interfaceCount)
                .map(device::getInterface)
                .firstOrNull { it.interfaceClass == UsbConstants.USB_CLASS_HID }
                ?: run {
                    Log.w(TAG, "no HID interface on ${device.deviceName}")
                    return null
                }
            val epIn = (0 until hidIface.endpointCount)
                .map(hidIface::getEndpoint)
                .firstOrNull {
                    it.direction == UsbConstants.USB_DIR_IN &&
                        it.type == UsbConstants.USB_ENDPOINT_XFER_INT
                }
                ?: run {
                    Log.w(TAG, "HID iface has no IN interrupt endpoint")
                    return null
                }
            val conn = usbManager.openDevice(device) ?: run {
                Log.w(TAG, "openDevice returned null (missing permission?)")
                return null
            }
            if (!conn.claimInterface(hidIface, true)) {
                Log.w(TAG, "claimInterface failed")
                conn.close()
                return null
            }
            val rd = fetchReportDescriptor(conn, hidIface.id)
            return HidEndpoint(device, conn, hidIface, epIn, rd)
        }

        /** Pull the HID report descriptor off the device. Empty on failure. */
        private fun fetchReportDescriptor(conn: UsbDeviceConnection, ifaceId: Int): ByteArray {
            val buf = ByteArray(4096)
            val n = conn.controlTransfer(
                REQUEST_TYPE_GET_DESCRIPTOR,
                REQUEST_GET_DESCRIPTOR,
                DESCRIPTOR_TYPE_HID_REPORT shl 8,
                ifaceId,
                buf,
                buf.size,
                2_000
            )
            if (n <= 0) {
                Log.w(TAG, "control transfer for HID report descriptor returned $n")
                return ByteArray(0)
            }
            return buf.copyOf(n)
        }

        /**
         * Classify a HID report descriptor by the first
         * `Usage Page (Generic Desktop) + Usage (..)` pair. Mirrors the
         * Rust receiver's hid_descriptor::classify.
         */
        fun classifyDescriptor(desc: ByteArray): Kind {
            var page: Int? = null
            var i = 0
            while (i < desc.size) {
                val prefix = desc[i].toInt() and 0xFF
                val size = when (prefix and 0x03) {
                    0 -> 0; 1 -> 1; 2 -> 2; else -> 4
                }
                val tag = prefix and 0xFC
                if (i + 1 + size > desc.size) break
                val data = when (size) {
                    0 -> 0
                    1 -> desc[i + 1].toInt() and 0xFF
                    2 -> ((desc[i + 2].toInt() and 0xFF) shl 8) or (desc[i + 1].toInt() and 0xFF)
                    else -> ByteBuffer4Le(desc, i + 1)
                }
                if (tag == 0x04) { // Global: Usage Page
                    page = data
                } else if (tag == 0x08) { // Local: Usage
                    if (page == 0x01) {
                        return when (data) {
                            0x02 -> Kind.MOUSE
                            0x06 -> Kind.KEYBOARD
                            0x04, 0x05 -> Kind.GAMEPAD
                            else -> Kind.OTHER
                        }
                    }
                }
                i += 1 + size
            }
            return Kind.OTHER
        }

        private fun ByteBuffer4Le(b: ByteArray, off: Int): Int =
            (b[off].toInt() and 0xFF) or
                ((b[off + 1].toInt() and 0xFF) shl 8) or
                ((b[off + 2].toInt() and 0xFF) shl 16) or
                ((b[off + 3].toInt() and 0xFF) shl 24)
    }
}
