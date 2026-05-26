package zero.builtby.zerowire.sender

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * Round-trip tests for [PairingPayload]. The wire format is shared with
 * the Rust desktop receiver (`desktop-linux-gui/src/pairing.rs`); these
 * vectors are pinned so any change to the URI shape breaks loudly.
 */
class QrPairingTest {

    @Test fun parsesMinimalPayload() {
        val p = PairingPayload.parse("zerowire://pair?host=192.168.1.42&port=47823&code=ABC123")!!
        assertEquals("192.168.1.42", p.host)
        assertEquals(47823, p.port)
        assertEquals("ABC123", p.code)
        assertNull(p.fingerprint)
    }

    @Test fun parsesWithFingerprint() {
        val p = PairingPayload.parse(
            "zerowire://pair?host=phone.local&port=47823&code=abc&fp=deadbeef"
        )!!
        assertEquals("phone.local", p.host)
        assertEquals("deadbeef", p.fingerprint)
    }

    @Test fun defaultsPortToCanonical() {
        val p = PairingPayload.parse("zerowire://pair?host=10.0.0.5&code=xyz")!!
        assertEquals(PairingPayload.DEFAULT_PORT, p.port)
    }

    @Test fun urlDecodesHost() {
        // IPv6 addresses end up url-encoded in the QR.
        val payload = "zerowire://pair?host=%5B%3A%3A1%5D&port=4242&code=k"
        val p = PairingPayload.parse(payload)!!
        assertEquals("[::1]", p.host)
        assertEquals(4242, p.port)
    }

    @Test fun rejectsWrongScheme() {
        assertNull(PairingPayload.parse("http://pair?host=x&code=y"))
        assertNull(PairingPayload.parse("zerowire://share?host=x&code=y"))
    }

    @Test fun rejectsMissingFields() {
        assertNull(PairingPayload.parse("zerowire://pair?host=x"))    // no code
        assertNull(PairingPayload.parse("zerowire://pair?code=x"))    // no host
        assertNull(PairingPayload.parse("zerowire://pair?host=&code=x"))
        assertNull(PairingPayload.parse(""))
        assertNull(PairingPayload.parse(null))
    }

    @Test fun rejectsBadPort() {
        assertNull(PairingPayload.parse("zerowire://pair?host=x&port=0&code=k"))
        assertNull(PairingPayload.parse("zerowire://pair?host=x&port=99999&code=k"))
        assertNull(PairingPayload.parse("zerowire://pair?host=x&port=abc&code=k"))
    }

    @Test fun encodeRoundTrips() {
        val original = PairingPayload(
            host = "192.168.1.42",
            port = 47823,
            code = "PAIR-XYZ_123",
            fingerprint = "abc123",
        )
        val encoded = original.encode()
        val parsed = PairingPayload.parse(encoded)!!
        assertEquals(original.host, parsed.host)
        assertEquals(original.port, parsed.port)
        assertEquals(original.code, parsed.code)
        assertEquals(original.fingerprint, parsed.fingerprint)
    }

    @Test fun encodeUrlEscapesHost() {
        val p = PairingPayload(host = "[::1]", port = 47823, code = "code")
        val enc = p.encode()
        // The literal brackets must not survive — they're invalid in a URI host.
        assertEquals(true, enc.contains("%5B"))
        assertEquals(true, enc.contains("%5D"))
    }

    @Test fun displayEndpointHumanReadable() {
        val p = PairingPayload(host = "phone.local", port = 47823, code = "k")
        assertEquals("phone.local:47823", p.displayEndpoint())
    }

    @Test fun parsedEncodedRoundTripIsStable() {
        // Encoding twice produces the same string — needed by the GUI
        // which renders the QR once and caches the texture.
        val p = PairingPayload(host = "10.0.0.5", port = 47823, code = "STABLE-1")
        assertNotNull(PairingPayload.parse(p.encode()))
        assertEquals(p.encode(), PairingPayload.parse(p.encode())!!.encode())
    }
}
