package zero.builtby.zerowire.sender

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.os.Build
import android.os.Bundle
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity

/**
 * Walking-skeleton entry point for the zerowire sender app.
 *
 * Behaviour today:
 *   * Enumerates currently-plugged USB devices and renders a list.
 *   * Generates a 6-digit pairing code for the session.
 *   * Starts [SenderService] which advertises `_zerowire._tcp.local.` via mDNS.
 *
 * What is *not* here yet:
 *   * TLS / PSK pairing exchange
 *   * Any actual receiver session handling (the service binds the listening
 *     socket but doesn't accept yet)
 *   * UI for per-device authorization
 */
private const val ACTION_USB_PERMISSION = "zero.builtby.zerowire.sender.USB_PERMISSION"

class MainActivity : AppCompatActivity() {

    private val permissionReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (intent.action == ACTION_USB_PERMISSION) {
                renderDevices()  // re-render so the [HID] flag is correct after permission
            }
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        requestUsbPermissionForPluggedInDevices()
        renderDevices()
        renderPairingCode()
        renderConnectHint()
        startService(Intent(this, SenderService::class.java))
        findViewById<TextView>(R.id.status).text = getString(R.string.status_advertising)

        val flags = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            Context.RECEIVER_NOT_EXPORTED
        } else {
            0
        }
        registerReceiver(permissionReceiver, IntentFilter(ACTION_USB_PERMISSION), flags)
    }

    override fun onResume() {
        super.onResume()
        renderDevices()
    }

    override fun onDestroy() {
        try { unregisterReceiver(permissionReceiver) } catch (_: Throwable) { /* ignore */ }
        super.onDestroy()
    }

    /** Ask the user once per device per session for USB permission. */
    private fun requestUsbPermissionForPluggedInDevices() {
        val um = getSystemService(Context.USB_SERVICE) as UsbManager
        val pi = PendingIntent.getBroadcast(
            this,
            0,
            Intent(ACTION_USB_PERMISSION).setPackage(packageName),
            PendingIntent.FLAG_MUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        for (d in um.deviceList.values) {
            if (!um.hasPermission(d)) {
                um.requestPermission(d, pi)
            }
        }
    }

    private fun renderConnectHint() {
        val tv = findViewById<TextView>(R.id.connect_command)
        tv.text = getString(R.string.connect_command, Build.MODEL ?: "Android")
    }

    private fun renderDevices() {
        val um = getSystemService(Context.USB_SERVICE) as UsbManager
        val devices: Collection<UsbDevice> = um.deviceList.values
        val tv = findViewById<TextView>(R.id.device_list)
        if (devices.isEmpty()) {
            tv.text = getString(R.string.no_devices)
            return
        }
        tv.text = devices.joinToString("\n") { d ->
            // Render in a roughly USB/IP-busid-shaped form.
            val isHid = (0 until d.interfaceCount).any {
                d.getInterface(it).interfaceClass == 0x03 /* HID */
            }
            val name = d.productName ?: d.deviceName
            "%-12s  %04x:%04x  %s  %s".format(
                d.deviceName.substringAfterLast('/'),
                d.vendorId,
                d.productId,
                if (isHid) "[HID]" else "     ",
                name
            )
        }
    }

    private fun renderPairingCode() {
        val code = PairingCodes.sixDigit()
        findViewById<TextView>(R.id.pairing_code).text = code
    }
}
