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

// ---------------- kernel USB/IP submit/return headers ----------------
//
// The wire format below is what flows inside `UsbipFrame.raw` once a device
// is attached. Both the Linux kernel's `vhci-hcd` driver (on the receiver
// side) and the Android userspace URB pump (on the sender side) speak this
// format verbatim; the Rust fixture `simulate-android-pump` does too. See
// <https://docs.kernel.org/usb/usbip_protocol.html>.
//
// All fields are u32/u64 **big-endian** on the wire. Buffer payloads
// (transfer_buffer / iso_packet_descriptor) follow the 48-byte header for
// `CMD_SUBMIT` (OUT) and `RET_SUBMIT` (IN); `UNLINK` headers have no body.

/// Fixed kernel USB/IP header size (CMD_SUBMIT / RET_SUBMIT / CMD_UNLINK / RET_UNLINK).
pub const URB_HEADER_LEN: usize = 48;

/// Direction bit in `CMD_SUBMIT`: 0 = host→device (OUT), 1 = device→host (IN).
pub const URB_DIR_OUT: u32 = 0;
pub const URB_DIR_IN: u32 = 1;

/// `transfer_flags` bits we care about. Most other URB flags are kernel-private
/// (NO_INTERRUPT, SETUP_MAP_*, etc.) and don't affect what userspace must do.
pub const URB_SHORT_NOT_OK: u32 = 0x0000_0001;
pub const URB_ZERO_PACKET: u32 = 0x0000_0040;
pub const URB_NO_TRANSFER_DMA_MAP: u32 = 0x0000_0004;

/// Endpoint type, as used internally for dispatch. Not on the wire — the
/// kernel header doesn't carry endpoint type, the pump must learn it from
/// the device's interface descriptors.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EndpointType {
    Control = 0,
    Isochronous = 1,
    Bulk = 2,
    Interrupt = 3,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum UrbError {
    #[error("urb header too short: {0} < 48")]
    Short(usize),
    #[error("unknown urb command: {0:#010x}")]
    UnknownCommand(u32),
}

/// USBIP_CMD_SUBMIT — receiver → sender. 48-byte header.
///
/// For OUT transfers, the transfer buffer of size `transfer_buffer_length`
/// follows the header. Iso packets follow the body if any. The `setup`
/// field (8 bytes) carries the USB setup packet for control transfers,
/// `0` for everything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdSubmit {
    pub seqnum: u32,
    pub devid: u32,
    pub direction: u32,
    pub ep: u32,
    pub transfer_flags: u32,
    pub transfer_buffer_length: u32,
    pub start_frame: u32,
    pub number_of_packets: u32,
    pub interval: u32,
    /// USB setup packet (8 bytes), all-zero when not a control xfer.
    pub setup: [u8; 8],
}

impl CmdSubmit {
    pub fn parse(hdr: &[u8]) -> Result<Self, UrbError> {
        if hdr.len() < URB_HEADER_LEN {
            return Err(UrbError::Short(hdr.len()));
        }
        let command = read_u32(hdr, 0);
        if command != UrbOp::CmdSubmit as u32 {
            return Err(UrbError::UnknownCommand(command));
        }
        let mut setup = [0u8; 8];
        setup.copy_from_slice(&hdr[40..48]);
        Ok(Self {
            seqnum: read_u32(hdr, 4),
            devid: read_u32(hdr, 8),
            direction: read_u32(hdr, 12),
            ep: read_u32(hdr, 16),
            transfer_flags: read_u32(hdr, 20),
            transfer_buffer_length: read_u32(hdr, 24),
            start_frame: read_u32(hdr, 28),
            number_of_packets: read_u32(hdr, 32),
            interval: read_u32(hdr, 36),
            setup,
        })
    }

