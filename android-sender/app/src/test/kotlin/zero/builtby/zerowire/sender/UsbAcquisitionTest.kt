package zero.builtby.zerowire.sender

import android.app.PendingIntent
import android.os.Build
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Pure-unit assertions about [UsbAcquisition] that don't require a
 * device or even an Android Context.
 *
 * The actual permission flow needs an instrumentation test on a real
 * device — see `docs/v0.4-hardware-test.md` for that path.
 */
class UsbAcquisitionTest {

    @Test fun permissionIntentFlagsHonoursApiLevel() {
        // Read whatever build-host SDK we're compiled against. The
        // value is set via reflection in robolectric, but the default
        // `testOptions.unitTests.isReturnDefaultValues = true` makes
        // `Build.VERSION.SDK_INT == 0` here, which we treat as "old".
        val flags = UsbAcquisition.permissionIntentFlags()
        when {
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> {
                assertTrue(
                    "MUTABLE must be set on API 31+ so the system can write EXTRA_DEVICE",
                    flags and PendingIntent.FLAG_MUTABLE != 0
                )
                // IMMUTABLE must NOT be set — they're mutually exclusive.
                assertEquals(0, flags and PendingIntent.FLAG_IMMUTABLE)
            }
            else -> {
                // Pre-S phones don't accept MUTABLE/IMMUTABLE in flags;
                // the call sites must omit them.
                assertEquals(0, flags and PendingIntent.FLAG_MUTABLE)
                assertEquals(0, flags and PendingIntent.FLAG_IMMUTABLE)
            }
        }
        // UPDATE_CURRENT is always required so the same PendingIntent
        // slot is reused across device requests.
        assertTrue(
            "UPDATE_CURRENT must be set so the system overwrites stale extras",
            flags and PendingIntent.FLAG_UPDATE_CURRENT != 0
        )
    }

    @Test fun permissionActionIsAppScoped() {
        // The action string must include our package prefix so other
        // apps can't claim it. setPackage() at the call site is the
        // primary defense, but the action name itself helps too.
        assertTrue(
            "ACTION_USB_PERMISSION must be namespaced under our package",
            UsbAcquisition.ACTION_USB_PERMISSION.startsWith("zero.builtby.zerowire.sender")
        )
    }

    @Test fun deviceKindHumanStringsArentBlank() {
        for (k in DeviceKind.values()) {
            assertNotEquals(
                "DeviceKind.${k.name}.human must not be blank",
                "",
                k.human.trim()
            )
        }
    }
}
