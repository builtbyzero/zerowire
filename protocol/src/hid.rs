//! HID fast lane — the sub-protocol on envelope channel `0x03`.
//!
//! ```text
//!  0               1               2               3
//!  +---------------+---------------+---------------+---------------+
//!  | hid_op (u8)   | bind_id (u8)  | seq (u16 BE)                  |
//!  +---------------+---------------+---------------+---------------+
//!  | payload ...                                                    |
//!  +---------------------------------------------------------------+
//! ```
//!
//! `bind_id` names a specific (device, receiver) binding established by an
//! earlier `BIND_ACK`. `seq` is a per-binding sender-side sequence number
//! used to detect drops and (optionally) smooth pointer jitter on RTT spikes.

use thiserror::Error;

pub const HID_HEADER_LEN: usize = 4;

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HidOp {
    /// Receiver asks sender to bind an exposed HID device for low-latency input.
    /// Payload: JSON `{ "busid": "...", "want": "input" }`.
    Bind = 0x01,
    /// Sender acknowledges with the chosen bind_id and the HID report descriptor.
    /// Payload: `bind_id` was placed in the header; this body is the raw report
    /// descriptor bytes.
    BindAck = 0x02,
    /// Sender pushes a raw HID input report to receiver.
    ReportIn = 0x10,
    /// Receiver pushes a raw HID output report to sender (e.g. caps-lock LED).
    ReportOut = 0x11,
    /// Get/set feature report. The direction is implied by which side sent it.
    ReportFeature = 0x12,
    /// Tear down a binding. Payload: JSON `{ "reason": "..." }`.
    Unbind = 0x20,
}

impl HidOp {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0x01 => HidOp::Bind,
            0x02 => HidOp::BindAck,
            0x10 => HidOp::ReportIn,
            0x11 => HidOp::ReportOut,
            0x12 => HidOp::ReportFeature,
            0x20 => HidOp::Unbind,
            _ => return None,
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HidError {
    #[error("hid frame too short ({0} < {HID_HEADER_LEN})")]
    Short(usize),
    #[error("unknown hid op: 0x{0:02x}")]
    UnknownOp(u8),
}

#[derive(Debug, PartialEq, Eq)]
pub struct HidFrame<'a> {
    pub op: HidOp,
    pub bind_id: u8,
    pub seq: u16,
    pub body: &'a [u8],
}

impl<'a> HidFrame<'a> {
    pub fn new(op: HidOp, bind_id: u8, seq: u16, body: &'a [u8]) -> Self {
        Self { op, bind_id, seq, body }
    }

    pub fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(self.op as u8);
        out.push(self.bind_id);
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(self.body);
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(HID_HEADER_LEN + self.body.len());
        self.encode_into(&mut v);
        v
    }

    pub fn parse(payload: &'a [u8]) -> Result<Self, HidError> {
        if payload.len() < HID_HEADER_LEN {
            return Err(HidError::Short(payload.len()));
        }
        let op = HidOp::from_u8(payload[0]).ok_or(HidError::UnknownOp(payload[0]))?;
        let bind_id = payload[1];
        let seq = u16::from_be_bytes([payload[2], payload[3]]);
        Ok(Self {
            op,
            bind_id,
            seq,
            body: &payload[HID_HEADER_LEN..],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_report_round_trip() {
        // A typical 4-byte boot-mouse report: buttons, dx, dy, wheel.
        let body = [0x01, 0x05, 0xFB, 0x00];
        let f = HidFrame::new(HidOp::ReportIn, 7, 12345, &body);
        let bytes = f.encode();
        assert_eq!(bytes[0], HidOp::ReportIn as u8);
        assert_eq!(bytes[1], 7);
        assert_eq!(&bytes[2..4], &12345u16.to_be_bytes());
        let back = HidFrame::parse(&bytes).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn bind_ack_carries_report_descriptor() {
        let descriptor = [0x05, 0x01, 0x09, 0x02, 0xA1, 0x01]; // partial mouse rd
        let f = HidFrame::new(HidOp::BindAck, 1, 0, &descriptor);
        let bytes = f.encode();
        let back = HidFrame::parse(&bytes).unwrap();
        assert_eq!(back.op, HidOp::BindAck);
        assert_eq!(back.body, &descriptor);
    }

    #[test]
    fn rejects_unknown_op() {
        let bytes = [0xEE, 0, 0, 0];
        assert!(matches!(
            HidFrame::parse(&bytes),
            Err(HidError::UnknownOp(0xEE))
        ));
    }

    #[test]
    fn rejects_short() {
        let bytes = [0x10, 0x00];
        assert!(matches!(HidFrame::parse(&bytes), Err(HidError::Short(2))));
    }
}
