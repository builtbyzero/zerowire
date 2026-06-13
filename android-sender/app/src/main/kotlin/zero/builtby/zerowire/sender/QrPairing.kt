package zero.builtby.zerowire.sender

/**
 * Pairing-QR payload, shared between the desktop receiver (generator)
 * and this Android sender (scanner).
 *
 * Format: a `zerowire://` URI so any QR scanner — system Camera, Google
 * Lens, the in-app zxing fragment — gives us a recognizable hand-off.
 *
 * ```
 *   zerowire://pair?host=<HOST>&port=<PORT>&code=<PAIRING_CODE>[&fp=<FINGERPRINT>]
 * ```
 *
 *   * `host` — receiver IP or DNS name. IPv4 / IPv6 / `host.local`.
 *   * `port` — receiver listening port (defaults to 47823 if absent).
 *   * `code` — the 6-digit (or longer) pairing code that's fed to
 *     `Psk.derivePsk`. The desktop generates this, the phone never
 *     types it.
 *   * `fp`  — optional hex SHA-256 fingerprint of the receiver's
 *     deterministic TLS cert. If present, the sender pins it on
 *     handshake and bails on mismatch. Defense in depth — the cert is
 *     already derived from the code, but pinning catches "user scanned
 *     the wrong sticker".
 */
data class PairingPayload(
    val host: String,
    val port: Int,
    val code: String,
    val fingerprint: String? = null,
) {
    fun displayEndpoint(): String = "$host:$port"

    fun encode(): String {
        val sb = StringBuilder("zerowire://pair?host=")
        sb.append(urlEncode(host))
        sb.append("&port=").append(port)
        sb.append("&code=").append(urlEncode(code))
        if (!fingerprint.isNullOrEmpty()) {
            sb.append("&fp=").append(urlEncode(fingerprint))
        }
        return sb.toString()
    }

    companion object {
        const val DEFAULT_PORT = 47823

        /**
         * Parse a scanned QR string into a [PairingPayload]. Returns
         * null on any malformed input — the UI surfaces "scan looked
         * bad, try again" rather than throwing.
         */
        fun parse(text: String?): PairingPayload? {
            if (text.isNullOrBlank()) return null
            val trimmed = text.trim()
            val lower = trimmed.lowercase()
            if (!lower.startsWith("zerowire://pair")) return null
            val qIdx = trimmed.indexOf('?')
            if (qIdx < 0 || qIdx == trimmed.length - 1) return null

            val params = mutableMapOf<String, String>()
            for (pair in trimmed.substring(qIdx + 1).split('&')) {
                val eq = pair.indexOf('=')
                if (eq <= 0) return null
                val k = pair.substring(0, eq)
                val v = pair.substring(eq + 1)
                params[k] = urlDecode(v) ?: return null
            }

            val host = params["host"] ?: return null
            if (host.isBlank()) return null
            val code = params["code"] ?: return null
            if (code.isBlank()) return null
            val port = params["port"]?.toIntOrNull() ?: DEFAULT_PORT
            if (port !in 1..65535) return null
            val fp = params["fp"]?.takeUnless { it.isBlank() }

            return PairingPayload(host = host, port = port, code = code, fingerprint = fp)
        }

        private fun urlEncode(s: String): String =
            java.net.URLEncoder.encode(s, Charsets.UTF_8.name())

        private fun urlDecode(s: String): String? =
            try { java.net.URLDecoder.decode(s, Charsets.UTF_8.name()) }
            catch (_: IllegalArgumentException) { null }
    }
}
