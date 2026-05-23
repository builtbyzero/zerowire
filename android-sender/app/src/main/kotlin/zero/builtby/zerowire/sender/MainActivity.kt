package zero.builtby.zerowire.sender

import android.content.Context
import android.content.Intent
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
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
class MainActivity : AppCompatActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        renderDevices()
        renderPairingCode()
        startService(Intent(this, SenderService::class.java))
        findViewById<TextView>(R.id.status).text = getString(R.string.status_advertising)
    }

    override fun onResume() {
        super.onResume()
        renderDevices()
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
