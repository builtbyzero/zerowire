package zero.builtby.zerowire.sender

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.hardware.usb.UsbConstants
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbDeviceConnection
import android.hardware.usb.UsbInterface
import android.hardware.usb.UsbManager
import android.os.Build
import android.util.Log
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlin.coroutines.resume

/**
 * Device-acquisition layer for the sender.
 *
 * Sits between [MainScreen]/[SenderViewModel] and the URB pump in
 * [UsbIpHost]. Its job:
 *
 *   1. Enumerate attached USB devices via [UsbManager] (no permission needed).
 *   2. Ask the user for `USB_DEVICE_PERMISSION` per device — Android's
 *      "share this USB device with zerowire?" system dialog.
 *   3. `openDevice()` → `claimInterface()` for every interface so the
 *      pump can issue control/bulk/interrupt URBs against any endpoint.
 *   4. Watch for attach/detach broadcasts so the UI state survives the
 *      user yanking the cable mid-session.
 *
 * # Why a dedicated layer
 *
 * v0.3 baked permission requests into `MainActivity.onCreate`, which was
 * fine for the walking-skeleton XML UI but doesn't survive Compose
 * recomposition and didn't give the service a clean place to acquire
 * the device on its own thread. v0.4 collapses everything into this
 * object so both the UI and the [SenderService] can call the same code.
 *
 * # PendingIntent flags
 *
 * On API 31+ (Android 12) `PendingIntent` flags must be either
 * `FLAG_IMMUTABLE` or `FLAG_MUTABLE` — older code that omitted the flag
 * crashes hard. The USB permission flow needs the **system** to put
 * extras (`EXTRA_DEVICE`, `EXTRA_PERMISSION_GRANTED`) into the intent
 * it broadcasts back to us, which means the intent must be MUTABLE for
 * pre-S behavior to keep working. We always pair MUTABLE with
 * `setPackage(packageName)` so other apps can't hijack the broadcast.
 */
object UsbAcquisition {

    private const val TAG = "zerowire/Acquisition"
    const val ACTION_USB_PERMISSION = "zero.builtby.zerowire.sender.USB_PERMISSION"

    /**
     * Build the PendingIntent that the USB permission dialog hands back
     * to our broadcast receiver. Public so tests can pin the flag math.
     */
    fun buildPermissionIntent(context: Context, requestCode: Int = 0): PendingIntent {
        val intent = Intent(ACTION_USB_PERMISSION).setPackage(context.packageName)
        return PendingIntent.getBroadcast(context, requestCode, intent, permissionIntentFlags())
    }

