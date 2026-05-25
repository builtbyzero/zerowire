//! USB/IP opcodes — kernel.org spec.
//!
//! See <https://docs.kernel.org/usb/usbip_protocol.html>.
//!
//! We relay the on-wire bytes verbatim from the receiver's USB/IP stack
//! (vhci-hcd on Linux, usbip-win2 on Windows, our DriverKit sysext on macOS)
//! straight to the Android sender's userspace USB/IP implementation. The
//! only thing zerowire layers on top is:
//!
//! * a 4-byte `import_id` prefix inside the envelope payload, so we can
//!   multiplex multiple devices on one socket (`UsbipFrame`), and
//! * an optional 1-byte unlink reason trailer (`UnlinkReason`).

use thiserror::Error;

/// USB/IP "op" codes from the spec (the "command code" field of the header,
/// big-endian on the wire).
///
/// These cover the framing of the high-level requests; the per-URB submit /
/// return / unlink codes live in `UrbOp`.
#[repr(u16)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Receiver asks the server for its device list.
    OpReqDevlist = 0x8005,
    /// Server replies with its device list.
    OpRepDevlist = 0x0005,
    /// Receiver claims a device by busid.
    OpReqImport = 0x8003,
    /// Server confirms (or denies) the import.
    OpRepImport = 0x0003,
}

impl Op {
    pub fn from_u16(v: u16) -> Option<Self> {
        Some(match v {
            0x8005 => Op::OpReqDevlist,
            0x0005 => Op::OpRepDevlist,
            0x8003 => Op::OpReqImport,
            0x0003 => Op::OpRepImport,
            _ => return None,
        })
    }
}

/// Per-URB op codes (the `command` field, u32 big-endian).
#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UrbOp {
    /// Receiver submits a USB Request Block.
    CmdSubmit = 0x00000001,
    /// Receiver asks to cancel an outstanding URB.
    CmdUnlink = 0x00000002,
    /// Server completes a URB.
    RetSubmit = 0x00000003,
    /// Server acks an unlink.
    RetUnlink = 0x00000004,
}

impl UrbOp {
    pub fn from_u32(v: u32) -> Option<Self> {
        Some(match v {
            0x00000001 => UrbOp::CmdSubmit,
            0x00000002 => UrbOp::CmdUnlink,
            0x00000003 => UrbOp::RetSubmit,
            0x00000004 => UrbOp::RetUnlink,
            _ => return None,
        })
    }
}

/// zerowire-specific reason byte for unlinks.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UnlinkReason {
    Unspecified = 0x00,
    Timeout = 0x01,
    DeviceUnplugged = 0x02,
    UserRevoked = 0x03,
    SenderShutdown = 0x04,
}

impl UnlinkReason {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0x01 => UnlinkReason::Timeout,
            0x02 => UnlinkReason::DeviceUnplugged,
            0x03 => UnlinkReason::UserRevoked,
            0x04 => UnlinkReason::SenderShutdown,
            _ => UnlinkReason::Unspecified,
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum UsbipError {
    #[error("frame too short for import_id ({0} < 4)")]
    Short(usize),
}

/// One USB/IP packet, demultiplexed.
///
/// The envelope-channel-0x02 payload starts with a 4-byte big-endian
/// `import_id` that names which attached device this packet belongs to, then
/// the raw USB/IP packet follows. This is the zerowire extension; the bytes
/// after `import_id` are kernel-compatible USB/IP.
#[derive(Debug, PartialEq, Eq)]
pub struct UsbipFrame<'a> {
    pub import_id: u32,
    pub raw: &'a [u8],
}

impl<'a> UsbipFrame<'a> {
    pub fn new(import_id: u32, raw: &'a [u8]) -> Self {
        Self { import_id, raw }
    }

    pub fn parse(payload: &'a [u8]) -> Result<Self, UsbipError> {
        if payload.len() < 4 {
            return Err(UsbipError::Short(payload.len()));
        }
        let import_id = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
        Ok(Self {
            import_id,
            raw: &payload[4..],
        })
    }

    pub fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.import_id.to_be_bytes());
        out.extend_from_slice(self.raw);
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(4 + self.raw.len());
        self.encode_into(&mut v);
        v
    }
}

