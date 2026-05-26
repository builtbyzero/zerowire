//! USB/IP receive path — the v0.2 general-passthrough sibling of `receiver`.
//!
//! Same session shape as the HID fast lane up through `ATTACH`, but the
//! sender replies with `ATTACH_OK_USBIP` (carrying devid/speed/descriptor)
//! and then streams `UsbipFrame`s on `Channel::Usbip`. We either:
//!
//! * hand the socket to the kernel via `usbip::attach_socket` (real mode),
//!   or
//! * log frames to a transcript via `usbip::SimulatedAttach`
//!   (simulate mode — what CI runs).
//!
//! The real-attach path consumes the TCP socket on success — after that
//! point this function returns and userspace has nothing left to do. The
//! simulate path keeps reading until `Unbind` or socket close.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use zerowire_protocol::{
    control::{ControlMessage, DeviceSummary},
    envelope::Channel,
    usbip::{UsbipAttachInfo, UsbipFrame},
};

use crate::usbip::SimulatedAttach;
use crate::wire::{recv_control, recv_envelope, send_control};

pub struct UsbipReceiveOpts {
    pub target: String,
    pub busid: Option<String>,
    pub client_name: String,
    /// When `Some`, write the attach transcript and per-URB summary here
    /// instead of touching `vhci-hcd`.
    pub simulate_transcript: Option<PathBuf>,
    /// In simulate mode, also drive a scripted URB sequence against the
    /// sender's pump (issues GET_DESCRIPTOR / bulk IN / bulk OUT / unlink).
    /// The count adds N extra bulk-IN URBs on top of the base 7-step script.
    /// `None` => v0.2 behaviour: passively log frames the sender pushes.
    pub simulate_drive_urbs: Option<u32>,
}

/// Drive a USB/IP receive session. Returns when the sender unbinds or the
/// socket closes.
pub fn run_usbip_receive(opts: UsbipReceiveOpts, stop: Arc<AtomicBool>) -> Result<()> {
    let addr: SocketAddr = opts
        .target
        .to_socket_addrs()
        .with_context(|| format!("resolving {}", opts.target))?
        .next()
        .ok_or_else(|| anyhow!("no addresses for {}", opts.target))?;
    log::info!("dialing usbip target {}", addr);
    let mut sock = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    sock.set_nodelay(true)?;

    // HELLO.
    send_control(
        &mut sock,
        &ControlMessage::Hello {
            version: 1,
            client: opts.client_name.clone(),
            supports: vec!["usbip/1.1.1".into()],
        },
    )?;
    match recv_control(&mut sock)? {
        ControlMessage::HelloAck { name, .. } => log::info!("hello_ack from {name}"),
        ControlMessage::Error { code, message } => bail!("sender error {code}: {message}"),
        other => bail!("unexpected reply to HELLO: {:?}", other),
    }

    // LIST_DEVICES.
    send_control(&mut sock, &ControlMessage::ListDevices)?;
    let devices: Vec<DeviceSummary> = match recv_control(&mut sock)? {
        ControlMessage::DeviceList { devices } => devices,
        other => bail!("expected DEVICE_LIST, got {:?}", other),
    };
    let chosen = pick_device(&devices, opts.busid.as_deref())?;
    log::info!(
        "attaching busid={} ({})",
        chosen.busid,
        chosen.product.clone().unwrap_or_default()
    );

    // ATTACH (usbip mode).
    send_control(
        &mut sock,
        &ControlMessage::Attach {
            busid: chosen.busid.clone(),
            mode: "usbip".into(),
        },
    )?;
    let info: UsbipAttachInfo = match recv_control(&mut sock)? {
        ControlMessage::AttachOkUsbip { info } => info,
        ControlMessage::AttachDenied { busid, reason } => {
            bail!("sender denied attach for {busid}: {reason}")
        }
        ControlMessage::AttachOk { busid, import_id } => {
            // Old sender — degrade to a synthesized info block; CI mock
            // shouldn't hit this, but keep the code defensive.
            log::warn!("sender returned legacy ATTACH_OK; synthesizing UsbipAttachInfo");
            UsbipAttachInfo {
                busid,
                import_id,
                devid: 0,
                speed: 2,
                vendor_id: chosen.vendor_id,
                product_id: chosen.product_id,
                descriptor_hex: None,
            }
        }
        other => bail!("unexpected reply to ATTACH: {:?}", other),
    };

    if let Some(transcript) = opts.simulate_transcript {
        if let Some(n_extra) = opts.simulate_drive_urbs {
            return run_simulated_drive(&mut sock, &info, &transcript, n_extra);
        }
        return run_simulated(&mut sock, &info, &transcript, stop);
    }

    run_real(sock, &info)
}

/// Simulated attach **plus** an active URB-issuing driver. Used by the
/// v0.3 android-pump loopback test to round-trip real CMD_SUBMIT /
/// CMD_UNLINK packets against the sender pump (fixture or real phone).
fn run_simulated_drive(
    sock: &mut TcpStream,
    info: &UsbipAttachInfo,
    transcript: &std::path::Path,
    n_extra_bulk: u32,
) -> Result<()> {
    let sim = SimulatedAttach::create(transcript, info)?;
    log::info!(
        "simulated vhci-driver attach: port={} busid={} import_id={}",
        sim.port(),
        sim.busid(),
        info.import_id,
    );
    let script = crate::urb_driver::script_default(n_extra_bulk, info.devid);
    let stats = crate::urb_driver::run_script(sock, info.import_id, &script, transcript)?;
    log::info!(
        "simulated drive complete: urbs_issued={} urbs_ok={} unlinks_issued={} unlinks_ok={} bytes={}",
        stats.urbs_issued, stats.urbs_ok, stats.unlinks_issued, stats.unlinks_ok, stats.bytes_returned,
    );
    Ok(())
}

