package zero.builtby.zerowire.sender

import android.content.Context
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.util.Log
import org.json.JSONArray
import org.json.JSONObject
import java.io.IOException
import java.net.Socket

/**
 * One TCP connection from a receiver. Runs the full session state machine:
 *
 *   HELLO       → HELLO_ACK
 *   LIST_DEVICES→ DEVICE_LIST
 *   ATTACH      → ATTACH_OK | ATTACH_DENIED
 *   HID Bind    → HID BindAck
 *   loop {
 *     read HID report from claimed device
 *     send HidOp::ReportIn envelope
 *   }
 *
 * One session ⇒ one bound HID device ⇒ one background reader thread. We
 * keep this synchronous-with-its-own-thread instead of dragging in
 * coroutines for IO; the wire side is already blocking and 1 thread per
 * receiver is plenty.
 *
 * **Hardware verification status:** code-reviewed, not yet run against a
 * real receiver from a real device. See `docs/hid-demo.md`.
 */
class ReceiverSession(
    private val socket: Socket,
    private val ctx: Context,
    private val senderId: String,
    private val deviceName: String,
) : Runnable {

    @Volatile private var running = true
    private val usbManager get() = ctx.getSystemService(Context.USB_SERVICE) as UsbManager

    override fun run() {
        socket.tcpNoDelay = true
        socket.soTimeout = 30_000
        Log.i(TAG, "session start: peer=${socket.inetAddress.hostAddress}")
        try {
            val input = socket.getInputStream()
            val output = socket.getOutputStream()
            // 1. HELLO → HELLO_ACK
            expectControl(input, "HELLO")
            sendJson(output, JSONObject().apply {
                put("op", "HELLO_ACK")
                put("sender_id", senderId)
                put("name", deviceName)
                put("supports", JSONArray(listOf("hid-fastlane/1")))
            })
            // 2. LIST_DEVICES → DEVICE_LIST
            val listMsg = expectControl(input, "LIST_DEVICES")
            val devices = usbManager.deviceList.values
            sendJson(output, UsbInventory.deviceListMessage(devices))
            // 3. ATTACH(hid) → ATTACH_OK
            val attach = expectControl(input, "ATTACH")
            val busid = attach.optString("busid")
            val mode = attach.optString("mode")
            val targetDevice = devices.firstOrNull { UsbInventory.busidFor(it) == busid }
            if (targetDevice == null || mode != "hid") {
                sendJson(output, JSONObject().apply {
                    put("op", "ATTACH_DENIED")
                    put("busid", busid)
                    put("reason", if (targetDevice == null) "unknown busid" else "mode $mode not supported")
                })
                return
            }
            sendJson(output, JSONObject().apply {
                put("op", "ATTACH_OK")
                put("busid", busid)
                put("import_id", 1)
            })
            // 4. HID Bind → BindAck + stream reports
            runHidBinding(input, output, targetDevice)
        } catch (e: IOException) {
            Log.w(TAG, "session ended: ${e.message}")
        } catch (e: Throwable) {
            Log.e(TAG, "session crashed", e)
        } finally {
            try { socket.close() } catch (_: Throwable) {}
            Log.i(TAG, "session end")
        }
    }

    fun stop() {
        running = false
        try { socket.close() } catch (_: Throwable) {}
    }

    // ---------------- internals ----------------

    private fun runHidBinding(
        input: java.io.InputStream,
        output: java.io.OutputStream,
        device: UsbDevice,
    ) {
        val env = WireProtocol.readEnvelope(input)
        if (env.channel != WireProtocol.Channel.HID) {
            throw IOException("expected HID envelope, got ${env.channel}")
        }
        val frame = WireProtocol.parseHidFrame(env.payload)
        if (frame.op != WireProtocol.HidOp.BIND) {
            throw IOException("expected Bind, got ${frame.op}")
        }
        val ep = HidEndpoint.open(usbManager, device)
            ?: throw IOException("failed to claim HID interface on ${device.deviceName}")
        try {
            val kind = when (ep.kind) {
                HidEndpoint.Kind.MOUSE -> "mouse"
                HidEndpoint.Kind.KEYBOARD -> "keyboard"
                HidEndpoint.Kind.GAMEPAD -> "gamepad"
                HidEndpoint.Kind.OTHER -> "other"
            }
            val bindId = if (frame.bindId == 0) 1 else frame.bindId
            val ackBody = BindAckBody.build(
                busid = UsbInventory.busidFor(device),
                kind = kind,
                vendorId = device.vendorId,
                productId = device.productId,
                name = device.productName ?: device.deviceName,
                reportDescriptor = ep.reportDescriptor,
            )
            val ackFrame = WireProtocol.encodeHidFrame(
                WireProtocol.HidOp.BIND_ACK,
                bindId,
                0,
                ackBody,
            )
            WireProtocol.writeEnvelope(output, WireProtocol.Channel.HID, ackFrame)
            Log.i(TAG, "BIND_ACK sent for ${device.productName} ($kind, rd=${ep.reportDescriptor.size}B)")

            var seq = 0
            while (running) {
                val report = try {
                    ep.readReport(timeoutMs = 1_000)
                } catch (e: IOException) {
                    Log.w(TAG, "read error: ${e.message}")
                    break
                } ?: continue
                seq = (seq + 1) and 0xFFFF
                val frameOut = WireProtocol.encodeHidFrame(
                    WireProtocol.HidOp.REPORT_IN,
                    bindId,
                    seq,
                    report,
                )
                try {
                    WireProtocol.writeEnvelope(output, WireProtocol.Channel.HID, frameOut)
                } catch (e: IOException) {
                    Log.w(TAG, "peer gone: ${e.message}")
                    break
                }
            }
            // Polite unbind on the way out.
            try {
                val unbind = WireProtocol.encodeHidFrame(
                    WireProtocol.HidOp.UNBIND,
                    bindId,
                    0,
                    """{"reason":"session ended"}""".toByteArray(Charsets.UTF_8),
                )
                WireProtocol.writeEnvelope(output, WireProtocol.Channel.HID, unbind)
            } catch (_: Throwable) { /* ignore */ }
        } finally {
            ep.close()
        }
    }

    private fun sendJson(out: java.io.OutputStream, msg: JSONObject) {
        WireProtocol.writeEnvelope(
            out,
            WireProtocol.Channel.CONTROL,
            msg.toString().toByteArray(Charsets.UTF_8),
        )
    }

    private fun expectControl(input: java.io.InputStream, op: String): JSONObject {
        val env = WireProtocol.readEnvelope(input)
        if (env.channel != WireProtocol.Channel.CONTROL) {
            throw IOException("expected CONTROL, got ${env.channel}")
        }
        val text = env.payload.toString(Charsets.UTF_8)
        val obj = JSONObject(text)
        val gotOp = obj.optString("op")
        if (gotOp != op) {
            throw IOException("expected op=$op, got op=$gotOp")
        }
        return obj
    }

    companion object {
        private const val TAG = "zerowire/Session"
    }
}
