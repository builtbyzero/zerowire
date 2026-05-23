//! The zerowire envelope.
//!
//! Every byte on the wire after the TLS handshake is one of these frames.
//! It exists so we can multiplex USB/IP transfers and the HID fast lane on a
//! single TCP socket without inventing a new transport.
//!
//! ```text
//!  0               1               2               3
//!  +---------------+---------------+---------------+---------------+
//!  | magic 'Z' 'W' | version (u8)  | channel (u8)                  |
//!  +---------------+---------------+---------------+---------------+
//!  | length (u32 big-endian, payload bytes)                         |
//!  +---------------------------------------------------------------+
//!  | payload ...                                                    |
//!  +---------------------------------------------------------------+
//! ```

use thiserror::Error;

/// Two-byte magic: ASCII `Z` `W`.
pub const MAGIC: [u8; 2] = [b'Z', b'W'];

/// Current protocol version. Bumped on breaking changes only.
pub const PROTOCOL_VERSION: u8 = 1;

/// Fixed envelope header size on the wire.
pub const HEADER_LEN: usize = 8;

/// Conservative cap on a single envelope payload. USB/IP URBs are bounded by
/// `wMaxPacketSize` * `bNumPackets` so 1 MiB is comfortable headroom. We
/// enforce it so a hostile peer can't ask us to allocate gigabytes.
pub const MAX_PAYLOAD: usize = 1 << 20;

/// Logical channels multiplexed over the envelope.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Channel {
    /// Handshake, capability negotiation, device list, pairing — JSON.
    Control = 0x01,
    /// USB/IP protocol packets (with a 4-byte `import_id` prefix). See `usbip`.
    Usbip = 0x02,
    /// HID fast lane (mouse/keyboard/gamepad input). See `hid`.
    Hid = 0x03,
    /// Empty payload, every 5s idle.
    Keepalive = 0x7F,
}

