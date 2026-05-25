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
import java.net.Socket
import java.util.UUID
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import kotlin.concurrent.thread

/**
 * Foreground service responsible for:
 *   * Owning the TCP listening socket (TLS arrives in v0.2).
 *   * Advertising `_zerowire._tcp.local.` via mDNS.
 *   * Spawning a [ReceiverSession] per incoming connection.
 *   * Tracking active sessions so we can tear them down cleanly on stop.
 *
 * Exposes a process-global [snapshot] so the UI (and `adb shell dumpsys` if
 * we ever add it) can read service health without binding.
 */
class SenderService : Service() {

    companion object {
        private const val TAG = "zerowire/SenderService"
        private const val CHANNEL_ID = "zerowire-status"
        private const val NOTIF_ID = 1
        const val SERVICE_TYPE = "_zerowire._tcp."          // NsdManager wants the trailing dot but no `.local.`
        const val DEFAULT_PORT = 47823

        // -------- diagnostics surface (process-global, single service) --------
        private val state = ServiceState()
        fun snapshot(): ServiceState.Snapshot = state.snapshot()
    }

    private var nsd: NsdManager? = null
    private var regListener: NsdManager.RegistrationListener? = null
    private var serverSocket: ServerSocket? = null
    private val senderId: String = UUID.randomUUID().toString()
    private val sessions = CopyOnWriteArrayList<ReceiverSession>()
    @Volatile private var acceptLoopAlive = true

    override fun onCreate() {
        super.onCreate()
        ensureChannel()
        startForeground(NOTIF_ID, buildNotification("Starting…"))
        try {
            startListenerAndAdvertise()
        } catch (e: Throwable) {
            Log.e(TAG, "service startup failed", e)
            state.recordError(e.message ?: e.javaClass.simpleName)
            updateNotification("startup failed: ${e.message}")
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int = START_STICKY

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onDestroy() {
        super.onDestroy()
        acceptLoopAlive = false
        regListener?.let { l ->
            try { nsd?.unregisterService(l) } catch (_: Throwable) { /* ignore */ }
        }
        try { serverSocket?.close() } catch (_: Throwable) { /* ignore */ }
        sessions.forEach { runCatching { it.stop() } }
        sessions.clear()
        state.reset()
    }

    private fun startListenerAndAdvertise() {
        val socket = ServerSocket(DEFAULT_PORT)
        serverSocket = socket
        val port = socket.localPort
        state.setPort(port)
        Log.i(TAG, "TCP listener bound on 0.0.0.0:$port  (sender_id=$senderId)")

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
                    state.setMdnsRegistered(true)
                    updateNotification("Idle — advertising as ${svc.serviceName}")
                }
                override fun onRegistrationFailed(svc: NsdServiceInfo, errorCode: Int) {
                    Log.e(TAG, "mDNS registration failed: $errorCode")
                    state.setMdnsRegistered(false)
                    state.recordError("mDNS errorCode=$errorCode")
                    updateNotification("mDNS failed; direct IP only")
                }
                override fun onServiceUnregistered(svc: NsdServiceInfo) {
                    Log.i(TAG, "mDNS unregistered")
                    state.setMdnsRegistered(false)
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
                val peer = client.inetAddress?.hostAddress ?: "?"
                Log.i(TAG, "accepted connection from $peer")
                state.incSessions()
                updateNotification("Connected: ${state.snapshot().sessions} session(s)")
                val session = ReceiverSession(
                    socket = client,
                    ctx = applicationContext,
                    senderId = senderId,
                    deviceName = Build.MODEL ?: "Android",
                )
                sessions.add(session)
                thread(start = true, isDaemon = true, name = "zerowire-session") {
                    try {
                        session.run()
                    } finally {
                        sessions.remove(session)
                        state.decSessions()
                        val n = state.snapshot().sessions
                        updateNotification(if (n == 0) "Idle — advertising" else "Connected: $n session(s)")
                    }
                }
            }
        }
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

    private fun updateNotification(text: String) {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        try {
            nm.notify(NOTIF_ID, buildNotification(text))
        } catch (e: Throwable) {
            Log.w(TAG, "notification update failed: ${e.message}")
        }
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

/**
 * Process-global service health snapshot. Single service instance per
 * process so we get away with statics; if the service is ever bound to
 * multiple instances we'll move this onto the binder.
 */
class ServiceState {
    private val port = AtomicInteger(0)
    private val sessions = AtomicInteger(0)
    @Volatile private var mdnsRegistered = false
    private val lastError = AtomicReference<String?>(null)

    fun setPort(p: Int) { port.set(p) }
    fun setMdnsRegistered(b: Boolean) { mdnsRegistered = b }
    fun incSessions() { sessions.incrementAndGet() }
    fun decSessions() { sessions.updateAndGet { (it - 1).coerceAtLeast(0) } }
    fun recordError(msg: String) { lastError.set(msg) }
    fun reset() {
        port.set(0); sessions.set(0); mdnsRegistered = false; lastError.set(null)
    }

    fun snapshot(): Snapshot = Snapshot(
        port = port.get(),
        sessions = sessions.get(),
        mdnsRegistered = mdnsRegistered,
        lastError = lastError.get(),
    )

    data class Snapshot(
        val port: Int,
        val sessions: Int,
        val mdnsRegistered: Boolean,
        val lastError: String?,
    )
}
