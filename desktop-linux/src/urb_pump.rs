//! Userspace URB pump — fixture for the Android sender side.
//!
//! Mirrors `android-sender/.../UsbIpHost.kt` in pure Rust against a
//! `FakeDevice` instead of `UsbDeviceConnection`. The point is to exercise
//! the protocol crate's `CmdSubmit`/`RetSubmit`/`CmdUnlink`/`RetUnlink`
//! codec end-to-end against the receiver path, so the on-wire format the
//! Kotlin pump speaks is testable on a dev box that has no phone.
//!
//! The pump itself is the same shape as the Kotlin version:
//!
//! ```text
//!   loop {
//!     header = read 48 bytes
//!     match command {
//!       CMD_SUBMIT => if OUT: read transfer_buffer; dispatch to fake device;
//!                     write RET_SUBMIT (+ IN body)
//!       CMD_UNLINK => mark seqnum cancelled; write RET_UNLINK
//!       other      => bail
//!     }
//!   }
//! ```

use std::io::{Read, Write};

use anyhow::{anyhow, bail, Context, Result};

use zerowire_protocol::{
    envelope::Channel,
    usbip::{
        peek_urb_op, CmdSubmit, CmdUnlink, EndpointType, RetSubmit, RetUnlink, UrbOp, UsbipFrame,
        URB_DIR_IN, URB_DIR_OUT, URB_HEADER_LEN,
    },
};

use crate::tls as ztls;

// Negated errno values — must match what the kernel-side vhci-hcd expects
// (and what the Kotlin pump returns). Only the two below are exercised by
// the fake device; the Kotlin pump uses -ETIMEDOUT/-ECONNRESET for live
// transfer failures that the fake never hits.
const ENODEV: i32 = -19;
const EOPNOTSUPP: i32 = -95;

/// One endpoint advertised by the fake device. Direction is encoded in
/// `address` per USB convention: bit 7 set = IN, else OUT; lower 4 bits =
/// endpoint number.
#[derive(Debug, Clone)]
pub struct FakeEndpoint {
    pub address: u8,
    pub xfer_type: EndpointType,
}

impl FakeEndpoint {
    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }
}

/// Minimal "device" the pump dispatches against. Real Kotlin uses
/// `UsbDeviceConnection`; here we just pattern-match on the setup packet.
///
/// Built around a one-LUN fake mass-storage device: 18-byte device
/// descriptor + 32-byte config descriptor available via GET_DESCRIPTOR;
/// bulk IN on ep 0x81 returns deterministic 64-byte chunks; bulk OUT on
/// ep 0x02 swallows whatever it's given.
pub struct FakeDevice {
    pub device_descriptor: Vec<u8>,
    pub config_descriptor: Vec<u8>,
    pub endpoints: Vec<FakeEndpoint>,
}

impl FakeDevice {
    /// Build a "mass storage class-0x08" fake device with one bulk IN +
    /// one bulk OUT endpoint, matching what the v0.2 mock_sender advertises.
    pub fn mass_storage(vendor: u16, product: u16) -> Self {
        let device_descriptor =
            crate::usbip::synth_device_descriptor(vendor, product, 0x08).to_vec();
        Self {
            device_descriptor,
            // Minimal 32-byte "config + interface + 2 endpoints" descriptor.
            // The exact bytes don't matter for the test — the assertion is
            // round-trip length + the receiver successfully parsing it.
            config_descriptor: synth_config_descriptor(0x08, 0x06, 0x50),
            endpoints: vec![
                FakeEndpoint {
                    address: 0x81,
                    xfer_type: EndpointType::Bulk,
                },
                FakeEndpoint {
                    address: 0x02,
                    xfer_type: EndpointType::Bulk,
                },
            ],
        }
    }

    fn endpoint(&self, address: u8) -> Option<&FakeEndpoint> {
        self.endpoints.iter().find(|e| e.address == address)
    }
}

/// Outcome of a single `CMD_SUBMIT`. The pump turns this into the
/// matching `RET_SUBMIT`. Status is a (negated) errno; `data` is the IN
/// data payload (empty for OUT or non-data control xfers).
pub struct SubmitResult {
    pub status: i32,
    pub actual_length: u32,
    pub data: Vec<u8>,
}

