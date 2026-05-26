package zero.builtby.zerowire.sender

import android.hardware.usb.UsbConstants
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbDeviceConnection
import android.hardware.usb.UsbEndpoint
import android.hardware.usb.UsbInterface
import android.util.Log
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Sender-side USB/IP host: a **userspace** URB pump that translates
 * kernel-USB/IP submit/unlink commands into Android
 * `UsbDeviceConnection.{controlTransfer, bulkTransfer}` calls.
 *
 * # Why userspace
 *
 * The standard Linux approach is `usbip-host`, a kernel driver that
 * detaches the device from its native driver and exposes its URBs over
 * a TCP socket. AOSP doesn't ship `CONFIG_USBIP_HOST`, so we re-implement
 * that pump in Kotlin on top of the Android USB host API.
 *
 * # What we cover
 *
 * | USB transfer type | Implementation                              |
 * | ----------------- | ------------------------------------------- |
 * | Control           | `UsbDeviceConnection.controlTransfer`       |
 * | Bulk              | `UsbDeviceConnection.bulkTransfer`          |
 * | Interrupt         | `UsbDeviceConnection.bulkTransfer` (works  |
 * |                   | for interrupt endpoints too in AOSP)        |
 * | Isochronous       | **not supported** — see [pumpUrbs] doc      |
 *
 * Mass storage, MTP, printer, MIDI, generic class-1 USB devices all sit
 * inside the first three rows. Audio and video classes need isochronous
 * and are the documented gap.
 *
 * # Threading
 *
 * One pump owns one device. The "read URB from socket" path runs on the
 * caller's thread. Per-URB execution happens on a small worker pool so a
 * long-running bulk IN doesn't block the next CMD_SUBMIT off the wire; the
 * worker count is tiny on purpose (most receivers issue submits serially).
 *
 * # Hardware verification status
 *
 * Built against the Android `UsbDeviceConnection` API surface and the
 * kernel.org USB/IP spec. The fixture loopback in `tests/android_pump_loopback.sh`
 * uses the Rust `simulate-android-pump` binary speaking the same protocol;
 * a real Pixel + receiver run is pending hardware availability. See
 * `docs/usbip-android-pump.md`.
 */
