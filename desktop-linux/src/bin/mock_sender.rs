//! zerowire-mock-sender — pretend to be the Android sender.
//!
//! Listens on a TCP port, accepts one connection, walks through the zerowire
//! HELLO → DEVICE_LIST → ATTACH → BIND_ACK/ATTACH_OK_USBIP flow, then
//! streams either fake HID reports (`--mode hid`, default) or fake USB/IP
//! URBs (`--mode usbip`). Lets us verify the Linux receiver end-to-end
//! without any Android hardware.
//!
//! When `--psk <code>` is set, the connection is wrapped in TLS 1.3 using
//! the deterministic pairing-PSK identity (see `tls.rs`).

use std::net::TcpListener;
use std::time::Duration;

use anyhow::{bail, Result};
use clap::{Parser, ValueEnum};

use zerowire_cli::receiver::{hid_envelope, synth_mouse_bind_ack_body};
use zerowire_cli::tls::{self as ztls, PskIdentity};
use zerowire_cli::usbip as zusbip;
use zerowire_cli::wire as zwire;

use zerowire_protocol::{
    control::{ControlMessage, DeviceSummary},
    envelope::{Channel, Envelope},
    hid::{HidFrame, HidOp},
    usbip::{UsbipAttachInfo, UsbipFrame},
};

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Mode {
    /// HID fast lane — boot-mouse reports. v0.1 compatible.
    Hid,
    /// USB/IP general passthrough — synthesizes URB-shaped frames so the
    /// receiver's simulated vhci attach has something to log.
    Usbip,
}

#[derive(Parser, Debug)]
#[command(name = "zerowire-mock-sender", about = "Stand-in for the Android sender.")]
struct Args {
    /// Bind on this address, e.g. `127.0.0.1:47823`.
    #[arg(long, default_value = "127.0.0.1:47823")]
    listen: String,
    /// Stop after this many reports/URBs.
    #[arg(long, default_value_t = 50)]
    reports: u32,
    /// Sleep between reports/URBs (milliseconds).
    #[arg(long, default_value_t = 20)]
    interval_ms: u64,
    /// busid we expose. Anything reasonable; receiver echoes it back.
    #[arg(long, default_value = "1-2")]
    busid: String,
    /// Don't `Unbind`/close; keep the connection open until killed. Useful
    /// when you want to drive the receiver manually.
    #[arg(long)]
    persistent: bool,
    /// What kind of device to emulate.
    #[arg(long, value_enum, default_value_t = Mode::Hid)]
    mode: Mode,
    /// When set, run the session over TLS 1.3 using a PSK derived from the
    /// pairing code via HKDF-SHA256.
    #[arg(long)]
    psk: Option<String>,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();
    let listener = TcpListener::bind(&args.listen)?;
    log::info!(
        "zerowire-mock-sender listening on {} (mode={:?} tls={})",
        args.listen,
        args.mode,
        args.psk.is_some()
    );
    let (sock, peer) = listener.accept()?;
    log::info!("peer {peer:?} connected");
    sock.set_nodelay(true)?;
    sock.set_read_timeout(Some(Duration::from_secs(30)))?;

    if let Some(code) = &args.psk {
        let identity = PskIdentity::derive(code)?;
        log::info!(
            "tls identity derived from psk (cert fingerprint: {})",
            short_fp(&identity.fingerprint)
        );
        let cfg = ztls::server_config(&identity)?;
        let mut s = ztls::server_accept(cfg, sock)?;
        run_session(&mut s, &args)
    } else {
        let mut s = sock;
        run_session(&mut s, &args)
    }
}

fn short_fp(fp: &[u8; 32]) -> String {
    let mut out = String::new();
    for b in &fp[..8] {
        use std::fmt::Write;
        write!(out, "{b:02x}").unwrap();
    }
    out
}