/// Stats the pump records so callers (tests) can assert on what happened.
#[derive(Debug, Default, Clone)]
pub struct PumpStats {
    pub submits_seen: u32,
    pub submits_ok: u32,
    pub control_in_bytes: u32,
    pub bulk_in_bytes: u32,
    pub bulk_out_bytes: u32,
    pub unlinks_seen: u32,
    pub unsupported: u32,
}

/// Run the pump until EOF on `framed_in`, the caller-side close, or an
/// error. `framed_in` / `framed_out` must be the inbound / outbound halves
/// of the same logical TCP/TLS stream — typically a single
/// `StreamOwned<...>`, so the same reference goes both places via
/// generic `Read + Write` wrapping.
pub fn run_pump<S: Read + Write>(
    stream: &mut S,
    import_id: u32,
    device: &FakeDevice,
) -> Result<PumpStats> {
    let mut stats = PumpStats::default();
    loop {
        // Read the envelope; pump cares only about `Channel::Usbip`.
        let env = match ztls::recv_envelope(stream) {
            Ok(e) => e,
            Err(_) => break,
        };
        match env.channel {
            Channel::Usbip => {
                let frame = UsbipFrame::parse(&env.payload).context("parsing UsbipFrame")?;
                if frame.import_id != import_id {
                    bail!(
                        "frame import_id {:#x} != session import_id {:#x}",
                        frame.import_id,
                        import_id
                    );
                }
                handle_urb_packet(stream, import_id, device, frame.raw, &mut stats)?;
            }
            Channel::Control => {
                // Receiver may close cleanly with a control message; ignore.
            }
            Channel::Hid | Channel::Keepalive => {}
        }
    }
    Ok(stats)
}

/// One URB transaction: read header, optionally read OUT body, dispatch,
/// write RET frame.
fn handle_urb_packet<W: Write>(
    out: &mut W,
    import_id: u32,
    device: &FakeDevice,
    raw: &[u8],
    stats: &mut PumpStats,
) -> Result<()> {
    if raw.len() < URB_HEADER_LEN {
        bail!("URB packet too short: {} < {}", raw.len(), URB_HEADER_LEN);
    }
    let op = peek_urb_op(raw).ok_or_else(|| anyhow!("unknown URB op: {:02x?}", &raw[..4]))?;
    match op {
        UrbOp::CmdSubmit => {
            let cmd = CmdSubmit::parse(&raw[..URB_HEADER_LEN])?;
            stats.submits_seen += 1;
            let out_body = if cmd.direction == URB_DIR_OUT {
                let want = cmd.transfer_buffer_length as usize;
                if raw.len() < URB_HEADER_LEN + want {
                    bail!(
                        "CMD_SUBMIT OUT truncated: got {} bytes after header, need {}",
                        raw.len() - URB_HEADER_LEN,
                        want,
                    );
                }
                &raw[URB_HEADER_LEN..URB_HEADER_LEN + want]
            } else {
                &[][..]
            };
            let res = dispatch_submit(device, &cmd, out_body);
            if res.status == 0 {
                stats.submits_ok += 1;
                if cmd.is_control() && cmd.direction == URB_DIR_IN {
                    stats.control_in_bytes += res.actual_length;
                } else if cmd.direction == URB_DIR_IN {
                    stats.bulk_in_bytes += res.actual_length;
                } else {
                    stats.bulk_out_bytes += res.actual_length;
                }
            } else if res.status == EOPNOTSUPP {
                stats.unsupported += 1;
            }
            send_ret_submit(out, import_id, &cmd, &res)?;
        }
        UrbOp::CmdUnlink => {
            let u = CmdUnlink::parse(&raw[..URB_HEADER_LEN])?;
            stats.unlinks_seen += 1;
            send_ret_unlink(out, import_id, &u, 0)?;
        }
        UrbOp::RetSubmit | UrbOp::RetUnlink => {
            // We're the *sender* — we don't expect to see these inbound.
            bail!("unexpected inbound RET op {:?}", op);
        }
    }
    Ok(())
}

