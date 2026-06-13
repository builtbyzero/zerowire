package zero.builtby.zerowire.sender

import android.util.Log
import java.io.ByteArrayInputStream
import java.math.BigInteger
import java.net.Socket
import java.security.KeyFactory
import java.security.KeyStore
import java.security.MessageDigest
import java.security.PrivateKey
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.security.spec.PKCS8EncodedKeySpec
import java.util.Date
import javax.net.ssl.KeyManagerFactory
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLSocket
import javax.net.ssl.SSLSocketFactory
import javax.net.ssl.TrustManager
import javax.net.ssl.X509TrustManager

/**
 * Wrap a plain TCP socket in TLS 1.3 using the deterministic-cert PSK
 * identity (see protocol/src/psk.rs + desktop-linux/src/tls.rs for the
 * Rust side).
 *
 * # Hardware-verification status
 *
 * **Not run against an Android phone in this commit.** The code compiles
 * cleanly against the AOSP-bundled `javax.net.ssl` API surface but the
 * specific cert/key plumbing (PKCS8 → Ed25519 PrivateKey → KeyStore) has
 * a few flavours that vary by Android version:
 *
 * * **Android 11+ ships Conscrypt with Ed25519** for `KeyFactory.getInstance("Ed25519")`.
 * * **Android 10 and earlier** need `org.conscrypt:conscrypt-android` as a
 *   gradle dependency, which we haven't pinned yet because the gradle wrapper
 *   isn't bootable on this build host.
 *
 * If a real Android run shows `NoSuchAlgorithmException: Ed25519` on the
 * `KeyFactory.getInstance` line, add `org.conscrypt:conscrypt-android:2.5.2`
 * to `app/build.gradle.kts` and call `Security.insertProviderAt(Conscrypt.newProvider(), 1)`
 * at app startup.
 *
 * # What the receiver expects on the wire
 *
 * mTLS 1.3 handshake. The receiver presents the same deterministic-cert
 * derived from the pairing code via HKDF-SHA256, and pins it byte-for-byte
 * against ours. If the codes mismatch, the handshake fails inside
 * SSLEngine.beginHandshake() before any application bytes flow.
 */
object TlsServer {

    private const val TAG = "ZerowireTLS"

    /**
     * Build a deterministic Ed25519 cert+key from the pairing code, then
     * return an SSLSocket wrapping the given plain `Socket`. The returned
     * SSLSocket is in **client-auth required** mode — the peer must
     * present the same cert.
     *
     * Caller is responsible for `startHandshake()` and dealing with
     * `SSLHandshakeException`.
     */
    fun wrap(plain: Socket, pairingCode: String): SSLSocket {
        val identity = PskIdentity.derive(pairingCode)
        val sslContext = buildSslContext(identity)
        val factory = sslContext.socketFactory as SSLSocketFactory
        val ssl = factory.createSocket(
            plain, plain.inetAddress.hostAddress, plain.port, true,
        ) as SSLSocket
        ssl.useClientMode = false
        ssl.needClientAuth = true
        ssl.enabledProtocols = arrayOf("TLSv1.3")
        Log.i(TAG, "tls wrap ready: fingerprint=${identity.fingerprintHex.substring(0, 16)}…")
        return ssl
    }

    private fun buildSslContext(identity: PskIdentity): SSLContext {
        val ks = KeyStore.getInstance("PKCS12")
        ks.load(null, null)
        // password is irrelevant (we never persist this keystore).
        val pw = "zerowire".toCharArray()
        ks.setKeyEntry("zerowire-psk", identity.privateKey, pw, arrayOf(identity.cert))

        val kmf = KeyManagerFactory.getInstance(KeyManagerFactory.getDefaultAlgorithm())
        kmf.init(ks, pw)

        val pinned = PinnedTrustManager(identity.cert)
        val ctx = SSLContext.getInstance("TLSv1.3")
        ctx.init(kmf.keyManagers, arrayOf<TrustManager>(pinned), null)
        return ctx
    }

