package zero.builtby.zerowire.sender

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Cheap deterministic checks on the rate formatter — the notification
 * text relies on this so it's worth nailing the boundaries.
 */
class SenderStateTest {

    @Test fun formatRateUsesBytesUnderOneKb() {
        assertEquals("0 B/s", formatRate(0))
        assertEquals("999 B/s", formatRate(999))
    }

    @Test fun formatRateSwitchesToKbAtOneThousand() {
        assertEquals("1.0 kB/s", formatRate(1_000))
        assertEquals("500.0 kB/s", formatRate(500_000))
    }

    @Test fun formatRateUsesMbAndGb() {
        assertEquals("1.0 MB/s", formatRate(1_000_000))
        assertEquals("1.00 GB/s", formatRate(1_000_000_000))
    }

    @Test fun formatRateClampsNegatives() {
        assertEquals("0 B/s", formatRate(-1234))
    }

    @Test fun senderStateSharingPreservesBusid() {
        val s = SenderState.Sharing(
            deviceName = "Logitech mouse",
            busid = "1-2",
            receiverEndpoint = "192.168.1.5:47823",
            bytesPerSec = 1234,
        )
        val updated = s.copy(bytesPerSec = 5678)
        assertEquals("1-2", updated.busid)
        assertEquals(5678, updated.bytesPerSec)
    }
}