fn dispatch_submit(device: &FakeDevice, cmd: &CmdSubmit, out_body: &[u8]) -> SubmitResult {
    if cmd.is_control() {
        return execute_control(device, cmd, out_body);
    }
    let addr = (((cmd.direction == URB_DIR_IN) as u8) << 7) | (cmd.ep as u8 & 0x0F);
    let ep = match device.endpoint(addr) {
        Some(e) => e,
        None => {
            return SubmitResult {
                status: ENODEV,
                actual_length: 0,
                data: Vec::new(),
            }
        }
    };
    match ep.xfer_type {
        EndpointType::Bulk | EndpointType::Interrupt => execute_bulk(cmd, ep, out_body),
        EndpointType::Isochronous => SubmitResult {
            status: EOPNOTSUPP,
            actual_length: 0,
            data: Vec::new(),
        },
        EndpointType::Control => execute_control(device, cmd, out_body),
    }
}

fn execute_control(device: &FakeDevice, cmd: &CmdSubmit, out_body: &[u8]) -> SubmitResult {
    let setup = cmd.setup;
    let bm_request_type = setup[0];
    let b_request = setup[1];
    let w_value = u16::from_le_bytes([setup[2], setup[3]]);
    let _w_index = u16::from_le_bytes([setup[4], setup[5]]);
    let w_length = u16::from_le_bytes([setup[6], setup[7]]);
    let is_in = bm_request_type & 0x80 != 0;

    // GET_DESCRIPTOR (standard, device-to-host): bRequest=0x06, wValue
    // hi byte = descriptor type (0x01 device, 0x02 config).
    if is_in && b_request == 0x06 {
        let desc_type = (w_value >> 8) as u8;
        let buf: &[u8] = match desc_type {
            0x01 => &device.device_descriptor,
            0x02 => &device.config_descriptor,
            _ => {
                return SubmitResult {
                    status: EOPNOTSUPP,
                    actual_length: 0,
                    data: Vec::new(),
                }
            }
        };
        let take = (w_length as usize).min(buf.len());
        return SubmitResult {
            status: 0,
            actual_length: take as u32,
            data: buf[..take].to_vec(),
        };
    }

    // No-data control (SET_ADDRESS / SET_CONFIGURATION / etc.): ack with 0.
    if !is_in && cmd.transfer_buffer_length == 0 {
        return SubmitResult {
            status: 0,
            actual_length: 0,
            data: Vec::new(),
        };
    }

    // OUT control with data — just acknowledge however much we got.
    if !is_in {
        return SubmitResult {
            status: 0,
            actual_length: out_body.len() as u32,
            data: Vec::new(),
        };
    }

    // Anything else (vendor-specific INs) — report empty success.
    SubmitResult {
        status: 0,
        actual_length: 0,
        data: Vec::new(),
    }
}

fn execute_bulk(cmd: &CmdSubmit, ep: &FakeEndpoint, out_body: &[u8]) -> SubmitResult {
    if ep.is_in() {
        let len = cmd.transfer_buffer_length as usize;
        // Deterministic payload: byte i = (seqnum + i) mod 256. Lets the
        // receiver assert content as well as length.
        let mut buf = vec![0u8; len];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = ((cmd.seqnum as usize + i) & 0xFF) as u8;
        }
        SubmitResult {
            status: 0,
            actual_length: len as u32,
            data: buf,
        }
    } else {
        SubmitResult {
            status: 0,
            actual_length: out_body.len() as u32,
            data: Vec::new(),
        }
    }
}

fn send_ret_submit<W: Write>(
    out: &mut W,
    import_id: u32,
    cmd: &CmdSubmit,
    res: &SubmitResult,
) -> Result<()> {
    let ret = RetSubmit {
        seqnum: cmd.seqnum,
        devid: 0,
        direction: cmd.direction,
        ep: cmd.ep,
        status: res.status,
        actual_length: res.actual_length,
        start_frame: 0,
        number_of_packets: 0,
        error_count: 0,
    };
    let mut payload = Vec::with_capacity(URB_HEADER_LEN + res.data.len());
    payload.extend_from_slice(&ret.encode());
    payload.extend_from_slice(&res.data);
    let frame = UsbipFrame::new(import_id, &payload).encode();
    ztls::send_envelope(out, Channel::Usbip, &frame)?;
    Ok(())
}

fn send_ret_unlink<W: Write>(
    out: &mut W,
    import_id: u32,
    u: &CmdUnlink,
    status: i32,
) -> Result<()> {
    let ret = RetUnlink {
        seqnum: u.seqnum,
        devid: 0,
        direction: u.direction,
        ep: u.ep,
        status,
    };
    let frame = UsbipFrame::new(import_id, &ret.encode()).encode();
    ztls::send_envelope(out, Channel::Usbip, &frame)?;
    Ok(())
}