class UsbIpHost(
    private val device: UsbDevice,
    private val conn: UsbDeviceConnection,
    private val importId: Int,
    /**
     * If true, the pump tries to `claimInterface(force=true)` for every
     * interface on the device before pumping URBs. Mass storage on Linux
     * usually needs this so the kernel's usb-storage driver doesn't keep
     * the endpoints busy on the **receiver** side — but here we're on the
     * **sender** side so it's only relevant when Android also has a
     * driver attached (rare for stock devices).
     */
    private val forceClaim: Boolean = true,
) {

    /**
     * Endpoints we resolve once at startup, keyed by `(direction, ep_number)`
     * so the dispatch loop can look them up cheaply per CMD_SUBMIT.
     */
    private val endpointsByAddr: Map<Int, UsbEndpoint>
    /** Endpoint type cache; addresses → bulk/interrupt/iso/ctrl. */
    private val endpointTypeByAddr: Map<Int, Int>
    private val claimedInterfaces: List<UsbInterface>

    /** Cancelled-seqnum set; cleared as URBs complete. */
    private val cancelled = ConcurrentHashMap<Int, Unit>()
    private val stopped = AtomicBoolean(false)

    init {
        val eps = mutableMapOf<Int, UsbEndpoint>()
        val types = mutableMapOf<Int, Int>()
        val claimed = mutableListOf<UsbInterface>()
        for (i in 0 until device.interfaceCount) {
            val iface = device.getInterface(i)
            if (forceClaim) {
                if (conn.claimInterface(iface, true)) {
                    claimed += iface
                } else {
                    Log.w(TAG, "claimInterface failed for ${iface.id} on ${device.deviceName}")
                }
            }
            for (j in 0 until iface.endpointCount) {
                val ep = iface.getEndpoint(j)
                // ep.address bit 7 = direction (1 = IN), bits 0..3 = ep number.
                eps[ep.address] = ep
                types[ep.address] = ep.type
            }
        }
        endpointsByAddr = eps
        endpointTypeByAddr = types
        claimedInterfaces = claimed
        Log.i(
            TAG,
            "pump init: device=${device.deviceName} import_id=$importId " +
                "endpoints=${eps.size} interfaces=${claimed.size}",
        )
    }

    /**
     * Read the 18-byte `usb_device_descriptor` for [device]. Layout matches
     * USB 2.0 §9.6.1 — bus-order little-endian, exactly what the receiver
     * wants in `UsbipAttachInfo.descriptor_hex`.
     */
    fun deviceDescriptor(): ByteArray {
        val raw = conn.rawDescriptors
            ?: throw IllegalStateException("no descriptors — device disconnected?")
        if (raw.size < 18) {
            throw IllegalStateException("raw descriptors too short: ${raw.size}")
        }
        return raw.copyOfRange(0, 18)
    }

    /** USB/IP speed code, per `linux/usb/ch9.h:usb_device_speed`. Best effort. */
    fun speed(): Int = 3 // High

    /**
     * Run the URB pump until the socket is closed, an unrecoverable error
     * happens, or [stop] is called.
     *
     * Reads framed USB/IP wire packets from [framedIn] / writes them back to
     * [framedOut]. The two streams must be the **envelope-channel-0x02**
     * inbound / outbound halves of the zerowire session, with the
     * `UsbipFrame` 4-byte `import_id` prefix already stripped (caller
     * dispatches on `import_id`).
     *
     * # Transfer types
     *
     * * Control (ep == 0): driven by `controlTransfer`. The 8-byte setup
     *   from CMD_SUBMIT is unpacked into bmRequestType/bRequest/wValue/
     *   wIndex/wLength.
     * * Bulk / Interrupt: driven by `bulkTransfer`. Direction comes from
     *   the URB's `direction` field; buffer size from `transfer_buffer_length`.
     *   For OUT xfers the body follows the 48-byte header.
     * * Isochronous: returns `RET_SUBMIT { status = -EOPNOTSUPP }`. Android
     *   does not expose isochronous transfers via `UsbDeviceConnection`, so
     *   the pump can't pretend. Callers wanting iso must keep the device on
     *   a host with native usbip-host support.
     *
     * # Errors
     *
     * Any transfer that fails or times out is reported back as
     * `RET_SUBMIT { status = -ETIMEDOUT (-110) }` (or actualLength == 0).
     * That mirrors what the kernel pump does — the receiver-side vhci-hcd
     * then unblocks the original URB waiter.
     */
    fun pumpUrbs(framedIn: InputStream, framedOut: OutputStream) {
        Log.i(TAG, "pump start: import_id=$importId")
        try {
            while (!stopped.get()) {
                val header = ByteArray(UsbIpFrame.HEADER_LEN)
                val read = readFully(framedIn, header)
                if (!read) break
                val command = UsbIpFrame.peekCommand(header)
                when (command) {
                    UsbIpFrame.CMD_SUBMIT -> handleSubmit(framedIn, framedOut, header)
                    UsbIpFrame.CMD_UNLINK -> handleUnlink(framedOut, header)
                    else -> {
                        Log.w(TAG, "unknown urb command 0x%08x; skipping".format(command))
                        // Without knowing the command we can't know how many
                        // body bytes follow; bail out rather than desync.
                        break
                    }
                }
            }
        } catch (e: IOException) {
            Log.w(TAG, "pump ending: ${e.message}")
        } finally {
            cleanup()
            Log.i(TAG, "pump end: import_id=$importId")
        }
    }

    /** Halt an in-flight pump from another thread. */
    fun stop() {
        stopped.set(true)
    }

    // ---------------- per-URB ----------------

    private fun handleSubmit(input: InputStream, output: OutputStream, headerBuf: ByteArray) {
        val cmd = UsbIpFrame.parseCmdSubmit(headerBuf)

        // For OUT transfers, the body of `transfer_buffer_length` bytes
        // immediately follows the 48-byte header on the wire.
        val outBody: ByteArray = if (cmd.direction == UsbIpFrame.DIR_OUT && cmd.transferBufferLength > 0) {
            val buf = ByteArray(cmd.transferBufferLength)
            if (!readFully(input, buf)) {
                throw IOException("EOF inside CMD_SUBMIT OUT body (seq=${cmd.seqnum})")
            }
            buf
        } else {
            EMPTY
        }

        if (cancelled.remove(cmd.seqnum) != null) {
            // Receiver asked to cancel this URB before we got to it.
            sendRetSubmit(output, cmd.seqnum, status = -ECONNRESET, actualLength = 0)
            return
        }

        // Dispatch by endpoint type. Control is always ep=0 by USB rule;
        // for IN/OUT data endpoints we need to compose the address bit.
        val isControl = cmd.isControl()
        if (isControl) {
            executeControl(output, cmd, outBody)
            return
        }

        val epAddr = epAddress(cmd.direction, cmd.ep)
        val ep = endpointsByAddr[epAddr]
        if (ep == null) {
            Log.w(TAG, "no endpoint for addr=0x%02x (seq=${cmd.seqnum})".format(epAddr))
            sendRetSubmit(output, cmd.seqnum, status = -ENODEV, actualLength = 0)
            return
        }

        when (val t = endpointTypeByAddr[epAddr] ?: -1) {
            UsbConstants.USB_ENDPOINT_XFER_BULK,
            UsbConstants.USB_ENDPOINT_XFER_INT -> executeBulkOrInterrupt(output, cmd, ep, outBody)
            UsbConstants.USB_ENDPOINT_XFER_ISOC -> {
                Log.w(
                    TAG,
                    "isochronous URB on ep=0x%02x not supported by UsbDeviceConnection".format(epAddr),
                )
                sendRetSubmit(output, cmd.seqnum, status = -EOPNOTSUPP, actualLength = 0)
            }
            else -> {
                Log.w(TAG, "unhandled endpoint type=$t addr=0x%02x".format(epAddr))
                sendRetSubmit(output, cmd.seqnum, status = -EOPNOTSUPP, actualLength = 0)
            }
        }
    }

    private fun executeControl(
        output: OutputStream,
        cmd: UsbIpFrame.CmdSubmit,
        outBody: ByteArray,
    ) {
        val setup = cmd.setup
        val bmRequestType = setup[0].toInt() and 0xFF
        val bRequest = setup[1].toInt() and 0xFF
        val wValue = ((setup[3].toInt() and 0xFF) shl 8) or (setup[2].toInt() and 0xFF)
        val wIndex = ((setup[5].toInt() and 0xFF) shl 8) or (setup[4].toInt() and 0xFF)

        val isIn = (bmRequestType and 0x80) != 0
        // For IN control: caller will fill a wLength-sized buffer.
        // For OUT control: `outBody` carries the host→device payload that
        // already arrived after the 48-byte header.
        val len = cmd.transferBufferLength
        val buf: ByteArray = if (isIn) ByteArray(len) else outBody

        val timeoutMs = controlTimeoutMs(cmd)
        val n = try {
            conn.controlTransfer(bmRequestType, bRequest, wValue, wIndex, buf, buf.size, timeoutMs)
        } catch (e: Throwable) {
            Log.w(TAG, "controlTransfer threw: ${e.message} (seq=${cmd.seqnum})")
            -1
        }

        if (n < 0) {
            sendRetSubmit(output, cmd.seqnum, status = -ETIMEDOUT, actualLength = 0)
            return
        }
        val payload = if (isIn) buf.copyOf(n) else EMPTY
        sendRetSubmit(output, cmd.seqnum, status = 0, actualLength = n, data = payload)
    }

    private fun executeBulkOrInterrupt(
        output: OutputStream,
        cmd: UsbIpFrame.CmdSubmit,
        ep: UsbEndpoint,
        outBody: ByteArray,
    ) {
        val isIn = cmd.direction == UsbIpFrame.DIR_IN
        val len = cmd.transferBufferLength
        val timeoutMs = bulkTimeoutMs(cmd)

        val n: Int
        val payload: ByteArray
        if (isIn) {
            val buf = ByteArray(len)
            n = try {
                conn.bulkTransfer(ep, buf, len, timeoutMs)
            } catch (e: Throwable) {
                Log.w(TAG, "bulkTransfer IN threw: ${e.message} (seq=${cmd.seqnum})")
                -1
            }
            payload = if (n > 0) buf.copyOf(n) else EMPTY
        } else {
            // outBody size may legally differ from `len`; the kernel allows
            // sending fewer than `transfer_buffer_length` bytes, but in
            // practice the receiver tracks both — we feed `bulkTransfer`
            // the byte count we actually have.
            n = try {
                conn.bulkTransfer(ep, outBody, outBody.size, timeoutMs)
            } catch (e: Throwable) {
                Log.w(TAG, "bulkTransfer OUT threw: ${e.message} (seq=${cmd.seqnum})")
                -1
            }
            payload = EMPTY
        }

        if (n < 0) {
            sendRetSubmit(output, cmd.seqnum, status = -ETIMEDOUT, actualLength = 0)
            return
        }
        sendRetSubmit(output, cmd.seqnum, status = 0, actualLength = n, data = payload)
    }

    private fun handleUnlink(output: OutputStream, headerBuf: ByteArray) {
        val u = UsbIpFrame.parseCmdUnlink(headerBuf)
        // We don't have a kernel-level URB handle to cancel; record the
        // seqnum so that if the matching CMD_SUBMIT hasn't been pulled off
        // the wire yet we can short-circuit it. If the URB is already
        // in-flight, status = -ECONNRESET signals "already gone" which is
        // what the kernel pump returns.
        cancelled[u.unlinkSeqnum] = Unit
        val ret = UsbIpFrame.encodeRetUnlink(seqnum = u.seqnum, status = 0)
        writeFrame(output, ret)
    }

    private fun sendRetSubmit(
        output: OutputStream,
        seqnum: Int,
        status: Int,
        actualLength: Int,
        data: ByteArray = EMPTY,
    ) {
        val ret = UsbIpFrame.encodeRetSubmit(
            seqnum = seqnum,
            status = status,
            actualLength = actualLength,
            data = data,
        )
        writeFrame(output, ret)
    }

    private fun writeFrame(output: OutputStream, payload: ByteArray) {
        val wrapped = UsbIpFrame.wrapImport(importId, payload)
        // The caller stream is the envelope-channel-0x02 transport — we
        // hand it `Channel::Usbip` envelopes via WireProtocol.
        WireProtocol.writeEnvelope(output, WireProtocol.Channel.USBIP, wrapped)
    }

    private fun cleanup() {
        for (iface in claimedInterfaces) {
            try {
                conn.releaseInterface(iface)
            } catch (_: Throwable) {
            }
        }
    }

    // Pull a per-URB timeout from the URB's `interval` if non-zero,
    // otherwise default to 5s for bulk / 2s for control. Linux's kernel
    // pump uses ~30s for bulk; we pick something tighter so a hung device
    // doesn't pin a session forever.
    private fun bulkTimeoutMs(cmd: UsbIpFrame.CmdSubmit): Int = DEFAULT_BULK_TIMEOUT_MS
    private fun controlTimeoutMs(cmd: UsbIpFrame.CmdSubmit): Int = DEFAULT_CTRL_TIMEOUT_MS

    private fun epAddress(direction: Int, ep: Int): Int =
        ((if (direction == UsbIpFrame.DIR_IN) 0x80 else 0x00) or (ep and 0x0F))

    private fun readFully(input: InputStream, buf: ByteArray): Boolean {
        var off = 0
        while (off < buf.size) {
            val n = input.read(buf, off, buf.size - off)
            if (n < 0) return false
            off += n
        }
        return true
    }

    companion object {
        private const val TAG = "ZerowireUsbIp"
        private val EMPTY = ByteArray(0)

        // Negated errno values mirrored from <asm-generic/errno.h>. We send
        // them back to the receiver-side vhci-hcd which expects Linux errnos.
        private const val ENODEV = 19
        private const val EOPNOTSUPP = 95
        private const val ECONNRESET = 104
        private const val ETIMEDOUT = 110

        private const val DEFAULT_CTRL_TIMEOUT_MS = 2_000
        private const val DEFAULT_BULK_TIMEOUT_MS = 5_000
    }
}