fn run_session<S: std::io::Read + std::io::Write>(s: &mut S, args: &Args) -> Result<()> {
    // HELLO
    match ztls::recv_control(s)? {
        ControlMessage::Hello { version, client, .. } => {
            log::info!("hello v{version} from {client}");
        }
        other => bail!("expected HELLO, got {:?}", other),
    }
    ztls::send_control(
        s,
        &ControlMessage::HelloAck {
            sender_id: "mock-sender".into(),
            name: "zerowire-mock".into(),
            supports: vec!["hid-fastlane/1".into(), "usbip/1.1.1".into()],
        },
    )?;

    // LIST_DEVICES
    match ztls::recv_control(s)? {
        ControlMessage::ListDevices => {}
        other => bail!("expected LIST_DEVICES, got {:?}", other),
    }
    let device = match args.mode {
        Mode::Hid => DeviceSummary {
            busid: args.busid.clone(),
            vendor_id: 0xBADD,
            product_id: 0xC0DE,
            manufacturer: Some("builtbyzero".into()),
            product: Some("zerowire-mock mouse".into()),
            serial: None,
            device_class: 0,
            device_subclass: 0,
            device_protocol: 0,
            is_hid: true,
        },
        Mode::Usbip => DeviceSummary {
            busid: args.busid.clone(),
            vendor_id: 0x046d,
            product_id: 0xc52b,
            manufacturer: Some("builtbyzero".into()),
            product: Some("zerowire-mock mass-storage".into()),
            serial: Some("sim-0001".into()),
            device_class: 0x08,
            device_subclass: 0x06,
            device_protocol: 0x50,
            is_hid: false,
        },
    };
    ztls::send_control(
        s,
        &ControlMessage::DeviceList {
            devices: vec![device.clone()],
        },
    )?;

    // ATTACH
    let expected_mode = match args.mode {
        Mode::Hid => "hid",
        Mode::Usbip => "usbip",
    };
    match ztls::recv_control(s)? {
        ControlMessage::Attach { busid, mode } => {
            if busid != args.busid {
                bail!("receiver wants busid {busid:?}, we only offer {:?}", args.busid);
            }
            if mode != expected_mode {
                bail!("receiver wants mode {mode:?}, we only offer {expected_mode:?}");
            }
        }
        other => bail!("expected ATTACH, got {:?}", other),
    }

    match args.mode {
        Mode::Hid => run_hid(s, args),
        Mode::Usbip => run_usbip(s, args, &device),
    }
}

fn run_hid<S: std::io::Read + std::io::Write>(s: &mut S, args: &Args) -> Result<()> {
    ztls::send_control(
        s,
        &ControlMessage::AttachOk {
            busid: args.busid.clone(),
            import_id: 1,
        },
    )?;

    let env = ztls::recv_envelope(s)?;
    if env.channel != Channel::Hid {
        bail!("expected HID envelope, got {:?}", env.channel);
    }
    let bind_frame = HidFrame::parse(&env.payload)?;
    if bind_frame.op != HidOp::Bind {
        bail!("expected HID Bind, got {:?}", bind_frame.op);
    }
    let bind_id = if bind_frame.bind_id == 0 { 1 } else { bind_frame.bind_id };
    let body = synth_mouse_bind_ack_body(&args.busid);
    let ack = HidFrame::new(HidOp::BindAck, bind_id, 0, &body);
    s.write_all(&hid_envelope(&ack))?;
    log::info!("bind_ack sent (bind_id={bind_id})");

    log::info!("streaming {} mouse reports", args.reports);
    for i in 0..args.reports {
        let report = [0u8, 3, 0, 0];
        let f = HidFrame::new(HidOp::ReportIn, bind_id, (i + 1) as u16, &report);
        s.write_all(&hid_envelope(&f))?;
        std::thread::sleep(Duration::from_millis(args.interval_ms));
    }
    log::info!("done streaming");

    if !args.persistent {
        let body = br#"{"reason":"mock done"}"#;
        let f = HidFrame::new(HidOp::Unbind, bind_id, 0, body);
        s.write_all(&hid_envelope(&f))?;
        log::info!("unbind sent; closing");
    } else {
        log::info!("--persistent set; idling. Ctrl-C to stop.");
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
    Ok(())
}

fn run_usbip<S: std::io::Read + std::io::Write>(
    s: &mut S,
    args: &Args,
    device: &DeviceSummary,
) -> Result<()> {
    let import_id: u32 = 7;
    let descriptor = zusbip::synth_device_descriptor(device.vendor_id, device.product_id, device.device_class);
    let mut hex_str = String::with_capacity(descriptor.len() * 2);
    for b in descriptor {
        use std::fmt::Write;
        write!(hex_str, "{b:02x}").unwrap();
    }
    ztls::send_control(
        s,
        &ControlMessage::AttachOkUsbip {
            info: UsbipAttachInfo {
                busid: args.busid.clone(),
                import_id,
                // Synthetic devid: (bus << 16) | dev, with bus=1 dev=2.
                devid: 0x0001_0002,
                speed: 3,
                vendor_id: device.vendor_id,
                product_id: device.product_id,
                descriptor_hex: Some(hex_str),
            },
        },
    )?;

    log::info!("streaming {} fake URBs (import_id={import_id})", args.reports);
    // 48-byte plausible URB-shaped blob. The receiver simulate path treats
    // these as opaque.
    let urb_body = vec![0xABu8; 48];
    for _ in 0..args.reports {
        let frame = UsbipFrame::new(import_id, &urb_body).encode();
        let env = Envelope::new(Channel::Usbip, &frame).encode()?;
        s.write_all(&env)?;
        std::thread::sleep(Duration::from_millis(args.interval_ms));
    }
    log::info!("done streaming usbip");
    if !args.persistent {
        // Close cleanly.
    } else {
        log::info!("--persistent set; idling. Ctrl-C to stop.");
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
    Ok(())
}

// Suppress unused-imports for wire (we routed everything through ztls).
#[allow(dead_code)]
fn _unused() {
    let _ = zwire::OwnedEnvelope {
        channel: Channel::Control,
        payload: vec![],
    };
}