/// Build a 32-byte "config + interface + 2 endpoints" descriptor blob.
/// Layout per USB 2.0 §9.6.{3,5,6}; only the lengths and types are
/// canonical, the rest is filler the receiver path doesn't introspect.
fn synth_config_descriptor(class: u8, subclass: u8, proto: u8) -> Vec<u8> {
    let mut v = Vec::with_capacity(32);
    // Configuration descriptor: 9 bytes.
    v.extend_from_slice(&[
        0x09, // bLength
        0x02, // bDescriptorType (CONFIGURATION)
        0x20, 0x00, // wTotalLength = 32
        0x01, // bNumInterfaces
        0x01, // bConfigurationValue
        0x00, // iConfiguration
        0x80, // bmAttributes (bus-powered)
        0x32, // bMaxPower (100mA)
    ]);
    // Interface descriptor: 9 bytes.
    v.extend_from_slice(&[
        0x09, 0x04, // bLength, bDescriptorType (INTERFACE)
        0x00, // bInterfaceNumber
        0x00, // bAlternateSetting
        0x02, // bNumEndpoints
        class, subclass, proto, // bInterfaceClass / Subclass / Protocol
        0x00, // iInterface
    ]);
    // Endpoint descriptor IN bulk: 7 bytes.
    v.extend_from_slice(&[
        0x07, 0x05, // bLength, bDescriptorType (ENDPOINT)
        0x81, // bEndpointAddress (IN, ep 1)
        0x02, // bmAttributes (bulk)
        0x40, 0x00, // wMaxPacketSize = 64
        0x00, // bInterval
    ]);
    // Endpoint descriptor OUT bulk: 7 bytes.
    v.extend_from_slice(&[
        0x07, 0x05, // bLength, bDescriptorType
        0x02, // bEndpointAddress (OUT, ep 2)
        0x02, // bmAttributes (bulk)
        0x40, 0x00, // wMaxPacketSize = 64
        0x00, // bInterval
    ]);
    assert_eq!(v.len(), 32);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Tiny in-process loopback: push one CMD_SUBMIT (GET_DESCRIPTOR
    /// Device) into the pump via a paired Cursor, see a well-shaped
    /// RET_SUBMIT come back.
    #[test]
    fn pump_returns_device_descriptor() {
        let import_id = 7u32;
        let dev = FakeDevice::mass_storage(0x046d, 0xc52b);

        // Build the inbound envelope: Channel::Usbip { UsbipFrame { import_id, CMD_SUBMIT + setup } }
        let setup = [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 18, 0x00];
        let cmd = CmdSubmit {
            seqnum: 1,
            devid: 0x0001_0002,
            direction: URB_DIR_IN,
            ep: 0,
            transfer_flags: 0,
            transfer_buffer_length: 18,
            start_frame: 0,
            number_of_packets: 0,
            interval: 0,
            setup,
        };
        let mut urb = Vec::new();
        urb.extend_from_slice(&cmd.encode());
        let frame = UsbipFrame::new(import_id, &urb).encode();
        let env =
            zerowire_protocol::envelope::Envelope::new(Channel::Usbip, &frame).encode().unwrap();

        // Pair a Cursor for input + a Vec for output, hooked into a tiny
        // duplex adapter.
        struct Duplex<'a> {
            r: Cursor<&'a [u8]>,
            w: Vec<u8>,
        }
        impl<'a> Read for Duplex<'a> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.r.read(buf)
            }
        }
        impl<'a> Write for Duplex<'a> {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.w.write(buf)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.w.flush()
            }
        }

        let mut d = Duplex {
            r: Cursor::new(&env[..]),
            w: Vec::new(),
        };
        let stats = run_pump(&mut d, import_id, &dev).unwrap();
        assert_eq!(stats.submits_seen, 1);
        assert_eq!(stats.submits_ok, 1);
        assert_eq!(stats.control_in_bytes, 18);

        // Parse out the response: one envelope, channel Usbip, UsbipFrame,
        // then RetSubmit + 18 bytes.
        let resp = d.w;
        let (env, _) = zerowire_protocol::envelope::Envelope::parse(&resp)
            .unwrap()
            .expect("response envelope");
        let f = UsbipFrame::parse(env.payload).unwrap();
        assert_eq!(f.import_id, import_id);
        let ret = RetSubmit::parse(&f.raw[..URB_HEADER_LEN]).unwrap();
        assert_eq!(ret.seqnum, 1);
        assert_eq!(ret.status, 0);
        assert_eq!(ret.actual_length, 18);
        let body = &f.raw[URB_HEADER_LEN..URB_HEADER_LEN + 18];
        assert_eq!(body[0], 0x12); // bLength = 18
        assert_eq!(body[1], 0x01); // bDescriptorType = DEVICE
        assert_eq!(&body[8..10], &0x046du16.to_le_bytes());
    }

    /// Bulk IN returns deterministic bytes the receiver can assert on.
    #[test]
    fn pump_bulk_in_returns_payload() {
        let import_id = 3u32;
        let dev = FakeDevice::mass_storage(0x046d, 0xc52b);
        let cmd = CmdSubmit {
            seqnum: 99,
            devid: 0,
            direction: URB_DIR_IN,
            ep: 1, // bulk IN, address 0x81
            transfer_flags: 0,
            transfer_buffer_length: 16,
            start_frame: 0,
            number_of_packets: 0,
            interval: 0,
            setup: [0; 8],
        };
        let urb = cmd.encode();
        let frame = UsbipFrame::new(import_id, &urb).encode();
        let env =
            zerowire_protocol::envelope::Envelope::new(Channel::Usbip, &frame).encode().unwrap();

        struct Duplex<'a> {
            r: Cursor<&'a [u8]>,
            w: Vec<u8>,
        }
        impl<'a> Read for Duplex<'a> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.r.read(buf)
            }
        }
        impl<'a> Write for Duplex<'a> {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.w.write(buf)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.w.flush()
            }
        }

        let mut d = Duplex {
            r: Cursor::new(&env[..]),
            w: Vec::new(),
        };
        let stats = run_pump(&mut d, import_id, &dev).unwrap();
        assert_eq!(stats.bulk_in_bytes, 16);
        let resp = d.w;
        let (env, _) =
            zerowire_protocol::envelope::Envelope::parse(&resp).unwrap().unwrap();
        let f = UsbipFrame::parse(env.payload).unwrap();
        let ret = RetSubmit::parse(&f.raw[..URB_HEADER_LEN]).unwrap();
        assert_eq!(ret.actual_length, 16);
        let body = &f.raw[URB_HEADER_LEN..URB_HEADER_LEN + 16];
        // Byte i should equal (seqnum + i) mod 256 = (99 + i) & 0xff.
        for (i, b) in body.iter().enumerate() {
            assert_eq!(*b, (99 + i) as u8);
        }
    }

    #[test]
    fn pump_unlink_acks() {
        let import_id = 1u32;
        let dev = FakeDevice::mass_storage(0, 0);
        let u = CmdUnlink {
            seqnum: 22,
            devid: 0,
            direction: 0,
            ep: 0,
            unlink_seqnum: 7,
        };
        let frame = UsbipFrame::new(import_id, &u.encode()).encode();
        let env =
            zerowire_protocol::envelope::Envelope::new(Channel::Usbip, &frame).encode().unwrap();
        struct Duplex<'a> {
            r: Cursor<&'a [u8]>,
            w: Vec<u8>,
        }
        impl<'a> Read for Duplex<'a> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.r.read(buf)
            }
        }
        impl<'a> Write for Duplex<'a> {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.w.write(buf)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.w.flush()
            }
        }
        let mut d = Duplex {
            r: Cursor::new(&env[..]),
            w: Vec::new(),
        };
        let stats = run_pump(&mut d, import_id, &dev).unwrap();
        assert_eq!(stats.unlinks_seen, 1);
        let resp = d.w;
        let (env, _) =
            zerowire_protocol::envelope::Envelope::parse(&resp).unwrap().unwrap();
        let f = UsbipFrame::parse(env.payload).unwrap();
        let r = RetUnlink::parse(&f.raw[..URB_HEADER_LEN]).unwrap();
        assert_eq!(r.seqnum, 22);
        assert_eq!(r.status, 0);
    }
}
