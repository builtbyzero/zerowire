package zero.builtby.zerowire.sender

import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec

/**
 * Kotlin port of `protocol/src/psk.rs`.
 *
 * Derives a 256-bit PSK from a human-typed pairing code via HKDF-SHA256.
 * Salt and info bytes MUST match the Rust receiver exactly:
 *
 *   salt = "zerowire/v1/psk"
 *   info = "tls-psk"         (root PSK)
 *   info = "zerowire/v1/tls-cert-key"  (deterministic Ed25519 seed)
 *
 * Pinned with the same vectors `protocol::psk::tests` uses.
 */
object Psk {
    private const val HMAC_ALGO = "HmacSHA256"
    private const val HASH_LEN = 32

    val PSK_SALT: ByteArray = "zerowire/v1/psk".toByteArray(Charsets.UTF_8)
    val PSK_INFO: ByteArray = "tls-psk".toByteArray(Charsets.UTF_8)
    val TLS_CERT_KEY_INFO: ByteArray =
        "zerowire/v1/tls-cert-key".toByteArray(Charsets.UTF_8)

    fun derivePsk(pairingCode: String): ByteArray =
        deriveWithInfo(pairingCode, PSK_INFO)

    fun deriveTlsCertSeed(pairingCode: String): ByteArray =
        deriveWithInfo(pairingCode, TLS_CERT_KEY_INFO)

    fun deriveWithInfo(pairingCode: String, info: ByteArray, length: Int = 32): ByteArray {
        val prk = hkdfExtract(PSK_SALT, pairingCode.toByteArray(Charsets.UTF_8))
        return hkdfExpand(prk, info, length)
    }

    private fun hkdfExtract(salt: ByteArray, ikm: ByteArray): ByteArray {
        val mac = Mac.getInstance(HMAC_ALGO).apply {
            init(SecretKeySpec(salt, HMAC_ALGO))
        }
        return mac.doFinal(ikm)
    }

    private fun hkdfExpand(prk: ByteArray, info: ByteArray, length: Int): ByteArray {
        require(length <= 255 * HASH_LEN) { "HKDF output too long" }
        val mac = Mac.getInstance(HMAC_ALGO).apply {
            init(SecretKeySpec(prk, HMAC_ALGO))
        }
        val n = (length + HASH_LEN - 1) / HASH_LEN
        val okm = ByteArray(length)
        var prev = ByteArray(0)
        var written = 0
        for (i in 1..n) {
            mac.reset()
            mac.update(prev)
            mac.update(info)
            mac.update(i.toByte())
            prev = mac.doFinal()
            val take = minOf(HASH_LEN, length - written)
            System.arraycopy(prev, 0, okm, written, take)
            written += take
        }
        return okm
    }
}