impl Channel {
    pub fn from_u8(v: u8) -> Result<Self, EnvelopeError> {
        match v {
            0x01 => Ok(Channel::Control),
            0x02 => Ok(Channel::Usbip),
            0x03 => Ok(Channel::Hid),
            0x7F => Ok(Channel::Keepalive),
            other => Err(EnvelopeError::UnknownChannel(other)),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EnvelopeError {
    #[error("buffer too short for an envelope header ({0} < {HEADER_LEN})")]
    Short(usize),
    #[error("bad magic: {0:?}")]
    BadMagic([u8; 2]),
    #[error("unsupported protocol version: {0}")]
    BadVersion(u8),
    #[error("unknown channel byte: 0x{0:02x}")]
    UnknownChannel(u8),
    #[error("payload too large: {0} > {MAX_PAYLOAD}")]
    TooLarge(usize),
}

/// A parsed envelope. We borrow the payload to avoid copies on the hot path.
#[derive(Debug, PartialEq, Eq)]
pub struct Envelope<'a> {
    pub channel: Channel,
    pub payload: &'a [u8],
}

impl<'a> Envelope<'a> {
    pub fn new(channel: Channel, payload: &'a [u8]) -> Self {
        Self { channel, payload }
    }

    /// Encode this envelope into `out`. Returns the number of bytes written.
    pub fn encode_into(&self, out: &mut Vec<u8>) -> Result<usize, EnvelopeError> {
        if self.payload.len() > MAX_PAYLOAD {
            return Err(EnvelopeError::TooLarge(self.payload.len()));
        }
        let start = out.len();
        out.extend_from_slice(&MAGIC);
        out.push(PROTOCOL_VERSION);
        out.push(self.channel as u8);
        out.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());
        out.extend_from_slice(self.payload);
        Ok(out.len() - start)
    }

    /// Convenience: encode to a fresh `Vec`.
    pub fn encode(&self) -> Result<Vec<u8>, EnvelopeError> {
        let mut buf = Vec::with_capacity(HEADER_LEN + self.payload.len());
        self.encode_into(&mut buf)?;
        Ok(buf)
    }

    /// Try to parse one envelope out of `buf`.
    ///
    /// Returns `Ok(Some((env, consumed)))` on success, `Ok(None)` if the
    /// buffer doesn't yet hold a full frame, or `Err` on a hard protocol
    /// error (the caller should close the connection).
    pub fn parse(buf: &'a [u8]) -> Result<Option<(Envelope<'a>, usize)>, EnvelopeError> {
        if buf.len() < HEADER_LEN {
            return Ok(None);
        }
        if buf[0..2] != MAGIC {
            return Err(EnvelopeError::BadMagic([buf[0], buf[1]]));
        }
        if buf[2] != PROTOCOL_VERSION {
            return Err(EnvelopeError::BadVersion(buf[2]));
        }
        let channel = Channel::from_u8(buf[3])?;
        let len = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
        if len > MAX_PAYLOAD {
            return Err(EnvelopeError::TooLarge(len));
        }
        let total = HEADER_LEN + len;
        if buf.len() < total {
            return Ok(None);
        }
        let payload = &buf[HEADER_LEN..total];
        Ok(Some((Envelope { channel, payload }, total)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_control_envelope() {
        let payload = b"{\"op\":\"HELLO\"}";
        let env = Envelope::new(Channel::Control, payload);
        let bytes = env.encode().unwrap();
        assert_eq!(&bytes[0..2], &MAGIC);
        assert_eq!(bytes[2], PROTOCOL_VERSION);
        assert_eq!(bytes[3], Channel::Control as u8);

        let (parsed, n) = Envelope::parse(&bytes).unwrap().unwrap();
        assert_eq!(n, bytes.len());
        assert_eq!(parsed.channel, Channel::Control);
        assert_eq!(parsed.payload, payload);
    }

    #[test]
    fn partial_buffer_returns_none() {
        let env = Envelope::new(Channel::Hid, &[0u8; 32]);
        let bytes = env.encode().unwrap();
        // Truncate to less than the full frame.
        assert!(Envelope::parse(&bytes[..HEADER_LEN + 10]).unwrap().is_none());
        // And less than the header.
        assert!(Envelope::parse(&bytes[..3]).unwrap().is_none());
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = Envelope::new(Channel::Control, b"x").encode().unwrap();
        bytes[0] = b'X';
        assert!(matches!(
            Envelope::parse(&bytes),
            Err(EnvelopeError::BadMagic(_))
        ));
    }

    #[test]
    fn rejects_bad_version() {
        let mut bytes = Envelope::new(Channel::Control, b"x").encode().unwrap();
        bytes[2] = 99;
        assert!(matches!(
            Envelope::parse(&bytes),
            Err(EnvelopeError::BadVersion(99))
        ));
    }

    #[test]
    fn rejects_unknown_channel() {
        let mut bytes = Envelope::new(Channel::Control, b"x").encode().unwrap();
        bytes[3] = 0x55;
        assert!(matches!(
            Envelope::parse(&bytes),
            Err(EnvelopeError::UnknownChannel(0x55))
        ));
    }

    #[test]
    fn rejects_oversize_declared_length() {
        // Hand-craft a header that claims a huge payload.
        let mut bytes = vec![b'Z', b'W', PROTOCOL_VERSION, Channel::Usbip as u8];
        bytes.extend_from_slice(&((MAX_PAYLOAD as u32 + 1).to_be_bytes()));
        assert!(matches!(
            Envelope::parse(&bytes),
            Err(EnvelopeError::TooLarge(_))
        ));
    }

    #[test]
    fn parse_handles_back_to_back_envelopes() {
        let a = Envelope::new(Channel::Control, b"a").encode().unwrap();
        let b = Envelope::new(Channel::Hid, b"bb").encode().unwrap();
        let mut both = a.clone();
        both.extend(b.iter());
        let (env1, n1) = Envelope::parse(&both).unwrap().unwrap();
        assert_eq!(env1.channel, Channel::Control);
        assert_eq!(env1.payload, b"a");
        let (env2, n2) = Envelope::parse(&both[n1..]).unwrap().unwrap();
        assert_eq!(env2.channel, Channel::Hid);
        assert_eq!(env2.payload, b"bb");
        assert_eq!(n1 + n2, both.len());
    }
}
