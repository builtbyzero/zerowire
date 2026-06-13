//! zerowire-simulate-android-pump — stand-in for the Android URB pump.
//!
//! Speaks the same envelope + USB/IP wire format as the Kotlin
//! `UsbIpHost.kt` running on a real phone:
//!
//!   1. accept one TCP connection (optionally TLS via the same PSK
//!      identity as the rest of v0.2),
//!   2. walk the v0.2 handshake — HELLO → DEVICE_LIST → ATTACH(usbip) →
//!      ATTACH_OK_USBIP,
//!   3. run the urb_pump against a `FakeDevice` until the receiver
//!      closes.
//!
//! Used by `tests/android_pump_loopback.sh` to exercise the v0.3 URB
//! pump end-to-end on dev boxes without an Android phone. The wire
//! format is bit-identical to what the Kotlin pump emits, so a green
//! loopback proves the receiver side of v0.3 is correct; the only thing
//! left for hardware is `UsbDeviceConnection`-vs-`FakeDevice`.

use std::net::TcpListener;

use anyhow::{bail, Result};
use clap::Parser;

use zerowire_cli::tls::{self as ztls, PskIdentity};
use zerowire_cli::urb_pump::{run_pump, FakeDevice};
use zerowire_cli::usbip as zusbip;

use zerowire_protocol::{
    control::{ControlMessage, DeviceSummary},
    usbip::UsbipAttachInfo,
};

#[derive(Parser, Debug)]
#[command(
    name = "zerowire-simulate-android-pump",
    about = "Fixture sender pump — what the Android UsbIpHost.kt does, in Rust against a FakeDevice."
)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:47823")]
    listen: String,
    #[arg(long, default_value = "1-2")]
    busid: String,
    /// 32-bit import_id we hand back in ATTACH_OK_USBIP.
    #[arg(long, default_value_t = 7)]
    import_id: u32,
    /// USB vendor + product ids the fake mass-storage device pretends to be.
    #[arg(long, default_value_t = 0x046d)]
    vendor: u16,
    #[arg(long, default_value_t = 0xc52b)]
    product: u16,
    /// Optional PSK for TLS 1.3 mTLS.
    #[arg(long)]
    psk: Option<String>,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();
    let listener = TcpListener::bind(&args.listen)?;
    log::info!(
        "zerowire-simulate-android-pump listening on {} (tls={})",
        args.listen,
        args.psk.is_some()
    );
    let (sock, peer) = listener.accept()?;
    log::info!("peer {peer:?} connected");
    sock.set_nodelay(true)?;

    if let Some(code) = &args.psk {
        let identity = PskIdentity::derive(code)?;
        let cfg = ztls::server_config(&identity)?;
        let mut s = ztls::server_accept(cfg, sock)?;
        run_session(&mut s, &args)
    } else {
        let mut s = sock;
        run_session(&mut s, &args)
    }
}

fn run_session<S: std::io::Read + std::io::Write>(s: &mut S, args: &Args) -> Result<()> {
    // 1. HELLO
    match ztls::recv_control(s)? {
        ControlMessage::Hello { version, client, .. } => {
            log::info!("hello v{version} from {client}");
        }
        other => bail!("expected HELLO, got {:?}", other),
    }
    ztls::send_control(
        s,
        &ControlMessage::HelloAck {
            sender_id: "simulate-android-pump".into(),
            name: "zerowire-android-pump".into(),
            supports: vec!["usbip/1.1.1".into()],
        },
    )?;

    // 2. LIST_DEVICES
    match ztls::recv_control(s)? {
        ControlMessage::ListDevices => {}
        other => bail!("expected LIST_DEVICES, got {:?}", other),
    }
    let device = DeviceSummary {
        busid: args.busid.clone(),
        vendor_id: args.vendor,
        product_id: args.product,
        manufacturer: Some("builtbyzero".into()),
        product: Some("zerowire-android-pump fake mass-storage".into()),
        serial: Some("sim-0001".into()),
        device_class: 0x08,
        device_subclass: 0x06,
        device_protocol: 0x50,
        is_hid: false,
    };
    ztls::send_control(
        s,
        &ControlMessage::DeviceList {
            devices: vec![device.clone()],
        },
    )?;

    // 3. ATTACH(usbip)
    match ztls::recv_control(s)? {
        ControlMessage::Attach { busid, mode } => {
            if busid != args.busid {
                bail!(
                    "receiver wants busid {:?}, we only offer {:?}",
                    busid,
                    args.busid
                );
            }
            if mode != "usbip" {
                bail!("receiver wants mode {mode:?}, only usbip supported");
            }
        }
        other => bail!("expected ATTACH, got {:?}", other),
    }

    let descriptor = zusbip::synth_device_descriptor(args.vendor, args.product, 0x08);
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
                import_id: args.import_id,
                devid: 0x0001_0002,
                speed: 3,
                vendor_id: args.vendor,
                product_id: args.product,
                descriptor_hex: Some(hex_str),
            },
        },
    )?;

    // 4. URB pump
    let device = FakeDevice::mass_storage(args.vendor, args.product);
    let stats = run_pump(s, args.import_id, &device)?;
    log::info!(
        "pump done: submits_seen={} submits_ok={} unlinks={} ctrl_in_bytes={} bulk_in_bytes={} bulk_out_bytes={} unsupported={}",
        stats.submits_seen, stats.submits_ok, stats.unlinks_seen,
        stats.control_in_bytes, stats.bulk_in_bytes, stats.bulk_out_bytes,
        stats.unsupported,
    );
    Ok(())
}
