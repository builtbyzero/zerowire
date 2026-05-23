//! Blocking TCP helpers for sending/receiving zerowire envelopes.
//!
//! The CLI today is plain-text TCP. This is intentional for the v0.1 demo
//! and clearly documented in ARCHITECTURE.md §6.2. TLS + PSK proof land
//! once the bare wire works end-to-end.

use std::io::{Read, Write};
use std::net::TcpStream;

use anyhow::{anyhow, bail, Context, Result};
use zerowire_protocol::{
    control::ControlMessage,
    envelope::{Channel, Envelope, HEADER_LEN},
};

/// Write one envelope on `sock`.
pub fn send_envelope(sock: &mut TcpStream, channel: Channel, payload: &[u8]) -> Result<()> {
    let bytes = Envelope::new(channel, payload)
        .encode()
        .context("encoding envelope")?;
    sock.write_all(&bytes).context("writing envelope")?;
    Ok(())
}

/// Convenience: send a control message.
pub fn send_control(sock: &mut TcpStream, msg: &ControlMessage) -> Result<()> {
    let body = msg.to_json()?;
    send_envelope(sock, Channel::Control, &body)
}

/// A parsed envelope, fully owned (so we don't have lifetime issues with the
/// underlying buffer once the caller wants to keep it).
#[derive(Debug)]
pub struct OwnedEnvelope {
    pub channel: Channel,
    pub payload: Vec<u8>,
}

/// Block until one full envelope is read from `sock`.
pub fn recv_envelope(sock: &mut TcpStream) -> Result<OwnedEnvelope> {
    let mut header = [0u8; HEADER_LEN];
    sock.read_exact(&mut header)
        .context("reading envelope header")?;
    let len = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
    let mut full = Vec::with_capacity(HEADER_LEN + len);
    full.extend_from_slice(&header);
    full.resize(HEADER_LEN + len, 0);
    sock.read_exact(&mut full[HEADER_LEN..])
        .context("reading envelope payload")?;
    let (env, _) =
        Envelope::parse(&full)?.ok_or_else(|| anyhow!("incomplete envelope after read_exact"))?;
    Ok(OwnedEnvelope {
        channel: env.channel,
        payload: env.payload.to_vec(),
    })
}

/// Block until a control envelope is received. Errors if a non-control
/// envelope arrives first (we don't queue here; that's the receiver loop's job).
pub fn recv_control(sock: &mut TcpStream) -> Result<ControlMessage> {
    let env = recv_envelope(sock)?;
    match env.channel {
        Channel::Control => Ok(ControlMessage::from_json(&env.payload)?),
        other => bail!("expected Control envelope, got {:?}", other),
    }
}