    pub fn encode(&self) -> [u8; URB_HEADER_LEN] {
        let mut out = [0u8; URB_HEADER_LEN];
        write_u32(&mut out, 0, UrbOp::CmdSubmit as u32);
        write_u32(&mut out, 4, self.seqnum);
        write_u32(&mut out, 8, self.devid);
        write_u32(&mut out, 12, self.direction);
        write_u32(&mut out, 16, self.ep);
        write_u32(&mut out, 20, self.transfer_flags);
        write_u32(&mut out, 24, self.transfer_buffer_length);
        write_u32(&mut out, 28, self.start_frame);
        write_u32(&mut out, 32, self.number_of_packets);
        write_u32(&mut out, 36, self.interval);
        out[40..48].copy_from_slice(&self.setup);
        out
    }

    /// Convenience: true when the setup packet looks like a real one
    /// (bmRequestType != 0 || bRequest != 0). `ep == 0` is the canonical
    /// signal that this is a control transfer.
    pub fn is_control(&self) -> bool {
        self.ep == 0
    }
}

/// USBIP_RET_SUBMIT — sender → receiver. 48-byte header.
///
/// For IN transfers, the bytes the device returned follow the header
/// (`actual_length` of them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetSubmit {
    pub seqnum: u32,
    /// Kernel ignores; pumps echo 0.
    pub devid: u32,
    pub direction: u32,
    pub ep: u32,
    /// 0 on success, negated `errno` on failure (e.g. -ETIMEDOUT == -110).
    pub status: i32,
    pub actual_length: u32,
    pub start_frame: u32,
    pub number_of_packets: u32,
    pub error_count: u32,
}

impl RetSubmit {
    pub fn parse(hdr: &[u8]) -> Result<Self, UrbError> {
        if hdr.len() < URB_HEADER_LEN {
            return Err(UrbError::Short(hdr.len()));
        }
        let command = read_u32(hdr, 0);
        if command != UrbOp::RetSubmit as u32 {
            return Err(UrbError::UnknownCommand(command));
        }
        Ok(Self {
            seqnum: read_u32(hdr, 4),
            devid: read_u32(hdr, 8),
            direction: read_u32(hdr, 12),
            ep: read_u32(hdr, 16),
            status: read_u32(hdr, 20) as i32,
            actual_length: read_u32(hdr, 24),
            start_frame: read_u32(hdr, 28),
            number_of_packets: read_u32(hdr, 32),
            error_count: read_u32(hdr, 36),
        })
    }

    pub fn encode(&self) -> [u8; URB_HEADER_LEN] {
        let mut out = [0u8; URB_HEADER_LEN];
        write_u32(&mut out, 0, UrbOp::RetSubmit as u32);
        write_u32(&mut out, 4, self.seqnum);
        write_u32(&mut out, 8, self.devid);
        write_u32(&mut out, 12, self.direction);
        write_u32(&mut out, 16, self.ep);
        write_u32(&mut out, 20, self.status as u32);
        write_u32(&mut out, 24, self.actual_length);
        write_u32(&mut out, 28, self.start_frame);
        write_u32(&mut out, 32, self.number_of_packets);
        write_u32(&mut out, 36, self.error_count);
        out
    }
}

/// USBIP_CMD_UNLINK — receiver → sender. 48-byte header, no body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdUnlink {
    pub seqnum: u32,
    pub devid: u32,
    pub direction: u32,
    pub ep: u32,
    /// Sequence number of the in-flight `CMD_SUBMIT` to cancel.
    pub unlink_seqnum: u32,
}

impl CmdUnlink {
    pub fn parse(hdr: &[u8]) -> Result<Self, UrbError> {
        if hdr.len() < URB_HEADER_LEN {
            return Err(UrbError::Short(hdr.len()));
        }
        let command = read_u32(hdr, 0);
        if command != UrbOp::CmdUnlink as u32 {
            return Err(UrbError::UnknownCommand(command));
        }
        Ok(Self {
            seqnum: read_u32(hdr, 4),
            devid: read_u32(hdr, 8),
            direction: read_u32(hdr, 12),
            ep: read_u32(hdr, 16),
            unlink_seqnum: read_u32(hdr, 20),
        })
    }