/// Real `vhci-hcd` attach. The socket is consumed by the kernel.
fn run_real(sock: TcpStream, info: &UsbipAttachInfo) -> Result<()> {
    if !crate::usbip::vhci_available() {
        bail!(
            "vhci-hcd not loaded — run `sudo modprobe vhci-hcd` or use \
             --simulate-usbip <path> to exercise the path without the kernel module"
        );
    }
    let port = crate::usbip::attach_socket(&sock, info)?;
    log::info!(
        "kernel now driving busid={} on vhci port={port}; userspace is done",
        info.busid
    );
    // Don't `drop(sock)` immediately — give the kernel a moment to take
    // ownership of the fd (it dup()s on its side).
    std::thread::sleep(Duration::from_millis(50));
    drop(sock);
    Ok(())
}

/// Simulated attach: read USB/IP frames from the wire and log them.
fn run_simulated(
    sock: &mut TcpStream,
    info: &UsbipAttachInfo,
    transcript: &std::path::Path,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let sim = SimulatedAttach::create(transcript, info)?;
    log::info!(
        "simulated vhci attach: port={} busid={}",
        sim.port(),
        sim.busid()
    );
    let mut frames = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let env = match recv_envelope(sock) {
            Ok(e) => e,
            Err(_) => break,
        };
        match env.channel {
            Channel::Usbip => {
                let frame = UsbipFrame::parse(&env.payload).context("parsing UsbipFrame")?;
                frames = frames.wrapping_add(1);
                sim.record_urb(frame.import_id, frames, frame.raw.len())?;
            }
            Channel::Control => {
                // Sender may send a final Error/AttachDenied; honour it.
                if let Ok(m) = ControlMessage::from_json(&env.payload) {
                    if matches!(m, ControlMessage::Error { .. }) {
                        log::warn!("sender control error: {:?}", m);
                        break;
                    }
                }
            }
            Channel::Hid | Channel::Keepalive => {
                // HID frames on a usbip session shouldn't happen, but skip
                // gracefully if they do.
            }
        }
    }
    log::info!("simulated usbip session ended ({frames} URBs relayed)");
    Ok(())
}

fn pick_device(devs: &[DeviceSummary], busid: Option<&str>) -> Result<DeviceSummary> {
    if let Some(b) = busid {
        return devs
            .iter()
            .find(|d| d.busid == b)
            .cloned()
            .ok_or_else(|| anyhow!("sender does not expose busid {b}"));
    }
    devs.first()
        .cloned()
        .ok_or_else(|| anyhow!("sender exposes no devices"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;
    use zerowire_protocol::envelope::Envelope;

    /// Smoke test the simulated-receive path against a stub sender: shake
    /// hands, send one UsbipFrame, hang up. The transcript should record
    /// the attach and the URB.
    #[test]
    fn simulated_receiver_logs_one_urb() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "zerowire-sim-recv-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join("recv.log");

        // Stub sender thread: walks the v0.2 usbip handshake.
        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            // recv HELLO
            let _ = recv_control(&mut s).unwrap();
            send_control(
                &mut s,
                &ControlMessage::HelloAck {
                    sender_id: "test".into(),
                    name: "stub".into(),
                    supports: vec!["usbip/1.1.1".into()],
                },
            )
            .unwrap();
            // recv LIST_DEVICES
            let _ = recv_control(&mut s).unwrap();
            send_control(
                &mut s,
                &ControlMessage::DeviceList {
                    devices: vec![DeviceSummary {
                        busid: "1-3".into(),
                        vendor_id: 0x046d,
                        product_id: 0xc52b,
                        manufacturer: None,
                        product: Some("stub mass-storage".into()),
                        serial: None,
                        device_class: 0x08,
                        device_subclass: 0x06,
                        device_protocol: 0x50,
                        is_hid: false,
                    }],
                },
            )
            .unwrap();
            // recv ATTACH
            let _ = recv_control(&mut s).unwrap();
            send_control(
                &mut s,
                &ControlMessage::AttachOkUsbip {
                    info: UsbipAttachInfo {
                        busid: "1-3".into(),
                        import_id: 42,
                        devid: 0x0001_0003,
                        speed: 3,
                        vendor_id: 0x046d,
                        product_id: 0xc52b,
                        descriptor_hex: Some(hex_encode(
                            &crate::usbip::synth_device_descriptor(0x046d, 0xc52b, 0x08),
                        )),
                    },
                },
            )
            .unwrap();
            // Send one UsbipFrame.
            let urb = UsbipFrame::new(42, &[0u8; 32]).encode();
            let env = Envelope::new(Channel::Usbip, &urb).encode().unwrap();
            s.write_all(&env).unwrap();
            // Close.
            drop(s);
        });

        let stop = Arc::new(AtomicBool::new(false));
        let opts = UsbipReceiveOpts {
            target: addr.to_string(),
            busid: None,
            client_name: "test-client".into(),
            simulate_transcript: Some(transcript.clone()),
            simulate_drive_urbs: None,
        };
        run_usbip_receive(opts, stop).unwrap();
        server.join().unwrap();

        let body = std::fs::read_to_string(&transcript).unwrap();
        assert!(body.contains("sim-attach"), "{body}");
        assert!(body.contains("busid=1-3"), "{body}");
        assert!(body.contains("sim-urb"), "{body}");
        // record_urb is called for each UsbipFrame.
        assert!(body.lines().filter(|l| l.starts_with("sim-urb")).count() >= 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn hex_encode(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            use std::fmt::Write;
            write!(s, "{b:02x}").unwrap();
        }
        s
    }
}
