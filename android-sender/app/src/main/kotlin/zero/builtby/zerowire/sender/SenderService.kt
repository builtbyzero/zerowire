package zero.builtby.zerowire.sender

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Build
import android.os.IBinder
import android.util.Log
import java.net.ServerSocket
import java.util.UUID

/**
 * Foreground service responsible for, eventually:
 *   * Owning the TCP/TLS listening socket.
 *   * Advertising `_zerowire._tcp.local.` via mDNS.
 *   * Tracking active receiver sessions.
 *
 * Today it does mDNS advertisement + binds a listening socket on the
 * zerowire default port (47823), and ignores anyone who connects. The
 * actual session loop is not implemented yet — that's the next milestone.
 */
class SenderService : Service() {

    companion object {
        private const val TAG = "zerowire/SenderService"
        private const val CHANNEL_ID = "zerowire-status"
        private const val NOTIF_ID = 1
        const val SERVICE_TYPE = "_zerowire._tcp."          // NsdManager wants the trailing dot but no `.local.`
        const val DEFAULT_PORT = 47823
    }

    private var nsd: NsdManager? = null
    private var regListener: NsdManager.RegistrationListener? = null
    private var serverSocket: ServerSocket? = null
    private val senderId: String = UUID.randomUUID().toString()

    override fun onCreate() {
        super.onCreate()
        ensureChannel()
        startForeground(NOTIF_ID, buildNotification("Idle"))
        startListenerAndAdvertise()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int = START_STICKY

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onDestroy() {
        super.onDestroy()
        regListener?.let { l ->
            try { nsd?.unregisterService(l) } catch (_: Throwable) { /* ignore */ }
        }
        try { serverSocket?.close() } catch (_: Throwable) { /* ignore */ }
    }

    private fun startListenerAndAdvertise() {
        val socket = ServerSocket(DEFAULT_PORT)
        serverSocket = socket
        val port = socket.localPort

        nsd = (getSystemService(Context.NSD_SERVICE) as NsdManager).also { mgr ->
            val info = NsdServiceInfo().apply {
                serviceName = "zerowire-${senderId.take(8)}"
                serviceType = SERVICE_TYPE
                this.port = port
                // TXT records per ARCHITECTURE.md §4.
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.LOLLIPOP) {
                    setAttribute("v", "1")
                    setAttribute("id", senderId)
                    setAttribute("n", Build.MODEL ?: "Android")
                    setAttribute("port", port.toString())
                    setAttribute("caps", "usbip,hid")
                }
            }
            val listener = object : NsdManager.RegistrationListener {
                override fun onServiceRegistered(svc: NsdServiceInfo) {
                    Log.i(TAG, "mDNS registered: ${svc.serviceName} on port ${svc.port}")
                }
                override fun onRegistrationFailed(svc: NsdServiceInfo, errorCode: Int) {
                    Log.e(TAG, "mDNS registration failed: $errorCode")
                }
                override fun onServiceUnregistered(svc: NsdServiceInfo) {
                    Log.i(TAG, "mDNS unregistered")
                }
                override fun onUnregistrationFailed(svc: NsdServiceInfo, errorCode: Int) {
                    Log.e(TAG, "mDNS unregistration failed: $errorCode")
                }
            }
            regListener = listener
            mgr.registerService(info, NsdManager.PROTOCOL_DNS_SD, listener)
        }
        // TODO(v0.1): accept() loop on serverSocket, hand each Socket to a
        // SessionHandler that runs the zerowire HELLO → AUTH → device list flow.
    }

    private fun buildNotification(state: String): Notification {
        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, CHANNEL_ID)
        } else {
            @Suppress("DEPRECATION")
            Notification.Builder(this)
        }
        return builder
            .setContentTitle("zerowire")
            .setContentText(state)
            .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth) // placeholder
            .setOngoing(true)
            .build()
    }

    private fun ensureChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            if (nm.getNotificationChannel(CHANNEL_ID) == null) {
                val ch = NotificationChannel(
                    CHANNEL_ID,
                    "zerowire status",
                    NotificationManager.IMPORTANCE_LOW
                )
                nm.createNotificationChannel(ch)
            }
        }
    }
}