// ---------------- v0.2 general-passthrough framing ----------------
//
// The HID fast lane multiplexes one device per session and synthesizes input
// events on the receiver side. For non-HID devices (mass storage, MIDI,
// printers, generic) we need to relay the full USB/IP packet stream so the
// receiver kernel's `vhci-hcd` driver can drive the device as if it were
// physically attached. The framing below is the minimum we need on the wire
// to:
//
//   * Tell the receiver which USB device is being attached (so it can pick
//     a free vhci-hcd port and an `import_id`),
//   * Carry opaque URBs in both directions.
//
// The descriptor blob is the raw `usb_device_descriptor` (18 bytes,
// little-endian) plus the speed byte (USB/IP convention: 1=low, 2=full,
// 3=high, 5=super). We don't try to enumerate config/interface descriptors
// here — vhci-hcd asks for them via SUBMIT/GET_DESCRIPTOR URBs once
// attached.

/// USB/IP speed code, per the kernel header `linux/usb/ch9.h`
/// (`usb_device_speed`). Used both in our `UsbipAttachInfo` and when writing
/// to `/sys/devices/platform/vhci_hcd.0/attach`.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UsbipSpeed {
    Unknown = 0,
    Low = 1,
    Full = 2,
    High = 3,
    Wireless = 4,
    Super = 5,
    SuperPlus = 6,
}

impl UsbipSpeed {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => UsbipSpeed::Low,
            2 => UsbipSpeed::Full,
            3 => UsbipSpeed::High,
            4 => UsbipSpeed::Wireless,
            5 => UsbipSpeed::Super,
            6 => UsbipSpeed::SuperPlus,
            _ => UsbipSpeed::Unknown,
        }
    }
}

/// On-wire info the sender pushes to the receiver immediately after
/// `ATTACH_OK { mode: "usbip" }`. The receiver uses these fields verbatim
/// when telling its kernel to take ownership of the TCP socket.
///
/// JSON over the control channel rather than binary because attach is a
/// once-per-device event; the volume is irrelevant and JSON is easier to
/// debug. Bulk URBs ride on `Channel::Usbip` as `UsbipFrame`s.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UsbipAttachInfo {
    /// USB/IP busid, e.g. `"1-2"`. Stable per physical port; the receiver
    /// uses this string when telling vhci-hcd to take the socket.
    pub busid: String,
    /// Sender-side per-session ID, multiplexed inside `UsbipFrame.import_id`.
    pub import_id: u32,
    /// vhci-hcd device id: `(busnum << 16) | devnum`, per the kernel
    /// sysfs interface.
    pub devid: u32,
    /// USB device speed, see [`UsbipSpeed`].
    pub speed: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    /// Hex-encoded raw `usb_device_descriptor` (18 bytes). Optional — the
    /// receiver doesn't actually need it for vhci-hcd attach, but it's
    /// useful for logging/sim and we already have it on the sender side.
    pub descriptor_hex: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_round_trips() {
        for op in [
            Op::OpReqDevlist,
            Op::OpRepDevlist,
            Op::OpReqImport,
            Op::OpRepImport,
        ] {
            assert_eq!(Op::from_u16(op as u16), Some(op));
        }
        assert_eq!(Op::from_u16(0xdead), None);
    }

    #[test]
    fn urb_op_round_trips() {
        for op in [
            UrbOp::CmdSubmit,
            UrbOp::CmdUnlink,
            UrbOp::RetSubmit,
            UrbOp::RetUnlink,
        ] {
            assert_eq!(UrbOp::from_u32(op as u32), Some(op));
        }
        assert_eq!(UrbOp::from_u32(0xdeadbeef), None);
    }

    #[test]
    fn usbip_frame_round_trip() {
        let raw = [0xAA, 0xBB, 0xCC, 0xDD];
        let f = UsbipFrame::new(0x1234_5678, &raw);
        let bytes = f.encode();
        assert_eq!(bytes[..4], [0x12, 0x34, 0x56, 0x78]);
        let back = UsbipFrame::parse(&bytes).unwrap();
        assert_eq!(back.import_id, 0x1234_5678);
        assert_eq!(back.raw, &raw);
    }

    #[test]
    fn usbip_frame_short() {
        assert!(matches!(
            UsbipFrame::parse(&[1, 2, 3]),
            Err(UsbipError::Short(3))
        ));
    }
}
