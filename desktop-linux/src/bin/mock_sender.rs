//! zerowire-mock-sender — pretend to be the Android sender.
//!
//! Listens on a TCP port, accepts one connection, walks through the zerowire
//! HELLO → DEVICE_LIST → ATTACH → BIND_ACK flow, then streams a programmable
//! number of fake mouse-move HID reports. Lets us verify the Linux receiver
//! end-to-end without any Android hardware.

use std::io::Write;
use std::net::TcpListener;
use std::time::Duration;

use anyhow::{bail, Result};
use clap::Parser;

use zerowire_cli::receiver::{hid_envelope, synth_mouse_bind_ack_body};
use zerowire_cli::wire::{recv_control, send_control};

use zerowire_protocol::{
    control::{ControlMessage, DeviceSummary},
    envelope::Channel,
    hid::{HidFrame, HidOp},
};

#[derive(Parser, Debug)]
#[command(name = "zerowire-mock-sender", about = "Stand-in for the Android sender.")]
struct Args {
    /// Bind on this address, e.g. `127.0.0.1:47823`.
    #[arg(long, default_value = "127.0.0.1:47823")]
    listen: String,
    /// Stop after this many mouse-move reports.
    #[arg(long, default_value_t = 50)]
    reports: u32,
    /// Sleep between reports (milliseconds).
    #[arg(long, default_value_t = 20)]
    interval_ms: u64,
    /// busid we expose. Anything reasonable; receiver echoes it back.
    #[arg(long, default_value = "1-2")]
    busid: String,
    /// Don't `Unbind`/close; keep the connection open until killed. Useful
    /// when you want to drive the receiver manually.
    #[arg(long)]
    persistent: bool,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();
    let listener = TcpListener::bind(&args.listen)?;
    log::info!("zerowire-mock-sender listening on {}", args.listen);
    let (mut sock, peer) = listener.accept()?;
    log::info!("peer {peer:?} connected");
    sock.set_nodelay(true)?;
    sock.set_read_timeout(Some(Duration::from_secs(30)))?;

    // HELLO
    match recv_control(&mut sock)? {
        ControlMessage::Hello { version, client, .. } => {
            log::info!("hello v{version} from {client}");
        }
        other => bail!("expected HELLO, got {:?}", other),
    }
    send_control(
        &mut sock,
        &ControlMessage::HelloAck {
            sender_id: "mock-sender".into(),
            name: "zerowire-mock".into(),
            supports: vec!["hid-fastlane/1".into()],
        },
    )?;

    // LIST_DEVICES
    match recv_control(&mut sock)? {
        ControlMessage::ListDevices => {}
        other => bail!("expected LIST_DEVICES, got {:?}", other),
    }
    let device = DeviceSummary {
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
    };
    send_control(
        &mut sock,
        &ControlMessage::DeviceList { devices: vec![device.clone()] },
    )?;

    // ATTACH
    match recv_control(&mut sock)? {
        ControlMessage::Attach { busid, mode } => {
            if busid != args.busid {
                bail!("receiver wants busid {busid:?}, we only offer {:?}", args.busid);
            }
            if mode != "hid" {
                bail!("only HID mode supported, got {mode:?}");
            }
        }
        other => bail!("expected ATTACH, got {:?}", other),
    }
    send_control(
        &mut sock,
        &ControlMessage::AttachOk { busid: args.busid.clone(), import_id: 1 },
    )?;

    // HID Bind — receive Bind, reply with BindAck.
    let env = zerowire_cli::wire::recv_envelope(&mut sock)?;
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
    sock.write_all(&hid_envelope(&ack))?;
    log::info!("bind_ack sent (bind_id={bind_id})");

    // Stream fake mouse reports.
    log::info!("streaming {} mouse reports", args.reports);
    for i in 0..args.reports {
        // 4-byte boot mouse report: [buttons, dx, dy, wheel]
        let report = [0u8, 3, 0, 0]; // 3px right, no buttons, no wheel
        let f = HidFrame::new(HidOp::ReportIn, bind_id, (i + 1) as u16, &report);
        sock.write_all(&hid_envelope(&f))?;
        std::thread::sleep(Duration::from_millis(args.interval_ms));
    }
    log::info!("done streaming");

    // Tear down unless told to hang.
    if !args.persistent {
        let body = br#"{"reason":"mock done"}"#;
        let f = HidFrame::new(HidOp::Unbind, bind_id, 0, body);
        sock.write_all(&hid_envelope(&f))?;
        log::info!("unbind sent; closing");
    } else {
        log::info!("--persistent set; idling. Ctrl-C to stop.");
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
    Ok(())
}