    pub fn encode(&self) -> [u8; URB_HEADER_LEN] {
        let mut out = [0u8; URB_HEADER_LEN];
        write_u32(&mut out, 0, UrbOp::CmdUnlink as u32);
        write_u32(&mut out, 4, self.seqnum);
        write_u32(&mut out, 8, self.devid);
        write_u32(&mut out, 12, self.direction);
        write_u32(&mut out, 16, self.ep);
        write_u32(&mut out, 20, self.unlink_seqnum);
        out
    }
}

/// USBIP_RET_UNLINK — sender → receiver. 48-byte header, no body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetUnlink {
    pub seqnum: u32,
    pub devid: u32,
    pub direction: u32,
    pub ep: u32,
    /// 0 on success, -ECONNRESET (-104) if the URB was already completed
    /// when the unlink arrived, etc.
    pub status: i32,
}

impl RetUnlink {
    pub fn parse(hdr: &[u8]) -> Result<Self, UrbError> {
        if hdr.len() < URB_HEADER_LEN {
            return Err(UrbError::Short(hdr.len()));
        }
        let command = read_u32(hdr, 0);
        if command != UrbOp::RetUnlink as u32 {
            return Err(UrbError::UnknownCommand(command));
        }
        Ok(Self {
            seqnum: read_u32(hdr, 4),
            devid: read_u32(hdr, 8),
            direction: read_u32(hdr, 12),
            ep: read_u32(hdr, 16),
            status: read_u32(hdr, 20) as i32,
        })
    }

    pub fn encode(&self) -> [u8; URB_HEADER_LEN] {
        let mut out = [0u8; URB_HEADER_LEN];
        write_u32(&mut out, 0, UrbOp::RetUnlink as u32);
        write_u32(&mut out, 4, self.seqnum);
        write_u32(&mut out, 8, self.devid);
        write_u32(&mut out, 12, self.direction);
        write_u32(&mut out, 16, self.ep);
        write_u32(&mut out, 20, self.status as u32);
        out
    }
}

/// Peek at the 4-byte command field at the start of a 48-byte URB header.
/// Returns `None` if the buffer is shorter than 4 bytes or the command is
/// not one of the four `UrbOp` values.
pub fn peek_urb_op(hdr: &[u8]) -> Option<UrbOp> {
    if hdr.len() < 4 {
        return None;
    }
    UrbOp::from_u32(read_u32(hdr, 0))
}

