package zero.builtby.zerowire.sender

import java.io.DataInputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Kotlin port of the zerowire envelope + HID-frame layout. Mirrors
 * `protocol/src/envelope.rs` and `protocol/src/hid.rs` so the two sides
 * see the same bytes on the wire.
 *
 * We deliberately keep this tiny — no codegen, no kotlinx-serialization,
 * just bytes. The control channel still carries JSON because that's what
 * the spec says; we hand-roll those payloads in [ControlJson].
 */
object WireProtocol {

    val MAGIC: ByteArray = byteArrayOf('Z'.code.toByte(), 'W'.code.toByte())
    const val PROTOCOL_VERSION: Byte = 1
    const val HEADER_LEN = 8

    /** Logical channels multiplexed over the envelope. Mirrors Rust `Channel`. */
    enum class Channel(val byte: Byte) {
        CONTROL(0x01),
        USBIP(0x02),
        HID(0x03),
        KEEPALIVE(0x7F);

        companion object {
            fun fromByte(b: Byte): Channel? = entries.firstOrNull { it.byte == b }
        }
    }

    /** HID op codes — mirrors Rust `HidOp`. */
    enum class HidOp(val byte: Byte) {
        BIND(0x01),
        BIND_ACK(0x02),
        REPORT_IN(0x10),
        REPORT_OUT(0x11),
        REPORT_FEATURE(0x12),
        UNBIND(0x20);

        companion object {
            fun fromByte(b: Byte): HidOp? = entries.firstOrNull { it.byte == b }
        }
    }

    /** Encode and write a full envelope to [out]. Thread-unsafe; caller must serialize. */
    fun writeEnvelope(out: OutputStream, channel: Channel, payload: ByteArray) {
        val hdr = ByteArray(HEADER_LEN)
        hdr[0] = MAGIC[0]
        hdr[1] = MAGIC[1]
        hdr[2] = PROTOCOL_VERSION
        hdr[3] = channel.byte
        ByteBuffer.wrap(hdr, 4, 4)
            .order(ByteOrder.BIG_ENDIAN)
            .putInt(payload.size)
        synchronized(out) {
            out.write(hdr)
            out.write(payload)
            out.flush()
        }
    }

    /** One owned envelope read from [input]. */
    data class IncomingEnvelope(val channel: Channel, val payload: ByteArray)

    /** Block until a full envelope is read. Throws on EOF or protocol violation. */
    @Throws(IOException::class)
    fun readEnvelope(input: InputStream): IncomingEnvelope {
        val data = DataInputStream(input)
        val hdr = ByteArray(HEADER_LEN)
        data.readFully(hdr)
        if (hdr[0] != MAGIC[0] || hdr[1] != MAGIC[1]) {
            throw IOException("bad magic: ${hdr[0].toInt()},${hdr[1].toInt()}")
        }
        if (hdr[2] != PROTOCOL_VERSION) {
            throw IOException("unsupported protocol version: ${hdr[2].toInt()}")
        }
        val channel = Channel.fromByte(hdr[3])
            ?: throw IOException("unknown channel: 0x%02x".format(hdr[3].toInt() and 0xFF))
        val len = ByteBuffer.wrap(hdr, 4, 4).order(ByteOrder.BIG_ENDIAN).int
        if (len < 0 || len > 1 shl 20) {
            throw IOException("payload length out of range: $len")
        }
        val payload = ByteArray(len)
        if (len > 0) data.readFully(payload)
        return IncomingEnvelope(channel, payload)
    }

    /** Encode a HID frame body (4-byte header + body). */
    fun encodeHidFrame(op: HidOp, bindId: Int, seq: Int, body: ByteArray): ByteArray {
        val out = ByteArray(4 + body.size)
        out[0] = op.byte
        out[1] = (bindId and 0xFF).toByte()
        out[2] = ((seq shr 8) and 0xFF).toByte()
        out[3] = (seq and 0xFF).toByte()
        System.arraycopy(body, 0, out, 4, body.size)
        return out
    }

    /** Decode the header of a HID frame. Returns (op, bindId, seq, body offset). */
    data class HidFrameView(val op: HidOp, val bindId: Int, val seq: Int, val body: ByteArray)

    fun parseHidFrame(payload: ByteArray): HidFrameView {
        if (payload.size < 4) throw IOException("hid frame too short: ${payload.size}")
        val op = HidOp.fromByte(payload[0])
            ?: throw IOException("unknown hid op: 0x%02x".format(payload[0].toInt() and 0xFF))
        val bindId = payload[1].toInt() and 0xFF
        val seq = ((payload[2].toInt() and 0xFF) shl 8) or (payload[3].toInt() and 0xFF)
        val body = payload.copyOfRange(4, payload.size)
        return HidFrameView(op, bindId, seq, body)
    }
}

/**
 * Build the BindAck body that the Rust receiver expects:
 *
 *     [ u16 BE meta_len ] [ meta_len bytes BindAckMeta JSON ] [ HID report descriptor ]
 */
object BindAckBody {

    fun build(
        busid: String,
        kind: String,              // "mouse" | "keyboard" | "gamepad" | "other"
        vendorId: Int,
        productId: Int,
        name: String,
        reportDescriptor: ByteArray
    ): ByteArray {
        val meta = org.json.JSONObject().apply {
            put("busid", busid)
            put("kind", kind)
            put("vendor_id", vendorId)
            put("product_id", productId)
            put("name", name)
        }.toString().toByteArray(Charsets.UTF_8)
        val out = ByteArray(2 + meta.size + reportDescriptor.size)
        out[0] = ((meta.size shr 8) and 0xFF).toByte()
        out[1] = (meta.size and 0xFF).toByte()
        System.arraycopy(meta, 0, out, 2, meta.size)
        System.arraycopy(reportDescriptor, 0, out, 2 + meta.size, reportDescriptor.size)
        return out
    }
}