    /**
     * Bitmask we feed [PendingIntent.getBroadcast]. Exposed so unit
     * tests can pin the value without standing up a Context.
     *
     * Why `MUTABLE | UPDATE_CURRENT`:
     *  * MUTABLE: the system writes `EXTRA_DEVICE` and
     *    `EXTRA_PERMISSION_GRANTED` into the intent before broadcasting
     *    it. An IMMUTABLE PendingIntent would silently drop those extras.
     *  * UPDATE_CURRENT: re-requesting permission for a different device
     *    must reuse the same PendingIntent slot but with fresh extras.
     */
    fun permissionIntentFlags(): Int =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            PendingIntent.FLAG_MUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        } else {
            PendingIntent.FLAG_UPDATE_CURRENT
        }

    /** Pluggable, no-permission-required snapshot of `UsbManager.deviceList`. */
    fun listAttached(context: Context): List<UsbDevice> {
        val um = context.getSystemService(Context.USB_SERVICE) as UsbManager
        return um.deviceList.values.toList()
    }

    /** True iff the user has already approved sharing [device] with us. */
    fun hasPermission(context: Context, device: UsbDevice): Boolean {
        val um = context.getSystemService(Context.USB_SERVICE) as UsbManager
        return um.hasPermission(device)
    }

    /**
     * Suspending permission request: kicks off the system dialog and
     * resumes with the user's decision.
     *
     * Internally we register a one-shot broadcast receiver scoped to
     * this request; on grant/deny we tear it down, regardless of
     * whether the user backgrounded the app between request and reply.
     */
    suspend fun requestPermission(context: Context, device: UsbDevice): Boolean {
        val um = context.getSystemService(Context.USB_SERVICE) as UsbManager
        if (um.hasPermission(device)) return true

        return suspendCancellableCoroutine { cont ->
            val receiver = object : BroadcastReceiver() {
                override fun onReceive(ctx: Context, intent: Intent) {
                    if (intent.action != ACTION_USB_PERMISSION) return
                    val granted = intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false)
                    val target = intent.getParcelableExtraCompat(UsbManager.EXTRA_DEVICE, UsbDevice::class.java)
                    if (target == null || target.deviceName != device.deviceName) {
                        // Reply for a different device or no device at all. Ignore
                        // and keep waiting — `setPackage()` already filtered hostile
                        // senders, so this is only "noise from another request".
                        return
                    }
                    try { ctx.unregisterReceiver(this) } catch (_: Throwable) { /* fine */ }
                    if (cont.isActive) cont.resume(granted)
                }
            }
            val flags = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                Context.RECEIVER_NOT_EXPORTED
            } else {
                0
            }
            context.registerReceiver(receiver, IntentFilter(ACTION_USB_PERMISSION), flags)
            cont.invokeOnCancellation {
                try { context.unregisterReceiver(receiver) } catch (_: Throwable) {}
            }
            val pi = buildPermissionIntent(context, requestCode = device.deviceId)
            um.requestPermission(device, pi)
        }
    }

    /**
     * Open [device] and claim every declared interface.
     *
     * Returns null if the device is gone, permission was revoked, or
     * we couldn't claim a single interface (which means the pump would
     * fail every URB and is not worth starting).
     */
    fun open(context: Context, device: UsbDevice): AcquiredDevice? {
        val um = context.getSystemService(Context.USB_SERVICE) as UsbManager
        if (!um.hasPermission(device)) {
            Log.w(TAG, "open: missing USB permission for ${device.deviceName}")
            return null
        }
        val conn = um.openDevice(device) ?: run {
            Log.w(TAG, "open: openDevice returned null for ${device.deviceName}")
            return null
        }
        val claimed = mutableListOf<UsbInterface>()
        for (i in 0 until device.interfaceCount) {
            val iface = device.getInterface(i)
            // force=true so we win even if AOSP attached a kernel driver
            // (rare for stock devices on a phone, but happens for USB-PD
            // hubs, USB-Audio class, etc.).
            if (conn.claimInterface(iface, true)) {
                claimed += iface
            } else {
                Log.w(TAG, "claimInterface failed: iface=${iface.id} dev=${device.deviceName}")
            }
        }
        if (claimed.isEmpty()) {
            Log.w(TAG, "open: no interfaces claimable on ${device.deviceName}, releasing")
            conn.close()
            return null
        }
        return AcquiredDevice(device, conn, claimed)
    }

    /**
     * Categorize [device] for the UI's "what is this?" hint. Cheap heuristic
     * based on the first interface's `interfaceClass`, exposed here so the
     * Compose layer doesn't reach into [UsbConstants].
     */
    fun describe(device: UsbDevice): DeviceKind {
        if (device.interfaceCount == 0) return DeviceKind.Other
        val ifc = device.getInterface(0)
        return when (ifc.interfaceClass) {
            UsbConstants.USB_CLASS_HID -> DeviceKind.HumanInputDevice
            UsbConstants.USB_CLASS_MASS_STORAGE -> DeviceKind.MassStorage
            UsbConstants.USB_CLASS_AUDIO -> DeviceKind.Audio
            UsbConstants.USB_CLASS_VIDEO -> DeviceKind.Video
            UsbConstants.USB_CLASS_PRINTER -> DeviceKind.Printer
            UsbConstants.USB_CLASS_COMM -> DeviceKind.Serial
            else -> DeviceKind.Other
        }
    }
}

enum class DeviceKind(val human: String) {
    HumanInputDevice("Mouse / keyboard / gamepad"),
    MassStorage("USB drive"),
    Audio("Audio (likely won't work — iso unsupported)"),
    Video("Webcam (likely won't work — iso unsupported)"),
    Printer("Printer"),
    Serial("Serial / modem"),
    Other("USB device"),
}

/**
 * A [UsbDevice] paired with the live `UsbDeviceConnection` and the set
 * of interfaces we successfully claimed. The pump owns this for the
 * duration of a sharing session and calls [close] when done.
 */
class AcquiredDevice(
    val device: UsbDevice,
    val connection: UsbDeviceConnection,
    private val claimedInterfaces: List<UsbInterface>,
) : AutoCloseable {

    /** Friendly name for notifications + UI. */
    val displayName: String
        get() = device.productName ?: device.manufacturerName ?: device.deviceName

    /** USB/IP-style busid the receiver references in CMD_SUBMIT envelopes. */
    val busid: String
        get() = UsbInventory.busidFor(device)

    override fun close() {
        for (iface in claimedInterfaces) {
            try { connection.releaseInterface(iface) } catch (_: Throwable) {}
        }
        try { connection.close() } catch (_: Throwable) {}
    }
}

/**
 * API-level compat shim for `Intent.getParcelableExtra`. The non-Class
 * overload is deprecated on API 33+ and gone-soon-anyway on API 34.
 */
@Suppress("DEPRECATION")
internal fun <T> Intent.getParcelableExtraCompat(name: String, clazz: Class<T>): T? =
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
        getParcelableExtra(name, clazz)
    } else {
        getParcelableExtra<android.os.Parcelable>(name) as? T
    }
