package zero.builtby.zerowire.sender

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity

/**
 * Walking-skeleton entry point for the zerowire sender app.
 *
 * Behaviour today:
 *   * Enumerates currently-plugged USB devices and renders a list.
 *   * Generates a 6-digit pairing code for the session.
 *   * Starts [SenderService] which advertises `_zerowire._tcp.local.` via mDNS.
 *   * Shows a live "diagnostic line" — listener IP:port, active session
 *     count, last error — so a tester can see at a glance whether the
 *     service is healthy before they touch the laptop.
 *
 * What is *not* here yet:
 *   * TLS / PSK pairing exchange
 *   * UI for per-device authorization
 */
private const val ACTION_USB_PERMISSION = "zero.builtby.zerowire.sender.USB_PERMISSION"
private const val TAG = "zerowire/Main"

class MainActivity : AppCompatActivity() {

    private val permissionReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (intent.action == ACTION_USB_PERMISSION) {
                Log.i(TAG, "USB permission broadcast received; refreshing device list")
                renderDevices()
            }
        }
    }

    private val ui = Handler(Looper.getMainLooper())
    private val diagnosticTick = object : Runnable {
        override fun run() {
            renderDiagnosticLine()
            ui.postDelayed(this, 1_000)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        // Register the permission listener *before* we issue any requests, so
        // we can't miss the result if Android grants instantly (cached "always
        // allow" entries).
        val flags = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            Context.RECEIVER_NOT_EXPORTED
        } else {
            0
        }
        registerReceiver(permissionReceiver, IntentFilter(ACTION_USB_PERMISSION), flags)

        requestUsbPermissionForPluggedInDevices()
        renderDevices()
        renderPairingCode()
        renderConnectHint()
        startService(Intent(this, SenderService::class.java))
        findViewById<TextView>(R.id.status).text = getString(R.string.status_advertising)
        ui.post(diagnosticTick)
    }

    override fun onResume() {
        super.onResume()
        renderDevices()
        renderDiagnosticLine()
    }

    override fun onDestroy() {
        ui.removeCallbacks(diagnosticTick)
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
                Log.i(TAG, "requesting USB permission for ${d.deviceName} ${"%04x:%04x".format(d.vendorId, d.productId)}")
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
            val perm = if (um.hasPermission(d)) "OK " else "NEEDS_PERM"
            val name = d.productName ?: d.deviceName
            "%-12s  %04x:%04x  %s  %s  %s".format(
                d.deviceName.substringAfterLast('/'),
                d.vendorId,
                d.productId,
                if (isHid) "[HID]" else "     ",
                perm,
                name
            )
        }
    }

    private fun renderPairingCode() {
        val code = PairingCodes.sixDigit()
        findViewById<TextView>(R.id.pairing_code).text = code
    }

    /**
     * Single-line "what is this app doing right now" — refreshed every
     * second. The most useful line on the whole screen when bring-up isn't
     * working: it tells you the IP:port to feed `--target` if mDNS dies.
     */
    private fun renderDiagnosticLine() {
        val tv = findViewById<TextView>(R.id.diagnostic) ?: return
        val ip = currentWifiIpv4() ?: "(no WiFi IP)"
        val state = SenderService.snapshot()
        val mdns = if (state.mdnsRegistered) "mDNS✓" else "mDNS…"
        val err = state.lastError?.let { "  err=$it" } ?: ""
        tv.text = "listen=%s:%d  %s  sessions=%d%s".format(ip, state.port, mdns, state.sessions, err)
    }

    @Suppress("DEPRECATION")
    private fun currentWifiIpv4(): String? {
        val wm = applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager ?: return null
        val raw = wm.connectionInfo?.ipAddress ?: 0
        if (raw == 0) return null
        return "%d.%d.%d.%d".format(
            raw and 0xFF,
            (raw shr 8) and 0xFF,
            (raw shr 16) and 0xFF,
            (raw shr 24) and 0xFF,
        )
    }
}
