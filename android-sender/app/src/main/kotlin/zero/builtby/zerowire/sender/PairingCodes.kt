package zero.builtby.zerowire.sender

import java.security.SecureRandom

/**
 * Random 6-digit pairing codes.
 *
 * For the walking skeleton, the code is purely visual. The real pairing
 * protocol (see ARCHITECTURE.md §5) will bind it to an HMAC of a freshly
 * generated PSK keyed by a code-derived secret.
 */
object PairingCodes {
    private val rng = SecureRandom()

    fun sixDigit(): String {
        // Uniform 000000..999999 — SecureRandom.nextInt(bound) is uniform.
        val n = rng.nextInt(1_000_000)
        return "%06d".format(n)
    }
}
