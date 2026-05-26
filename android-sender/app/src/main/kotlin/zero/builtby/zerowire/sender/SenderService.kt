package zero.builtby.zerowire.sender

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.hardware.usb.UsbDevice
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Binder
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.core.app.NotificationCompat
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.net.ServerSocket
import java.net.Socket
import java.util.UUID
import java.util.concurrent.CopyOnWriteArrayList
import kotlin.concurrent.thread

/**
 * Foreground service responsible for:
 *   * Owning the TCP listening socket.
 *   * Advertising `_zerowire._tcp.local.` via mDNS.
 *   * Spawning a [ReceiverSession] per incoming connection.
 *   * Driving the persistent notification that's required on Android
 *     14+ for sustained USB access (`ForegroundServiceType.connectedDevice`).
 *
 * The notification carries:
 *   * Connected device name (or "Idle" when nothing is bound).
 *   * Receiver endpoint (peer `host:port` of the latest session).
 *   * Live bytes/sec gauge sampled from the URB pump.
 *   * A Stop action that tears the whole session down.
 *
 * UI binding: [MainScreen] binds to this service via [LocalBinder] and
 * subscribes to [state]. That keeps the service authoritative about
 * the current session — the UI is a thin window onto its [StateFlow].
 */
class SenderService : Service() {

    companion object {
        private const val TAG = "zerowire/SenderService"
        private const val CHANNEL_ID = "zerowire-status"
        private const val NOTIF_ID = 1
        const val SERVICE_TYPE = "_zerowire._tcp."          // NsdManager wants the trailing dot but no `.local.`
        const val DEFAULT_PORT = 47823

        const val ACTION_STOP = "zero.builtby.zerowire.sender.STOP"
        const val ACTION_START_FOREGROUND = "zero.builtby.zerowire.sender.START"
    }

    private var nsd: NsdManager? = null
    private var regListener: NsdManager.RegistrationListener? = null
    private var serverSocket: ServerSocket? = null
    private val senderId: String = UUID.randomUUID().toString()
    private val sessions = CopyOnWriteArrayList<ReceiverSession>()
    @Volatile private var acceptLoopAlive = true

    private val binder = LocalBinder()
    private val _state = MutableStateFlow(SenderState.Idle as SenderState)
    val state: StateFlow<SenderState> = _state.asStateFlow()

    private var notifSampler: Thread? = null

