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

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const HID_HEADER_LEN: usize = 4;

/// Coarse classification of an exposed HID device. The sender fills this in
/// on the `BindAck` JSON sidecar so the receiver can pick the right uinput
/// device layout without having to parse the full HID report descriptor.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    Mouse,
    Keyboard,
    Gamepad,
    Other,
}

/// JSON payload the receiver sends as the body of a `HidOp::Bind` frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindRequest {
    pub busid: String,
    /// `"input"` for now (we don't yet do feature/output binds).
    pub want: String,
}

/// JSON payload the sender prepends to the report descriptor in a
/// `HidOp::BindAck` frame. Layout on the wire:
///
/// ```text
/// bind_ack body = [ u16 BE: meta_len ] [ meta_len bytes: BindAckMeta JSON ] [ report descriptor bytes ]
/// ```
///
/// This is a non-breaking extension of v1: the existing tests treat the
/// whole body as opaque report descriptor bytes, and any receiver that
/// doesn't know about the meta-prefix can keep doing that, since we use
/// `parse_bind_ack` for the structured view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindAckMeta {
    pub busid: String,
    pub kind: DeviceKind,
    pub vendor_id: u16,
    pub product_id: u16,
    pub name: String,
}

/// Build a `BindAck` body with the meta sidecar in front of the report descriptor.
pub fn encode_bind_ack_body(meta: &BindAckMeta, report_descriptor: &[u8]) -> Vec<u8> {
    let meta_json = serde_json::to_vec(meta).expect("BindAckMeta serializes");
    let mut out = Vec::with_capacity(2 + meta_json.len() + report_descriptor.len());
    out.extend_from_slice(&(meta_json.len() as u16).to_be_bytes());
    out.extend_from_slice(&meta_json);
    out.extend_from_slice(report_descriptor);
    out
}

/// Split a `BindAck` body back into (meta, report descriptor).
pub fn parse_bind_ack_body(body: &[u8]) -> Result<(BindAckMeta, &[u8]), HidError> {
    if body.len() < 2 {
        return Err(HidError::Short(body.len()));
    }
    let meta_len = u16::from_be_bytes([body[0], body[1]]) as usize;
    if body.len() < 2 + meta_len {
        return Err(HidError::Short(body.len()));
    }
    let meta: BindAckMeta = serde_json::from_slice(&body[2..2 + meta_len])
        .map_err(|_| HidError::BadMeta)?;
    Ok((meta, &body[2 + meta_len..]))
}

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
    #[error("bind_ack meta is not valid JSON")]
    BadMeta,
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

    #[test]
    fn bind_ack_body_round_trip() {
        let meta = BindAckMeta {
            busid: "1-2".into(),
            kind: DeviceKind::Mouse,
            vendor_id: 0x046d,
            product_id: 0xc52b,
            name: "Logitech Unifying Mouse".into(),
        };
        let rd: &[u8] = &[0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0xC0];
        let body = encode_bind_ack_body(&meta, rd);
        let (got_meta, got_rd) = parse_bind_ack_body(&body).unwrap();
        assert_eq!(got_meta, meta);
        assert_eq!(got_rd, rd);
    }

    #[test]
    fn bind_request_json_round_trip() {
        let req = BindRequest { busid: "1-2".into(), want: "input".into() };
        let json = serde_json::to_vec(&req).unwrap();
        let back: BindRequest = serde_json::from_slice(&json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn full_bind_ack_frame_round_trip() {
        let meta = BindAckMeta {
            busid: "1-2".into(),
            kind: DeviceKind::Keyboard,
            vendor_id: 0x05ac,
            product_id: 0x024f,
            name: "Apple Magic Keyboard".into(),
        };
        let rd: &[u8] = &[0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0xC0];
        let body = encode_bind_ack_body(&meta, rd);
        let frame = HidFrame::new(HidOp::BindAck, 3, 0, &body);
        let bytes = frame.encode();
        let parsed = HidFrame::parse(&bytes).unwrap();
        assert_eq!(parsed.op, HidOp::BindAck);
        assert_eq!(parsed.bind_id, 3);
        let (got_meta, got_rd) = parse_bind_ack_body(parsed.body).unwrap();
        assert_eq!(got_meta, meta);
        assert_eq!(got_rd, rd);
    }

    #[test]
    fn parse_bind_ack_rejects_garbage() {
        // claims meta_len = 5 but only 2 bytes follow
        let bad: [u8; 4] = [0x00, 0x05, b'{', b'}'];
        assert_eq!(parse_bind_ack_body(&bad), Err(HidError::Short(4)));
        // valid length but body is not JSON
        let bad2: Vec<u8> = vec![0x00, 0x03, b'n', b'o', b'!'];
        assert_eq!(parse_bind_ack_body(&bad2), Err(HidError::BadMeta));
    }
}
