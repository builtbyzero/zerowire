package zero.builtby.zerowire.sender

import android.hardware.usb.UsbDevice
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.QrCodeScanner
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.filled.UsbOff
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import android.os.Build

/**
 * Top-level theme. Honours Material You dynamic colour on Android 12+
 * and falls back to a stable light/dark scheme on older devices, so the
 * sender looks at home on any phone the user actually carries.
 */
@Composable
fun ZerowireTheme(
    darkTheme: Boolean = androidx.compose.foundation.isSystemInDarkTheme(),
    content: @Composable () -> Unit,
) {
    val ctx = LocalContext.current
    val colors = when {
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.S ->
            if (darkTheme) dynamicDarkColorScheme(ctx) else dynamicLightColorScheme(ctx)
        darkTheme -> darkColorScheme()
        else -> lightColorScheme()
    }
    MaterialTheme(colorScheme = colors, content = content)
}

/**
 * The whole UI. One screen, four visual states driven by the service's
 * [SenderState] (idle / connected-pending-share / sharing / error).
 *
 * Composables here intentionally don't call into [UsbAcquisition] or
 * [SenderService] directly — that's all funnelled through the
 * [SenderViewModel] so the screen stays pure presentational.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MainScreen(
    vm: SenderViewModel,
    onScanQr: () -> Unit,
) {
    val state by vm.serviceState.collectAsState()
    val devices by vm.devices.collectAsState()
    val receiver by vm.receiverHost.collectAsState()
    val pairing by vm.pairing.collectAsState()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.app_name)) },
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(horizontal = 16.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp)
        ) {
            StateHeader(state, devicesCount = devices.size)

            ReceiverPanel(
                receiver = receiver,
                onReceiverChange = vm::setReceiverHost,
                onScanQr = onScanQr,
                pairing = pairing,
            )

            DeviceList(
                state = state,
                devices = devices,
                onShareDevice = vm::shareDevice,
                modifier = Modifier.weight(1f, fill = true),
            )

            if (state is SenderState.Sharing) {
                StopButton(onStop = vm::stopSharing)
            }

            Text(
                stringResource(R.string.v04_note),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(bottom = 16.dp)
            )
        }
    }
}

@Composable
private fun StateHeader(state: SenderState, devicesCount: Int) {
    val (title, hint, tone) = when (state) {
        is SenderState.Idle ->
            if (devicesCount == 0)
                Triple(
                    stringResource(R.string.status_idle),
                    stringResource(R.string.status_idle_hint),
                    MaterialTheme.colorScheme.surfaceVariant
                )
            else
                Triple(
                    stringResource(R.string.status_attached),
                    stringResource(R.string.status_attached_hint),
                    MaterialTheme.colorScheme.secondaryContainer
                )
        is SenderState.Connected ->
            Triple(
                stringResource(R.string.status_attached),
                stringResource(R.string.notif_connected_text, state.receiverEndpoint),
                MaterialTheme.colorScheme.secondaryContainer,
            )
        is SenderState.Sharing ->
            Triple(
                "${stringResource(R.string.status_sharing)} ${state.deviceName}",
                "→ ${state.receiverEndpoint} · ${formatRate(state.bytesPerSec)}",
                MaterialTheme.colorScheme.primaryContainer,
            )
        is SenderState.Error ->
            Triple(
                stringResource(R.string.status_error_label),
                state.reason,
                MaterialTheme.colorScheme.errorContainer,
            )
    }
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(containerColor = tone),
    ) {
        Column(Modifier.padding(16.dp)) {
            Text(title, style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.height(4.dp))
            Text(hint, style = MaterialTheme.typography.bodyMedium)
        }
    }
}

@Composable
private fun ReceiverPanel(
    receiver: String,
    onReceiverChange: (String) -> Unit,
    onScanQr: () -> Unit,
    pairing: PairingPayload?,
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(
                stringResource(R.string.receiver_address_label),
                style = MaterialTheme.typography.titleSmall,
            )
            OutlinedTextField(
                value = receiver,
                onValueChange = onReceiverChange,
                singleLine = true,
                placeholder = { Text(stringResource(R.string.receiver_address_placeholder)) },
                modifier = Modifier.fillMaxWidth(),
            )
            Row(
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                FilledTonalButton(onClick = onScanQr) {
                    Icon(Icons.Filled.QrCodeScanner, contentDescription = null)
                    Spacer(Modifier.height(4.dp))
                    Text(stringResource(R.string.pairing_scan_action))
                }
                if (pairing != null) {
                    Text(
                        stringResource(R.string.pairing_code_label) + ": " + pairing.code,
                        style = MaterialTheme.typography.bodySmall,
                        fontFamily = FontFamily.Monospace,
                    )
                }
            }
        }
    }
}

@Composable
private fun DeviceList(
    state: SenderState,
    devices: List<UsbDevice>,
    onShareDevice: (UsbDevice) -> Unit,
    modifier: Modifier = Modifier,
) {
    Card(modifier = modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.device_list_title), style = MaterialTheme.typography.titleSmall)
            if (devices.isEmpty()) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Icon(Icons.Filled.UsbOff, contentDescription = null)
                    Spacer(Modifier.width(8.dp))
                    Text(stringResource(R.string.no_devices))
                }
                return@Card
            }
            val sharingBusid = (state as? SenderState.Sharing)?.busid
            LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                items(devices, key = { it.deviceName }) { d ->
                    DeviceRow(
                        device = d,
                        sharing = sharingBusid != null && sharingBusid == UsbInventory.busidFor(d),
                        onShare = { onShareDevice(d) },
                    )
                }
            }
        }
    }
}

@Composable
private fun DeviceRow(device: UsbDevice, sharing: Boolean, onShare: () -> Unit) {
    val kind = remember(device) { UsbAcquisition.describe(device) }
    val container =
        if (sharing) MaterialTheme.colorScheme.primaryContainer
        else MaterialTheme.colorScheme.surfaceVariant
    Surface(
        color = container,
        shape = RoundedCornerShape(12.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(
            modifier = Modifier.padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f)) {
                Text(
                    device.productName ?: device.deviceName,
                    style = MaterialTheme.typography.bodyLarge,
                )
                Text(
                    "%04x:%04x · %s".format(device.vendorId, device.productId, kind.human),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (sharing) {
                Text(
                    stringResource(R.string.status_sharing),
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.onPrimaryContainer,
                )
            } else {
                Button(onClick = onShare) {
                    Text(stringResource(R.string.action_share))
                }
            }
        }
    }
}

@Composable
private fun StopButton(onStop: () -> Unit) {
    Button(
        onClick = onStop,
        colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Icon(Icons.Filled.Stop, contentDescription = null)
        Spacer(Modifier.width(8.dp))
        Text(stringResource(R.string.action_stop))
    }
}

