//! Receiver-side **fake vhci-hcd**: issues a deterministic CMD_SUBMIT /
//! CMD_UNLINK script over the established usbip session, parses the
//! incoming RET_SUBMIT / RET_UNLINK responses, and writes a transcript +
//! summary line the loopback test asserts on.
//!
//! The real Linux receiver in production hands the TCP socket to the
//! kernel's vhci-hcd driver and is done. For CI / dev hosts that don't
//! have vhci-hcd, this module pretends to be vhci-hcd from the wire
//! side: it issues the URBs a real kernel would issue on attach + a few
//! bulk transfers, so the sender-side URB pump (Kotlin in production,
//! the Rust fixture in tests) has something to respond to.
//!
//! What it issues by default (`script_default`):
//!
//!   1. GET_DESCRIPTOR(Device, 18) on ep 0           — control IN
//!   2. GET_DESCRIPTOR(Config,  9) on ep 0           — control IN
//!   3. GET_DESCRIPTOR(Config, 32) on ep 0           — control IN
//!   4. SET_CONFIGURATION(1)       on ep 0           — control OUT (0-data)
//!   5. Bulk IN  on ep 1, 64 bytes                   — bulk IN
//!   6. Bulk OUT on ep 2, 64 bytes ascending pattern — bulk OUT
//!   7. Bulk IN  on ep 1, 32 bytes                   — bulk IN
//!   8. CMD_UNLINK targeting a fake seqnum           — unlink
//!
//! Each RET_SUBMIT is logged to the transcript so the test can assert on
//! actual_length / status. The function returns the URB count plus how
//! many RET_SUBMITs returned status==0.

use std::io::{Read, Write};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

use zerowire_protocol::{
    envelope::Channel,
    usbip::{
        peek_urb_op, CmdSubmit, CmdUnlink, RetSubmit, RetUnlink, UrbOp, UsbipFrame, URB_DIR_IN,
        URB_DIR_OUT, URB_HEADER_LEN,
    },
};

use crate::tls as ztls;

/// One URB the driver will issue, plus the assertion it expects on the
/// reply. The fixture pump fills in the deterministic IN-data; the
/// driver matches `expected_actual_length` / `expected_status`.
#[derive(Debug, Clone)]
pub struct ScriptedUrb {
    pub label: String,
    pub seqnum: u32,
    pub ep: u32,
    pub direction: u32,
    pub setup: [u8; 8],
    pub transfer_buffer_length: u32,
    /// OUT-only body. Always empty for IN xfers.
    pub out_body: Vec<u8>,
    pub expected_status: i32,
    pub expected_actual_length: u32,
}


