//! Control-channel messages — JSON, one per envelope.
//!
//! Control is the lowest-volume part of the protocol; we trade a few bytes
//! for debuggability and forward-compat. The bulk channels (USB/IP, HID)
//! stay binary.

use serde::{Deserialize, Serialize};

/// A summary of one USB device the sender currently has plugged in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSummary {
    /// Sender-side stable identifier (`<bus>-<port-chain>`), USB/IP-compatible.
    pub busid: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    /// `bDeviceClass` from the USB device descriptor.
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    /// True if at least one interface is HID. Receivers may prefer the HID
    /// fast lane for these.
    pub is_hid: bool,
}

/// All control-channel messages share a discriminator field `op` so they can
/// be deserialized polymorphically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum ControlMessage {
    /// Receiver → sender. First message after TLS handshake.
    #[serde(rename = "HELLO")]
    Hello {
        version: u8,
        client: String,
        supports: Vec<String>,
    },
    /// Sender → receiver.
    #[serde(rename = "HELLO_ACK")]
    HelloAck {
        sender_id: String,
        name: String,
        supports: Vec<String>,
    },
    /// Receiver → sender. PSK proof (HMAC-SHA256, base64).
    #[serde(rename = "AUTH")]
    Auth { proof_b64: String },
    /// Sender → receiver.
    #[serde(rename = "AUTH_OK")]
    AuthOk,
    /// Either direction. Hard-stop with a reason.
    #[serde(rename = "ERROR")]
    Error { code: String, message: String },
    /// Receiver → sender.
    #[serde(rename = "LIST_DEVICES")]
    ListDevices,
    /// Sender → receiver.
    #[serde(rename = "DEVICE_LIST")]
    DeviceList { devices: Vec<DeviceSummary> },
    /// Receiver → sender.
    #[serde(rename = "ATTACH")]
    Attach {
        busid: String,
        /// `"usbip"` or `"hid"`.
        mode: String,
    },
    /// Sender → receiver. Sender's per-session import_id for this device.
    #[serde(rename = "ATTACH_OK")]
    AttachOk { busid: String, import_id: u32 },
    /// Sender → receiver. The user denied or the device vanished.
    #[serde(rename = "ATTACH_DENIED")]
    AttachDenied { busid: String, reason: String },
}

impl ControlMessage {
    pub fn to_json(&self) -> serde_json::Result<Vec<u8>> {
        serde_json::to_vec(self)
    }
    pub fn from_json(bytes: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_round_trip() {
        let m = ControlMessage::Hello {
            version: 1,
            client: "linux-cli/0.1.0".into(),
            supports: vec!["usbip/1.1.1".into(), "hid-fastlane/1".into()],
        };
        let bytes = m.to_json().unwrap();
        let back = ControlMessage::from_json(&bytes).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn device_list_round_trip() {
        let m = ControlMessage::DeviceList {
            devices: vec![DeviceSummary {
                busid: "1-1.4".into(),
                vendor_id: 0x046d,
                product_id: 0xc52b,
                manufacturer: Some("Logitech".into()),
                product: Some("Unifying Receiver".into()),
                serial: None,
                device_class: 0x00,
                device_subclass: 0x00,
                device_protocol: 0x00,
                is_hid: true,
            }],
        };
        let bytes = m.to_json().unwrap();
        let back = ControlMessage::from_json(&bytes).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn unknown_op_errors_clean() {
        let raw = br#"{"op":"NOPE"}"#;
        assert!(ControlMessage::from_json(raw).is_err());
    }
}