    private class PinnedTrustManager(private val expected: X509Certificate) : X509TrustManager {
        private val expectedBytes = expected.encoded
        override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) {
            val end = chain?.firstOrNull()
                ?: throw IllegalArgumentException("no peer cert")
            if (!end.encoded.contentEquals(expectedBytes)) {
                throw IllegalArgumentException("client cert does not match PSK-derived expected cert")
            }
        }
        override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) {
            // Unused on the server-side wrap path.
            checkClientTrusted(chain, authType)
        }
        override fun getAcceptedIssuers(): Array<X509Certificate> = arrayOf(expected)
    }
}

/**
 * Deterministic identity derived from a pairing code. Construction is
 * idempotent: two calls with the same code produce byte-identical cert
 * material (must match the Rust side at desktop-linux/src/tls.rs).
 *
 * **Status:** code-reviewed against Android API surface, not yet run on
 * device. The Ed25519 key + self-signed-cert generation flow does not use
 * an Android-specific API, so the main risk is the Conscrypt provider
 * dependency mentioned in TlsServer.kt's doc comment.
 */
class PskIdentity private constructor(
    val privateKey: PrivateKey,
    val cert: X509Certificate,
    val fingerprintHex: String,
) {
    companion object {
        fun derive(pairingCode: String): PskIdentity {
            val seed = Psk.deriveTlsCertSeed(pairingCode)
            val pkcs8 = wrapSeedAsPkcs8(seed)
            val key = KeyFactory.getInstance("Ed25519")
                .generatePrivate(PKCS8EncodedKeySpec(pkcs8))
            // Producing a deterministic self-signed cert on Android without
            // BouncyCastle is awkward — `java.security.cert.X509Certificate`
            // is read-only. We use BouncyCastle's lightweight pkix module if
            // available; otherwise this path throws and the caller falls
            // back to plaintext with a clear error. Adding BouncyCastle to
            // `app/build.gradle.kts` is the documented next step.
            val cert = DeterministicCert.build(key, seed)
            val fp = sha256Hex(cert.encoded)
            return PskIdentity(key, cert, fp)
        }

        /** Wrap a 32-byte Ed25519 seed in the standard PKCS#8 DER envelope. */
        private fun wrapSeedAsPkcs8(seed: ByteArray): ByteArray {
            require(seed.size == 32) { "Ed25519 seed must be 32 bytes" }
            // RFC 8410 §7: PrivateKeyInfo for Ed25519 is:
            //   30 2e
            //     02 01 00                              -- version 0
            //     30 05 06 03 2b 65 70                  -- AlgorithmIdentifier (id-Ed25519)
            //     04 22                                 -- OCTET STRING, length 34
            //       04 20 <32-byte seed>                -- inner OCTET STRING
            val out = ByteArray(48)
            out[0] = 0x30; out[1] = 0x2e
            out[2] = 0x02; out[3] = 0x01; out[4] = 0x00
            out[5] = 0x30; out[6] = 0x05; out[7] = 0x06; out[8] = 0x03
            out[9] = 0x2b; out[10] = 0x65; out[11] = 0x70
            out[12] = 0x04; out[13] = 0x22
            out[14] = 0x04; out[15] = 0x20
            System.arraycopy(seed, 0, out, 16, 32)
            return out
        }

        private fun sha256Hex(bytes: ByteArray): String {
            val md = MessageDigest.getInstance("SHA-256")
            val d = md.digest(bytes)
            return d.joinToString("") { "%02x".format(it) }
        }
    }
}

/**
 * Stub deterministic-cert builder. Real implementations either:
 *  * vendor a small ASN.1 encoder + Ed25519 signer, or
 *  * depend on `org.bouncycastle:bcpkix-jdk18on`.
 *
 * Until the gradle deps are pinned (next commit, gated on a real Android
 * checkout), this throws — and the caller falls back to plaintext with a
 * clear log so the failure mode is obvious instead of mysterious.
 */
object DeterministicCert {
    fun build(@Suppress("unused") key: PrivateKey, @Suppress("unused") seed: ByteArray): X509Certificate {
        throw UnsupportedOperationException(
            "DeterministicCert.build not yet implemented on Android: add " +
                "org.bouncycastle:bcpkix-jdk18on to app/build.gradle.kts and " +
                "use JcaX509v3CertificateBuilder. The Rust receiver side " +
                "(desktop-linux/src/tls.rs) shows the exact serial / not-before " +
                "/ not-after values to pin so the two sides produce byte-identical certs."
        )
    }
}