/// Default URB sequence; what `--simulate-issue-urbs N` issues. `n_bulk`
/// scales the body of the bulk-IN read (used in higher-N test runs).
pub fn script_default(n_extra_bulk_in: u32, devid: u32) -> Vec<ScriptedUrb> {
    let _ = devid;
    let mut s = vec![
        ScriptedUrb {
            label: "GET_DESCRIPTOR(Device,18)".into(),
            seqnum: 1,
            ep: 0,
            direction: URB_DIR_IN,
            setup: [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 18, 0x00],
            transfer_buffer_length: 18,
            out_body: Vec::new(),
            expected_status: 0,
            expected_actual_length: 18,
        },
        ScriptedUrb {
            label: "GET_DESCRIPTOR(Config,9)".into(),
            seqnum: 2,
            ep: 0,
            direction: URB_DIR_IN,
            setup: [0x80, 0x06, 0x00, 0x02, 0x00, 0x00, 9, 0x00],
            transfer_buffer_length: 9,
            out_body: Vec::new(),
            expected_status: 0,
            expected_actual_length: 9,
        },
        ScriptedUrb {
            label: "GET_DESCRIPTOR(Config,32)".into(),
            seqnum: 3,
            ep: 0,
            direction: URB_DIR_IN,
            setup: [0x80, 0x06, 0x00, 0x02, 0x00, 0x00, 32, 0x00],
            transfer_buffer_length: 32,
            out_body: Vec::new(),
            expected_status: 0,
            expected_actual_length: 32,
        },
        ScriptedUrb {
            label: "SET_CONFIGURATION(1)".into(),
            seqnum: 4,
            ep: 0,
            direction: URB_DIR_OUT,
            setup: [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
            transfer_buffer_length: 0,
            out_body: Vec::new(),
            expected_status: 0,
            expected_actual_length: 0,
        },
        ScriptedUrb {
            label: "bulk-IN-64".into(),
            seqnum: 5,
            ep: 1,
            direction: URB_DIR_IN,
            setup: [0; 8],
            transfer_buffer_length: 64,
            out_body: Vec::new(),
            expected_status: 0,
            expected_actual_length: 64,
        },
        ScriptedUrb {
            label: "bulk-OUT-64".into(),
            seqnum: 6,
            ep: 2,
            direction: URB_DIR_OUT,
            setup: [0; 8],
            transfer_buffer_length: 64,
            out_body: (0..64u8).collect(),
            expected_status: 0,
            expected_actual_length: 64,
        },
        ScriptedUrb {
            label: "bulk-IN-32".into(),
            seqnum: 7,
            ep: 1,
            direction: URB_DIR_IN,
            setup: [0; 8],
            transfer_buffer_length: 32,
            out_body: Vec::new(),
            expected_status: 0,
            expected_actual_length: 32,
        },
    ];
    for i in 0..n_extra_bulk_in {
        let seqnum = 100 + i;
        s.push(ScriptedUrb {
            label: format!("extra-bulk-IN-{seqnum}"),
            seqnum,
            ep: 1,
            direction: URB_DIR_IN,
            setup: [0; 8],
            transfer_buffer_length: 64,
            out_body: Vec::new(),
            expected_status: 0,
            expected_actual_length: 64,
        });
    }
    s
}

#[derive(Debug, Default, Clone)]
pub struct DriverStats {
    pub urbs_issued: u32,
    pub urbs_ok: u32,
    pub unlinks_issued: u32,
    pub unlinks_ok: u32,
    pub bytes_returned: u64,
}

/// Issue the scripted URBs on `stream` (the post-attach usbip session)
/// and assert on the responses. Each response is logged to `transcript`.
pub fn run_script<S: Read + Write>(
    stream: &mut S,
    import_id: u32,
    script: &[ScriptedUrb],
    transcript: &Path,
) -> Result<DriverStats> {
    use std::fs::OpenOptions;
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(transcript)
        .with_context(|| format!("opening {}", transcript.display()))?;

    let mut stats = DriverStats::default();
    for urb in script {
        // Build CMD_SUBMIT.
        let cmd = CmdSubmit {
            seqnum: urb.seqnum,
            devid: 0x0001_0002,
            direction: urb.direction,
            ep: urb.ep,
            transfer_flags: 0,
            transfer_buffer_length: urb.transfer_buffer_length,
            start_frame: 0,
            number_of_packets: 0,
            interval: 0,
            setup: urb.setup,
        };
        let mut payload = Vec::with_capacity(URB_HEADER_LEN + urb.out_body.len());
        payload.extend_from_slice(&cmd.encode());
        if !urb.out_body.is_empty() {
            payload.extend_from_slice(&urb.out_body);
        }
        let frame = UsbipFrame::new(import_id, &payload).encode();
        ztls::send_envelope(stream, Channel::Usbip, &frame)?;
        stats.urbs_issued += 1;

        // Read the matching RET_SUBMIT.
        let env = ztls::recv_envelope(stream)?;
        if env.channel != Channel::Usbip {
            bail!("expected Usbip envelope, got {:?}", env.channel);
        }
        let f = UsbipFrame::parse(&env.payload)?;
        if f.import_id != import_id {
            bail!(
                "RET frame import_id {:#x} != session {:#x}",
                f.import_id,
                import_id
            );
        }
        let op = peek_urb_op(f.raw)
            .ok_or_else(|| anyhow!("unknown URB op in reply for {}", urb.label))?;
        if op != UrbOp::RetSubmit {
            bail!("expected RET_SUBMIT for {}, got {:?}", urb.label, op);
        }
        let ret = RetSubmit::parse(&f.raw[..URB_HEADER_LEN])?;
        let body_len = (f.raw.len() - URB_HEADER_LEN) as u32;
        writeln!(
            log,
            "sim-urb {} seq={} ep={} dir={} status={} actual_length={} body_bytes={}",
            urb.label, ret.seqnum, urb.ep, urb.direction, ret.status, ret.actual_length, body_len
        )
        .ok();

        if ret.status == urb.expected_status && ret.actual_length == urb.expected_actual_length {
            stats.urbs_ok += 1;
        } else {
            bail!(
                "URB {} failed: status={} (want {}), actual_length={} (want {})",
                urb.label,
                ret.status,
                urb.expected_status,
                ret.actual_length,
                urb.expected_actual_length,
            );
        }
        if urb.direction == URB_DIR_IN {
            stats.bytes_returned += ret.actual_length as u64;
            // Cross-check the body length matches actual_length.
            if body_len != ret.actual_length {
                bail!(
                    "URB {} body bytes ({body_len}) != actual_length ({})",
                    urb.label,
                    ret.actual_length,
                );
            }
        }
    }

    // One final CMD_UNLINK targeting a synthetic seqnum, to exercise the
    // unlink path. Sender always returns RET_UNLINK status=0.
    let unlink = CmdUnlink {
        seqnum: 9001,
        devid: 0x0001_0002,
        direction: URB_DIR_OUT,
        ep: 0,
        unlink_seqnum: 5,
    };
    let payload = unlink.encode();
    let frame = UsbipFrame::new(import_id, &payload).encode();
    ztls::send_envelope(stream, Channel::Usbip, &frame)?;
    stats.unlinks_issued += 1;
    let env = ztls::recv_envelope(stream)?;
    if env.channel != Channel::Usbip {
        bail!("expected Usbip envelope for unlink, got {:?}", env.channel);
    }
    let f = UsbipFrame::parse(&env.payload)?;
    let op = peek_urb_op(f.raw).ok_or_else(|| anyhow!("unknown URB op in unlink reply"))?;
    if op != UrbOp::RetUnlink {
        bail!("expected RET_UNLINK, got {:?}", op);
    }
    let r = RetUnlink::parse(&f.raw[..URB_HEADER_LEN])?;
    writeln!(
        log,
        "sim-unlink seq={} status={} target_seq={}",
        r.seqnum, r.status, unlink.unlink_seqnum
    )
    .ok();
    if r.status == 0 {
        stats.unlinks_ok += 1;
    } else {
        bail!("RET_UNLINK status {} != 0", r.status);
    }

    writeln!(
        log,
        "sim-summary urbs_issued={} urbs_ok={} unlinks_issued={} unlinks_ok={} bytes_returned={}",
        stats.urbs_issued,
        stats.urbs_ok,
        stats.unlinks_issued,
        stats.unlinks_ok,
        stats.bytes_returned
    )
    .ok();

    Ok(stats)
}
