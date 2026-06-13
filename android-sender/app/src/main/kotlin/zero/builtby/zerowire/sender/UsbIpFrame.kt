package zero.builtby.zerowire.sender

import java.io.IOException
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Kernel-USB/IP submit/return header codec — mirror of
 * `protocol/src/usbip.rs` (`CmdSubmit` / `RetSubmit` / `CmdUnlink` /
 * `RetUnlink`).
 *
 * All fields on the wire are big-endian. The header is exactly 48 bytes;
 * `CMD_SUBMIT` OUT and `RET_SUBMIT` IN are followed by their transfer
 * buffers, everything else is header-only.
 *
 * We hand-roll the codec — no third-party deps — so the Kotlin side stays
 * the lightweight half of the system. The Rust crate's tests are the
 * source of truth; this file is a 1:1 translation.
 */
object UsbIpFrame {

    const val HEADER_LEN = 48

    // Per-URB op codes (the `command` field, u32 big-endian).
    const val CMD_SUBMIT = 0x00000001
    const val CMD_UNLINK = 0x00000002
    const val RET_SUBMIT = 0x00000003
    const val RET_UNLINK = 0x00000004

    // Direction bit in CMD_SUBMIT.
    const val DIR_OUT = 0
    const val DIR_IN = 1

    /** Decoded `USBIP_CMD_SUBMIT` header. */
    data class CmdSubmit(
        val seqnum: Int,
        val devid: Int,
        val direction: Int,
        val ep: Int,
        val transferFlags: Int,
        val transferBufferLength: Int,
        val startFrame: Int,
        val numberOfPackets: Int,
        val interval: Int,
        val setup: ByteArray,
    ) {
        init {
            require(setup.size == 8) { "setup must be 8 bytes" }
        }

        /** True when this URB is targeting endpoint 0 (control transfer). */
        fun isControl(): Boolean = ep == 0
    }

    /** Decoded `USBIP_CMD_UNLINK` header. */
    data class CmdUnlink(
        val seqnum: Int,
        val devid: Int,
        val direction: Int,
        val ep: Int,
        val unlinkSeqnum: Int,
    )

    /** Peek the 4-byte command field; useful for dispatch before full parse. */
    fun peekCommand(buf: ByteArray): Int {
        if (buf.size < 4) throw IOException("urb header too short for op: ${buf.size}")
        return ByteBuffer.wrap(buf, 0, 4).order(ByteOrder.BIG_ENDIAN).int
    }

    fun parseCmdSubmit(buf: ByteArray): CmdSubmit {
        if (buf.size < HEADER_LEN) {
            throw IOException("CMD_SUBMIT too short: ${buf.size}")
        }
        val b = ByteBuffer.wrap(buf, 0, HEADER_LEN).order(ByteOrder.BIG_ENDIAN)
        val cmd = b.int
        if (cmd != CMD_SUBMIT) {
            throw IOException("expected CMD_SUBMIT (0x1), got 0x%08x".format(cmd))
        }
        val seqnum = b.int
        val devid = b.int
        val direction = b.int
        val ep = b.int
        val transferFlags = b.int
        val transferBufferLength = b.int
        val startFrame = b.int
        val numberOfPackets = b.int
        val interval = b.int
        val setup = ByteArray(8)
        System.arraycopy(buf, 40, setup, 0, 8)
        return CmdSubmit(
            seqnum = seqnum,
            devid = devid,
            direction = direction,
            ep = ep,
            transferFlags = transferFlags,
            transferBufferLength = transferBufferLength,
            startFrame = startFrame,
            numberOfPackets = numberOfPackets,
            interval = interval,
            setup = setup,
        )
    }