    override fun onCreate() {
        super.onCreate()
        ensureChannel()
        startForeground(NOTIF_ID, buildNotification(_state.value))
        startListenerAndAdvertise()
        startNotificationSampler()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                Log.i(TAG, "STOP action received from notification")
                stopAllSessions()
                stopSelf()
            }
        }
        return START_STICKY
    }

    override fun onBind(intent: Intent?): IBinder = binder

    override fun onDestroy() {
        super.onDestroy()
        acceptLoopAlive = false
        notifSampler?.interrupt()
        regListener?.let { l ->
            try { nsd?.unregisterService(l) } catch (_: Throwable) { /* ignore */ }
        }
        try { serverSocket?.close() } catch (_: Throwable) { /* ignore */ }
        stopAllSessions()
        _state.value = SenderState.Idle
    }

    /** UI calls this on the binder when the user picks a device + receiver. */
    fun setSharing(device: UsbDevice, receiver: String) {
        _state.value = SenderState.Sharing(
            deviceName = device.productName ?: device.deviceName,
            busid = UsbInventory.busidFor(device),
            receiverEndpoint = receiver,
            bytesPerSec = 0,
        )
        refreshNotification()
    }

    /** UI calls this to surface an error string from acquisition / pairing. */
    fun setError(reason: String) {
        _state.value = SenderState.Error(reason)
        refreshNotification()
    }

    /** UI calls this when the user taps Stop in the app. */
    fun stopAllSessions() {
        sessions.forEach { runCatching { it.stop() } }
        sessions.clear()
        _state.value = SenderState.Idle
        refreshNotification()
    }

    // ---------------- mDNS + accept loop (unchanged from v0.3) ----------------

    private fun startListenerAndAdvertise() {
        val socket = ServerSocket(DEFAULT_PORT)
        serverSocket = socket
        val port = socket.localPort

        nsd = (getSystemService(Context.NSD_SERVICE) as NsdManager).also { mgr ->
            val info = NsdServiceInfo().apply {
                serviceName = "zerowire-${senderId.take(8)}"
                serviceType = SERVICE_TYPE
                this.port = port
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
        startAcceptLoop(socket)
    }

    private fun startAcceptLoop(server: ServerSocket) {
        thread(start = true, isDaemon = true, name = "zerowire-accept") {
            Log.i(TAG, "accept loop running on port ${server.localPort}")
            while (acceptLoopAlive && !server.isClosed) {
                val client: Socket = try {
                    server.accept()
                } catch (e: Throwable) {
                    if (acceptLoopAlive) Log.w(TAG, "accept failed: ${e.message}")
                    break
                }
                val peer = "${client.inetAddress?.hostAddress ?: "?"}:${client.port}"
                val session = ReceiverSession(
                    socket = client,
                    ctx = applicationContext,
                    senderId = senderId,
                    deviceName = Build.MODEL ?: "Android",
                )
                sessions.add(session)
                _state.value = SenderState.Connected(peer)
                refreshNotification()
                thread(start = true, isDaemon = true, name = "zerowire-session") {
                    try { session.run() } finally {
                        sessions.remove(session)
                        if (sessions.isEmpty()) {
                            _state.value = SenderState.Idle
                            refreshNotification()
                        }
                    }
                }
            }
        }
    }

    // ---------------- notification ----------------

    /**
     * Background thread that re-renders the notification once per second
     * while a session is running. Sampling here keeps the URB pump's
     * hot loop free of clock calls and the notification rate-limited
     * regardless of how fast bytes flow.
     */
    private fun startNotificationSampler() {
        notifSampler = thread(start = true, isDaemon = true, name = "zerowire-notif") {
            var lastBytes = 0L
            while (acceptLoopAlive) {
                try { Thread.sleep(1000) } catch (_: InterruptedException) { break }
                val total = sessions.sumOf { it.bytesTransferred() }
                val perSec = (total - lastBytes).coerceAtLeast(0)
                lastBytes = total
                val cur = _state.value
                if (cur is SenderState.Sharing) {
                    _state.value = cur.copy(bytesPerSec = perSec)
                    refreshNotification()
                }
            }
        }
    }

    private fun refreshNotification() {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        nm.notify(NOTIF_ID, buildNotification(_state.value))
    }

    private fun buildNotification(state: SenderState): Notification {
        val openAppIntent = PendingIntent.getActivity(
            this, 0,
            Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            pendingIntentFlags(),
        )
        val stopIntent = PendingIntent.getService(
            this, 1,
            Intent(this, SenderService::class.java).setAction(ACTION_STOP),
            pendingIntentFlags(),
        )

        val title: String
        val text: String
        val sub: String?
        when (state) {
            is SenderState.Idle -> {
                title = getString(R.string.notif_idle_title)
                text = getString(R.string.notif_idle_text)
                sub = null
            }
            is SenderState.Connected -> {
                title = getString(R.string.notif_connected_title)
                text = getString(R.string.notif_connected_text, state.receiverEndpoint)
                sub = null
            }
            is SenderState.Sharing -> {
                title = getString(R.string.notif_sharing_title, state.deviceName)
                text = getString(
                    R.string.notif_sharing_text,
                    state.receiverEndpoint,
                    formatRate(state.bytesPerSec),
                )
                sub = state.busid
            }
            is SenderState.Error -> {
                title = getString(R.string.notif_error_title)
                text = state.reason
                sub = null
            }
        }

        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
            .setContentTitle(title)
            .setContentText(text)
            .setSubText(sub)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setContentIntent(openAppIntent)
            .addAction(
                android.R.drawable.ic_menu_close_clear_cancel,
                getString(R.string.notif_stop_action),
                stopIntent,
            )
            .setStyle(NotificationCompat.BigTextStyle().bigText(text))
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .build()
    }

    private fun pendingIntentFlags(): Int =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        } else {
            PendingIntent.FLAG_UPDATE_CURRENT
        }

    private fun ensureChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            if (nm.getNotificationChannel(CHANNEL_ID) == null) {
                val ch = NotificationChannel(
                    CHANNEL_ID,
                    getString(R.string.notif_channel_name),
                    NotificationManager.IMPORTANCE_LOW
                ).apply {
                    description = getString(R.string.notif_channel_description)
                    setShowBadge(false)
                }
                nm.createNotificationChannel(ch)
            }
        }
    }

    /** AIDL-free binder. UI binds in-process and gets the service back. */
    inner class LocalBinder : Binder() {
        val service: SenderService get() = this@SenderService
    }
}

/**
 * Format a bytes/sec rate as a tight human string for the notification.
 * Public + free-standing so the UI can use the same renderer.
 */
fun formatRate(bytesPerSec: Long): String {
    val b = bytesPerSec.coerceAtLeast(0)
    return when {
        b < 1_000 -> "$b B/s"
        b < 1_000_000 -> "%.1f kB/s".format(b / 1_000.0)
        b < 1_000_000_000 -> "%.1f MB/s".format(b / 1_000_000.0)
        else -> "%.2f GB/s".format(b / 1_000_000_000.0)
    }
}

/**
 * Sealed UI state. Public so [MainScreen] composables can pattern-match
 * on the active variant without leaking service internals.
 */
sealed interface SenderState {
    data object Idle : SenderState
    data class Connected(val receiverEndpoint: String) : SenderState
    data class Sharing(
        val deviceName: String,
        val busid: String,
        val receiverEndpoint: String,
        val bytesPerSec: Long,
    ) : SenderState
    data class Error(val reason: String) : SenderState
}
