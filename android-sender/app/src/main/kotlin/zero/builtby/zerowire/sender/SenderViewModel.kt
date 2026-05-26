package zero.builtby.zerowire.sender

import android.app.Application
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.hardware.usb.UsbDevice
import android.os.IBinder
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch

/**
 * Lifecycle-scoped glue between the Compose UI ([MainScreen]) and the
 * [SenderService] binder.
 *
 *   * Owns a [ServiceConnection] that survives recomposition.
 *   * Exposes [serviceState] — a [StateFlow] that mirrors the bound
 *     service's [SenderService.state] but emits [SenderState.Idle]
 *     while unbound (the UI must always have *something* to render).
 *   * Mirrors [SettingsStore] values (receiver host) as [StateFlow]s
 *     so the Compose layer can `collectAsState` without remembering
 *     a coroutine scope per text field.
 */
@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class SenderViewModel(app: Application) : AndroidViewModel(app) {

    private val settings = SettingsStore.from(app)

    /** Live USB-device snapshot. Manually refreshed by the UI when the user pulls. */
    private val _devices = MutableStateFlow<List<UsbDevice>>(emptyList())
    val devices: StateFlow<List<UsbDevice>> = _devices.asStateFlow()

    /** Last scanned/typed pairing payload. Null until the user pairs. */
    private val _pairing = MutableStateFlow<PairingPayload?>(null)
    val pairing: StateFlow<PairingPayload?> = _pairing.asStateFlow()

    private val _bound = MutableStateFlow<SenderService?>(null)

    /** Mirror of the service's state, or [SenderState.Idle] when unbound. */
    val serviceState: StateFlow<SenderState> = _bound
        .flatMapLatest { svc -> svc?.state ?: flowOf(SenderState.Idle) }
        .stateIn(viewModelScope, SharingStarted.Eagerly, SenderState.Idle)

    /** Editable receiver host string the UI shows in its field. */
    val receiverHost: StateFlow<String> = MutableStateFlow("")
    private val _receiverHost get() = receiverHost as MutableStateFlow<String>

    init {
        viewModelScope.launch {
            settings.receiverHost.collect { saved ->
                if (!saved.isNullOrBlank() && _receiverHost.value.isBlank()) {
                    _receiverHost.value = saved
                }
            }
        }
    }

    /** UI calls this on attach/refresh / lifecycle resume. */
    fun refreshDevices() {
        _devices.value = UsbAcquisition.listAttached(getApplication())
    }

    fun setReceiverHost(value: String) {
        _receiverHost.value = value
        viewModelScope.launch { settings.setReceiverHost(value) }
    }

    fun applyPairing(payload: PairingPayload) {
        _pairing.value = payload
        _receiverHost.value = payload.displayEndpoint()
        viewModelScope.launch {
            settings.setReceiverHost(payload.displayEndpoint())
            settings.setLastPairingCode(payload.code)
        }
    }

    fun clearPairing() {
        _pairing.value = null
    }

    private var permissionJob: Job? = null

    /**
     * Kick off "share this device" — request USB permission if needed,
     * then ask the service to flip into Sharing state.
     */
    fun shareDevice(device: UsbDevice) {
        permissionJob?.cancel()
        permissionJob = viewModelScope.launch {
            val ctx = getApplication<Application>()
            val granted = UsbAcquisition.requestPermission(ctx, device)
            if (!granted) {
                _bound.value?.setError(ctx.getString(R.string.error_usb_denied))
                return@launch
            }
            val receiver = _receiverHost.value.ifBlank {
                settings.receiverHost.first() ?: ""
            }
            _bound.value?.setSharing(device, receiver)
            viewModelScope.launch {
                settings.setLastBusid(UsbInventory.busidFor(device))
            }
        }
    }

    fun stopSharing() {
        _bound.value?.stopAllSessions()
    }

    // -------- Binder lifecycle --------

    private val connection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName?, binder: IBinder?) {
            val local = binder as? SenderService.LocalBinder ?: return
            _bound.value = local.service
        }

        override fun onServiceDisconnected(name: ComponentName?) {
            _bound.value = null
        }
    }

    fun bind(context: Context) {
        val intent = Intent(context, SenderService::class.java)
        context.startService(intent)
        context.bindService(intent, connection, Context.BIND_AUTO_CREATE)
    }

    fun unbind(context: Context) {
        try { context.unbindService(connection) } catch (_: Throwable) { /* ignore */ }
    }
}
