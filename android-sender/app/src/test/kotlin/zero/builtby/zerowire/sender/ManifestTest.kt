package zero.builtby.zerowire.sender

import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/**
 * Source-level checks on `AndroidManifest.xml`. These don't need a
 * device or `aapt2` because they just grep the on-disk file — but they
 * guard against the kind of regression where someone "cleans up" the
 * USB intent filter and breaks USB_DEVICE_ATTACHED launch.
 */
class ManifestTest {

    private val manifest: String by lazy {
        val candidates = listOf(
            "src/main/AndroidManifest.xml",
            "app/src/main/AndroidManifest.xml",
            "../app/src/main/AndroidManifest.xml",
        )
        candidates.map { File(it) }.firstOrNull { it.exists() }?.readText()
            ?: error("AndroidManifest.xml not found relative to ${File(".").absolutePath}")
    }

    @Test fun declaresUsbDeviceAttachedFilter() {
        assertTrue(
            "MainActivity must opt in to USB_DEVICE_ATTACHED so the system launches " +
                "us when a device is plugged in.",
            manifest.contains("android.hardware.usb.action.USB_DEVICE_ATTACHED")
        )
    }

    @Test fun bindsDeviceFilterMetadata() {
        // The intent filter has to be paired with a metadata pointer to
        // res/xml/device_filter.xml or Android won't route the broadcast.
        assertTrue(
            "USB_DEVICE_ATTACHED meta-data must point at res/xml/device_filter",
            manifest.contains("@xml/device_filter")
        )
    }

    @Test fun foregroundServiceTypeConnectedDevice() {
        // Android 14+ enforces this — the connectedDevice type is the only
        // legal one for sustained USB sessions.
        assertTrue(
            manifest.contains("android:foregroundServiceType=\"connectedDevice\"")
        )
        assertTrue(
            manifest.contains(
                "android.permission.FOREGROUND_SERVICE_CONNECTED_DEVICE"
            )
        )
    }

    @Test fun declaresPostNotificationsPermission() {
        // Required on Android 13+ for the ongoing foreground notification.
        assertTrue(manifest.contains("android.permission.POST_NOTIFICATIONS"))
    }

    @Test fun declaresCameraForQrPairing() {
        // The new QR pairing flow needs CAMERA at runtime; the in-app zxing
        // scanner handles the prompt but the permission has to be declared.
        assertTrue(manifest.contains("android.permission.CAMERA"))
    }

    @Test fun cameraIsNotRequired() {
        // Phones without a camera should still be installable from the Play
        // Store (Android TV, etc.); QR scan degrades to manual code entry.
        assertTrue(
            manifest.contains("android:name=\"android.hardware.camera\" android:required=\"false\"")
        )
    }
}
