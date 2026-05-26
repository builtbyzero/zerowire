package zero.builtby.zerowire.sender

import android.content.Context
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map

/**
 * Tiny DataStore-backed key/value store for things the UI wants
 * remembered across launches:
 *
 *   * `receiver_host` — last-known `host:port` the user pointed the
 *     sender at. Pre-filled into the input field on next launch.
 *   * `last_pairing_code` — the most recent pairing code, so a
 *     re-pair after kill-the-app doesn't always need a fresh QR.
 *     Stored opportunistically; the user can always re-scan.
 *   * `last_busid` — `busid` of the device that was sharing last.
 *     Lets the UI pre-select that row when the same stick is plugged
 *     back in.
 *
 * No proto/typed schema — Preferences DataStore is enough.
 */
class SettingsStore(private val store: DataStore<Preferences>) {

    val receiverHost: Flow<String?> = store.data.map { it[KEY_RECEIVER_HOST] }
    val lastPairingCode: Flow<String?> = store.data.map { it[KEY_PAIRING_CODE] }
    val lastBusid: Flow<String?> = store.data.map { it[KEY_BUSID] }

    suspend fun setReceiverHost(value: String) {
        store.edit { it[KEY_RECEIVER_HOST] = value }
    }

    suspend fun setLastPairingCode(value: String) {
        store.edit { it[KEY_PAIRING_CODE] = value }
    }

    suspend fun setLastBusid(value: String) {
        store.edit { it[KEY_BUSID] = value }
    }

    companion object {
        private val KEY_RECEIVER_HOST = stringPreferencesKey("receiver_host")
        private val KEY_PAIRING_CODE = stringPreferencesKey("pairing_code")
        private val KEY_BUSID = stringPreferencesKey("last_busid")

        private val Context.dataStore by preferencesDataStore(name = "zerowire-sender")

        fun from(context: Context): SettingsStore =
            SettingsStore(context.applicationContext.dataStore)
    }
}
