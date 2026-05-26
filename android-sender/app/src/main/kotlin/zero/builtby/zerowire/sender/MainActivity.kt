package zero.builtby.zerowire.sender

import android.Manifest
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.hardware.usb.UsbManager
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.core.content.ContextCompat
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanIntentResult
import com.journeyapps.barcodescanner.ScanOptions
import android.widget.Toast

/**
 * Compose-based entry point for the v0.4 sender app.
 *
 * Responsibilities:
 *   * Bind to [SenderService] and pass the binder down to [SenderViewModel].
 *   * Register the QR-scan activity result for pairing.
 *   * Watch for USB attach/detach broadcasts and refresh the device list.
 *   * Ask for `POST_NOTIFICATIONS` on Android 13+ once at launch — the
 *     foreground notification is non-optional, so silently failing is
 *     worse than asking up front.
 *
 * All actual logic lives in [SenderViewModel]. This Activity is glue.
 */
class MainActivity : ComponentActivity() {

    private val vm: SenderViewModel by viewModels()

    private val qrLauncher = registerForActivityResult(ScanContract()) { result: ScanIntentResult? ->
        val text = result?.contents
        val payload = PairingPayload.parse(text)
        if (payload == null) {
            Toast.makeText(
                this,
                getString(R.string.pairing_scan_bad),
                Toast.LENGTH_SHORT,
            ).show()
            return@registerForActivityResult
        }
        vm.applyPairing(payload)
    }

    private val notificationPermissionLauncher = registerForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { /* outcome is reflected by the user's next launch; nothing to do here */ }

    private val usbBroadcastReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            when (intent.action) {
                UsbManager.ACTION_USB_DEVICE_ATTACHED,
                UsbManager.ACTION_USB_DEVICE_DETACHED -> vm.refreshDevices()
            }
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        maybeRequestNotificationPermission()
        vm.bind(this)
        vm.refreshDevices()

        setContent {
            ZerowireTheme {
                MainScreen(
                    vm = vm,
                    onScanQr = ::launchQrScan,
                )
            }
        }
    }

    override fun onStart() {
        super.onStart()
        val filter = IntentFilter().apply {
            addAction(UsbManager.ACTION_USB_DEVICE_ATTACHED)
            addAction(UsbManager.ACTION_USB_DEVICE_DETACHED)
        }
        val flags = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            Context.RECEIVER_NOT_EXPORTED
        } else {
            0
        }
        registerReceiver(usbBroadcastReceiver, filter, flags)
    }

    override fun onResume() {
        super.onResume()
        vm.refreshDevices()
    }

    override fun onStop() {
        try { unregisterReceiver(usbBroadcastReceiver) } catch (_: Throwable) {}
        super.onStop()
    }

    override fun onDestroy() {
        vm.unbind(this)
        super.onDestroy()
    }

    private fun launchQrScan() {
        val options = ScanOptions().apply {
            setDesiredBarcodeFormats(ScanOptions.QR_CODE)
            setPrompt(getString(R.string.pairing_scan_prompt))
            setBeepEnabled(false)
            setOrientationLocked(false)
        }
        qrLauncher.launch(options)
    }

    private fun maybeRequestNotificationPermission() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
        val granted = ContextCompat.checkSelfPermission(
            this,
            Manifest.permission.POST_NOTIFICATIONS,
        ) == PackageManager.PERMISSION_GRANTED
        if (!granted) {
            notificationPermissionLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }
}