    fun parseCmdUnlink(buf: ByteArray): CmdUnlink {
        if (buf.size < HEADER_LEN) {
            throw IOException("CMD_UNLINK too short: ${buf.size}")
        }
        val b = ByteBuffer.wrap(buf, 0, HEADER_LEN).order(ByteOrder.BIG_ENDIAN)
        val cmd = b.int
        if (cmd != CMD_UNLINK) {
            throw IOException("expected CMD_UNLINK (0x2), got 0x%08x".format(cmd))
        }
        val seqnum = b.int
        val devid = b.int
        val direction = b.int
        val ep = b.int
        val unlinkSeqnum = b.int
        return CmdUnlink(
            seqnum = seqnum,
            devid = devid,
            direction = direction,
            ep = ep,
            unlinkSeqnum = unlinkSeqnum,
        )
    }

    /**
     * Encode a `USBIP_RET_SUBMIT` header. The `data` argument is the
     * transfer-buffer payload (for IN xfers); the result is `48 + data.size`
     * bytes. For OUT xfers, `data` is empty.
     */
    fun encodeRetSubmit(
        seqnum: Int,
        status: Int,
        actualLength: Int,
        startFrame: Int = 0,
        numberOfPackets: Int = 0,
        errorCount: Int = 0,
        direction: Int = 0,
        ep: Int = 0,
        data: ByteArray = EMPTY,
    ): ByteArray {
        val out = ByteArray(HEADER_LEN + data.size)
        val b = ByteBuffer.wrap(out, 0, HEADER_LEN).order(ByteOrder.BIG_ENDIAN)
        b.putInt(RET_SUBMIT)
        b.putInt(seqnum)
        b.putInt(0)                       // devid — kernel ignores on return
        b.putInt(direction)
        b.putInt(ep)
        b.putInt(status)
        b.putInt(actualLength)
        b.putInt(startFrame)
        b.putInt(numberOfPackets)
        b.putInt(errorCount)
        // bytes 40..48 stay zero (RET_SUBMIT padding).
        if (data.isNotEmpty()) System.arraycopy(data, 0, out, HEADER_LEN, data.size)
        return out
    }

    fun encodeRetUnlink(
        seqnum: Int,
        status: Int,
        direction: Int = 0,
        ep: Int = 0,
        devid: Int = 0,
    ): ByteArray {
        val out = ByteArray(HEADER_LEN)
        val b = ByteBuffer.wrap(out, 0, HEADER_LEN).order(ByteOrder.BIG_ENDIAN)
        b.putInt(RET_UNLINK)
        b.putInt(seqnum)
        b.putInt(devid)
        b.putInt(direction)
        b.putInt(ep)
        b.putInt(status)
        // remaining 24 bytes stay zero.
        return out
    }

    /**
     * Wrap a USB/IP payload (the kernel-format header + optional body) in
     * the zerowire UsbipFrame on-wire prefix (4-byte big-endian
     * `import_id` then `raw`). Mirrors the Rust `UsbipFrame::encode`.
     */
    fun wrapImport(importId: Int, payload: ByteArray): ByteArray {
        val out = ByteArray(4 + payload.size)
        out[0] = ((importId ushr 24) and 0xFF).toByte()
        out[1] = ((importId ushr 16) and 0xFF).toByte()
        out[2] = ((importId ushr 8) and 0xFF).toByte()
        out[3] = (importId and 0xFF).toByte()
        System.arraycopy(payload, 0, out, 4, payload.size)
        return out
    }

    /** Reverse of [wrapImport]: returns (importId, raw-after-prefix). */
    fun unwrapImport(payload: ByteArray): Pair<Int, ByteArray> {
        if (payload.size < 4) throw IOException("UsbipFrame too short: ${payload.size}")
        val id = ((payload[0].toInt() and 0xFF) shl 24) or
            ((payload[1].toInt() and 0xFF) shl 16) or
            ((payload[2].toInt() and 0xFF) shl 8) or
            (payload[3].toInt() and 0xFF)
        return id to payload.copyOfRange(4, payload.size)
    }

    private val EMPTY = ByteArray(0)
}
