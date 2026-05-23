package zero.builtby.zerowire.sender

import android.hardware.usb.UsbDevice
import org.json.JSONArray
import org.json.JSONObject

/**
 * Bridges Android [UsbDevice]s to the JSON shape the `protocol` crate
 * expects in `ControlMessage::DeviceList`.
 *
 * Kept JSON-only on purpose: the wire envelope for the control channel
 * is "one JSON object per envelope", and a tiny project doesn't need a
 * Kotlin port of every `serde` struct.
 */
object UsbInventory {

    /** Build a USB/IP-style busid for an [UsbDevice]. */
    fun busidFor(d: UsbDevice): String {
        // UsbDevice.deviceName looks like "/dev/bus/usb/001/002". We turn
        // that into "1-2" which matches what the receivers' clients expect.
        val parts = d.deviceName.split('/').takeLast(2)
        return if (parts.size == 2) {
            "${parts[0].toInt()}-${parts[1].toInt()}"
        } else {
            d.deviceName
        }
    }

    fun summarize(d: UsbDevice): JSONObject {
        val isHid = (0 until d.interfaceCount).any {
            d.getInterface(it).interfaceClass == 0x03 /* HID */
        }
        return JSONObject().apply {
            put("busid", busidFor(d))
            put("vendor_id", d.vendorId)
            put("product_id", d.productId)
            put("manufacturer", d.manufacturerName)
            put("product", d.productName)
            put("serial", null as String?)        // requires USB permission; deferred
            put("device_class", d.deviceClass)
            put("device_subclass", d.deviceSubclass)
            put("device_protocol", d.deviceProtocol)
            put("is_hid", isHid)
        }
    }

    fun deviceListMessage(devices: Collection<UsbDevice>): JSONObject {
        val arr = JSONArray()
        for (d in devices) arr.put(summarize(d))
        return JSONObject().apply {
            put("op", "DEVICE_LIST")
            put("devices", arr)
        }
    }
}