#[inline]
fn read_u32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}
#[inline]
fn write_u32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_be_bytes());
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

    #[test]
    fn cmd_submit_round_trip() {
        let cmd = CmdSubmit {
            seqnum: 0x0102_0304,
            devid: 0x0001_0002,
            direction: URB_DIR_IN,
            ep: 1,
            transfer_flags: URB_SHORT_NOT_OK,
            transfer_buffer_length: 64,
            start_frame: 0,
            number_of_packets: 0,
            interval: 0,
            setup: [0; 8],
        };
        let bytes = cmd.encode();
        assert_eq!(bytes.len(), URB_HEADER_LEN);
        // command field at offset 0 should be 0x00000001 in big-endian.
        assert_eq!(&bytes[..4], &[0, 0, 0, 1]);
        let back = CmdSubmit::parse(&bytes).unwrap();
        assert_eq!(back, cmd);
    }

    #[test]
    fn cmd_submit_control_setup() {
        // GET_DESCRIPTOR(Device) request: bmRequestType=0x80, bRequest=0x06,
        // wValue=0x0100, wIndex=0, wLength=18.
        let setup = [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x12, 0x00];
        let cmd = CmdSubmit {
            seqnum: 7,
            devid: 0,
            direction: URB_DIR_IN,
            ep: 0,
            transfer_flags: 0,
            transfer_buffer_length: 18,
            start_frame: 0,
            number_of_packets: 0,
            interval: 0,
            setup,
        };
        assert!(cmd.is_control());
        let back = CmdSubmit::parse(&cmd.encode()).unwrap();
        assert_eq!(back.setup, setup);
    }

    #[test]
    fn ret_submit_round_trip() {
        let r = RetSubmit {
            seqnum: 7,
            devid: 0,
            direction: URB_DIR_IN,
            ep: 1,
            status: 0,
            actual_length: 64,
            start_frame: 0,
            number_of_packets: 0,
            error_count: 0,
        };
        let bytes = r.encode();
        assert_eq!(&bytes[..4], &[0, 0, 0, 3]);
        let back = RetSubmit::parse(&bytes).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn ret_submit_negative_status() {
        let r = RetSubmit {
            seqnum: 7,
            devid: 0,
            direction: URB_DIR_IN,
            ep: 1,
            status: -110, // -ETIMEDOUT
            actual_length: 0,
            start_frame: 0,
            number_of_packets: 0,
            error_count: 0,
        };
        let back = RetSubmit::parse(&r.encode()).unwrap();
        assert_eq!(back.status, -110);
    }

    #[test]
    fn cmd_unlink_round_trip() {
        let u = CmdUnlink {
            seqnum: 9,
            devid: 0x10002,
            direction: URB_DIR_OUT,
            ep: 0,
            unlink_seqnum: 5,
        };
        let bytes = u.encode();
        assert_eq!(&bytes[..4], &[0, 0, 0, 2]);
        assert_eq!(CmdUnlink::parse(&bytes).unwrap(), u);
    }

    #[test]
    fn ret_unlink_round_trip() {
        let u = RetUnlink {
            seqnum: 9,
            devid: 0x10002,
            direction: 0,
            ep: 0,
            status: -104, // -ECONNRESET (canonical "already done")
        };
        let bytes = u.encode();
        assert_eq!(&bytes[..4], &[0, 0, 0, 4]);
        assert_eq!(RetUnlink::parse(&bytes).unwrap(), u);
    }

    #[test]
    fn peek_urb_op_dispatches() {
        assert_eq!(
            peek_urb_op(
                &CmdSubmit {
                    seqnum: 0,
                    devid: 0,
                    direction: 0,
                    ep: 0,
                    transfer_flags: 0,
                    transfer_buffer_length: 0,
                    start_frame: 0,
                    number_of_packets: 0,
                    interval: 0,
                    setup: [0; 8],
                }
                .encode()
            ),
            Some(UrbOp::CmdSubmit),
        );
        assert_eq!(
            peek_urb_op(
                &CmdUnlink {
                    seqnum: 0,
                    devid: 0,
                    direction: 0,
                    ep: 0,
                    unlink_seqnum: 0,
                }
                .encode()
            ),
            Some(UrbOp::CmdUnlink),
        );
        assert!(peek_urb_op(&[0, 0, 0]).is_none());
        assert!(peek_urb_op(&[0xde, 0xad, 0xbe, 0xef]).is_none());
    }

    #[test]
    fn parse_rejects_wrong_command() {
        // A RET_SUBMIT byte sequence handed to CmdSubmit::parse.
        let r = RetSubmit {
            seqnum: 1,
            devid: 0,
            direction: 0,
            ep: 0,
            status: 0,
            actual_length: 0,
            start_frame: 0,
            number_of_packets: 0,
            error_count: 0,
        };
        let bytes = r.encode();
        assert!(matches!(
            CmdSubmit::parse(&bytes),
            Err(UrbError::UnknownCommand(0x00000003)),
        ));
    }

    #[test]
    fn cmd_submit_rejects_short() {
        assert!(matches!(
            CmdSubmit::parse(&[0u8; 10]),
            Err(UrbError::Short(10)),
        ));
    }
}
