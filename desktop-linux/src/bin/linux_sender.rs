//! zerowire-linux-sender — Linux/hidraw analogue of the Android sender.
//!
//! Why this exists. The v0.1 stack has three real implementations of the
//! "sender" side: the production Android app, and two stubs — the
//! `zerowire-mock-sender` (purely synthetic JSON inside the same process)
//! and the loopback tests. None of them prove that *kernel-level USB HID
//! reports* survive the trip across the wire. This binary closes that gap
//! on Linux by enumerating real `/dev/hidrawN` devices (which on this
//! host are produced by a usbip-vudc + vhci-hcd loopback — see
//! `docs/synthetic-hw-verify.md`) and feeding their interrupt-in reports
//! verbatim through the existing zerowire protocol.
//!
//! What it mirrors from the Android sender:
//!
//! * `UsbInventory.summarize` → `enumerate_hidraw_devices` (sysfs read).
//! * `SenderService` accept loop → `serve_session`.
//! * `HidEndpoint` interrupt-read loop → `pump_reports`.
//!
//! What it does NOT mirror (deliberately):
//!
//! * mDNS advertisement. Synthetic verification is loopback-only; the
//!   receiver is invoked with `--target 127.0.0.1:47823`.
//! * USB permission UX. On Linux the access gate is filesystem
//!   permissions on `/dev/hidrawN`, handled outside the binary.
//! * Multiple concurrent receivers. v0.1 is one-at-a-time; this matches.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;

use zerowire_cli::hid_descriptor;
use zerowire_cli::receiver::hid_envelope;
use zerowire_cli::wire::{recv_control, recv_envelope, send_control};

use zerowire_protocol::{
    control::{ControlMessage, DeviceSummary},
    envelope::Channel,
    hid::{
        encode_bind_ack_body, BindAckMeta, BindRequest, DeviceKind, HidFrame, HidOp,
    },
};

#[derive(Parser, Debug)]
#[command(
    name = "zerowire-linux-sender",
    about = "Real-/dev/hidrawN sender shim for synthetic hardware verification."
)]
struct Args {
    /// TCP listen address. The receiver dials this with `--target`.
    #[arg(long, default_value = "127.0.0.1:47823")]
    listen: String,

    /// Restrict to a specific `/dev/hidrawN` path. Useful when other HID
    /// devices (touchscreens, keyboards) are also present — we don't want
    /// to expose them by accident.
    #[arg(long)]
    only: Option<PathBuf>,

    /// Only expose hidraw devices whose vendor:product matches this filter,
    /// in `vvvv:pppp` hex form. Useful in CI where the synthetic gadget's
    /// IDs (`badd:c0de`) are known up front.
    #[arg(long)]
    filter_vidpid: Option<String>,

    /// Display name for the HELLO_ACK reply.
    #[arg(long, default_value = "zerowire-linux-sender")]
    name: String,

    /// Quit after the first session ends instead of looping.
    #[arg(long)]
    once: bool,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();
    let stop = Arc::new(AtomicBool::new(false));

    let listener = TcpListener::bind(&args.listen)
        .with_context(|| format!("bind {}", args.listen))?;
    log::info!("listening on {}", args.listen);

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let (sock, peer) = listener.accept()?;
        log::info!("accepted {peer:?}");
        sock.set_nodelay(true)?;
        // Per-session timeout safety net: nothing should silently stall
        // for longer than the keepalive window. The receiver currently
        // doesn't send keepalives during a session, but a stuck handshake
        // shouldn't pin us forever either.
        sock.set_read_timeout(Some(Duration::from_secs(60)))?;

        if let Err(e) = serve_session(sock, &args) {
            log::warn!("session ended: {e:#}");
        }

        if args.once {
            log::info!("--once: exiting");
            break;
        }
    }
    Ok(())
}

fn serve_session(mut sock: TcpStream, args: &Args) -> Result<()> {
    // 1. HELLO ↔ HELLO_ACK.
    match recv_control(&mut sock)? {
        ControlMessage::Hello { version, client, supports } => {
            log::info!("hello v{version} from {client:?} supports={supports:?}");
        }
        other => bail!("expected HELLO, got {other:?}"),
    }
    send_control(
        &mut sock,
        &ControlMessage::HelloAck {
            sender_id: "linux-sender".into(),
            name: args.name.clone(),
            supports: vec!["hid-fastlane/1".into()],
        },
    )?;

    // 2. LIST_DEVICES ↔ DEVICE_LIST.
    let devices = enumerate_hidraw_devices(args.only.as_deref(), args.filter_vidpid.as_deref())
        .context("enumerating /dev/hidraw* devices")?;
    if devices.is_empty() {
        log::warn!("no exposable /dev/hidraw* devices found; receiver will get an empty list");
    } else {
        for d in &devices {
            log::info!(
                "exposing busid={} {:04x}:{:04x} hidraw={} name={:?}",
                d.summary.busid,
                d.summary.vendor_id,
                d.summary.product_id,
                d.hidraw_path.display(),
                d.summary.product
            );
        }
    }
    match recv_control(&mut sock)? {
        ControlMessage::ListDevices => {}
        other => bail!("expected LIST_DEVICES, got {other:?}"),
    }
    let summaries: Vec<DeviceSummary> = devices.iter().map(|d| d.summary.clone()).collect();
    send_control(&mut sock, &ControlMessage::DeviceList { devices: summaries })?;

    // 3. ATTACH.
    let (busid, _mode) = match recv_control(&mut sock)? {
        ControlMessage::Attach { busid, mode } => (busid, mode),
        other => bail!("expected ATTACH, got {other:?}"),
    };
    let chosen = devices
        .into_iter()
        .find(|d| d.summary.busid == busid)
        .ok_or_else(|| anyhow!("receiver asked for busid {busid:?}, we don't expose it"))?;
    send_control(
        &mut sock,
        &ControlMessage::AttachOk {
            busid: chosen.summary.busid.clone(),
            import_id: 1,
        },
    )?;

    // 4. HID Bind.
    let env = recv_envelope(&mut sock)?;
    if env.channel != Channel::Hid {
        bail!("expected HID envelope after ATTACH_OK, got {:?}", env.channel);
    }
    let bind_frame = HidFrame::parse(&env.payload)?;
    if bind_frame.op != HidOp::Bind {
        bail!("expected HID Bind, got {:?}", bind_frame.op);
    }
    let bind_req: BindRequest = serde_json::from_slice(bind_frame.body)
        .context("parsing BindRequest body")?;
    if bind_req.busid != chosen.summary.busid {
        bail!("bind busid {:?} != attached {:?}", bind_req.busid, chosen.summary.busid);
    }
    log::info!("bind requested: {:?}", bind_req);

    let bind_id = if bind_frame.bind_id == 0 { 1 } else { bind_frame.bind_id };
    let meta = BindAckMeta {
        busid: chosen.summary.busid.clone(),
        kind: kind_for(&chosen),
        vendor_id: chosen.summary.vendor_id,
        product_id: chosen.summary.product_id,
        name: chosen
            .summary
            .product
            .clone()
            .unwrap_or_else(|| format!("hidraw {}", chosen.hidraw_path.display())),
    };
    // For now we ship a textbook boot-mouse / boot-keyboard descriptor. The
    // real Android sender pulls the descriptor via libusb GET_DESCRIPTOR;
    // doing that on Linux means HIDIOCGRDESCSIZE / HIDIOCGRDESC ioctls.
    // Keeping the synthetic test on a known-good descriptor lets us assert
    // that the receiver's report decoder is the right shape.
    let descriptor = match meta.kind {
        DeviceKind::Mouse => hid_descriptor::mouse_descriptor(),
        DeviceKind::Keyboard => hid_descriptor::keyboard_descriptor(),
        _ => Vec::new(),
    };
    let body = encode_bind_ack_body(&meta, &descriptor);
    let ack = HidFrame::new(HidOp::BindAck, bind_id, 0, &body);
    sock.write_all(&hid_envelope(&ack))?;
    log::info!("bind_ack sent (bind_id={bind_id}, kind={:?})", meta.kind);

    // 5. Pump reports until socket closes or hidraw EOF.
    pump_reports(&mut sock, &chosen.hidraw_path, bind_id)?;

    // 6. Polite unbind so the receiver tears down its uinput device cleanly.
    let body = br#"{"reason":"hidraw stream ended"}"#;
    let f = HidFrame::new(HidOp::Unbind, bind_id, 0, body);
    let _ = sock.write_all(&hid_envelope(&f));
    Ok(())
}

fn kind_for(d: &ExposedHidraw) -> DeviceKind {
    // Trust the protocol/subclass we read from the USB interface descriptor
    // if it looks like boot; otherwise fall back to the descriptor classifier.
    // For our synthetic gadget the interface is HID class with subclass=1
    // (boot) and protocol=2 (mouse) so the cheap match works.
    if d.interface_class == 0x03 {
        match d.interface_protocol {
            1 => DeviceKind::Keyboard,
            2 => DeviceKind::Mouse,
            _ => DeviceKind::Other,
        }
    } else {
        DeviceKind::Other
    }
}

fn pump_reports(sock: &mut TcpStream, hidraw: &Path, bind_id: u8) -> Result<()> {
    let mut file = File::open(hidraw)
        .with_context(|| format!("opening {} for read", hidraw.display()))?;
    // Non-blocking so we can notice socket-write errors promptly; blocking
    // read would otherwise pin us until the next mouse motion.
    set_nonblocking(&file, true)?;
    let mut buf = [0u8; 64];
    let mut seq: u16 = 0;
    loop {
        match file.read(&mut buf) {
            Ok(0) => {
                log::info!("hidraw EOF; closing");
                return Ok(());
            }
            Ok(n) => {
                seq = seq.wrapping_add(1);
                let frame = HidFrame::new(HidOp::ReportIn, bind_id, seq, &buf[..n]);
                if let Err(e) = sock.write_all(&hid_envelope(&frame)) {
                    log::warn!("socket write failed: {e:#}");
                    return Ok(());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Sleep a millisecond; mouse interrupt endpoints poll at 8ms
                // (boot, 125Hz) so this is much faster than report cadence
                // and adds at most ~1ms to e2e latency.
                thread::sleep(Duration::from_millis(1));
            }
            Err(e) => return Err(e).context("reading hidraw"),
        }
    }
}

fn set_nonblocking(file: &File, on: bool) -> Result<()> {
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error()).context("F_GETFL");
    }
    let new = if on { flags | libc::O_NONBLOCK } else { flags & !libc::O_NONBLOCK };
    if unsafe { libc::fcntl(fd, libc::F_SETFL, new) } < 0 {
        return Err(std::io::Error::last_os_error()).context("F_SETFL");
    }
    Ok(())
}

// ----------------- /dev/hidraw enumeration -----------------

struct ExposedHidraw {
    summary: DeviceSummary,
    hidraw_path: PathBuf,
    interface_class: u8,
    interface_protocol: u8,
}

fn enumerate_hidraw_devices(
    only: Option<&Path>,
    filter_vidpid: Option<&str>,
) -> Result<Vec<ExposedHidraw>> {
    let filter = filter_vidpid
        .map(|s| parse_vidpid(s).context("parsing --filter-vidpid"))
        .transpose()?;

    let mut out = Vec::new();
    let mut entries: Vec<_> = fs::read_dir("/sys/class/hidraw")?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        let dev_path = PathBuf::from("/dev").join(&name);
        if let Some(restrict) = only {
            if dev_path != restrict {
                continue;
            }
        }
        let class_path = entry.path(); // /sys/class/hidraw/hidrawN
        let intf_path = fs::canonicalize(class_path.join("device"))?; // e.g. .../5-1:1.0/0003:BADD:C0DE.0003
        // The HID device dir lives inside an interface dir; walk up to find the USB device dir.
        let usb_device_path = find_usb_device_ancestor(&intf_path)?;

        let usb_uev = read_uevent(&usb_device_path.join("uevent"))?;
        let id_vendor = read_hex(&usb_device_path.join("idVendor"))?;
        let id_product = read_hex(&usb_device_path.join("idProduct"))?;
        if let Some((v, p)) = filter {
            if id_vendor != v || id_product != p {
                continue;
            }
        }
        let manufacturer = read_str(&usb_device_path.join("manufacturer")).ok();
        let product = read_str(&usb_device_path.join("product")).ok();
        let serial = read_str(&usb_device_path.join("serial")).ok();
        let device_class = read_hex_u8(&usb_device_path.join("bDeviceClass")).unwrap_or(0);
        let device_subclass = read_hex_u8(&usb_device_path.join("bDeviceSubClass")).unwrap_or(0);
        let device_protocol = read_hex_u8(&usb_device_path.join("bDeviceProtocol")).unwrap_or(0);

        // The interface dir is the parent of the HID dir; e.g.
        // /sys/devices/.../5-1/5-1:1.0/0003:BADD:C0DE.0003
        //               usbdev ^^^ ^^^^^^ interface
        let interface_path = intf_path
            .parent()
            .ok_or_else(|| anyhow!("no interface dir parent for {}", intf_path.display()))?;
        let interface_class = read_hex_u8(&interface_path.join("bInterfaceClass")).unwrap_or(0);
        let interface_protocol =
            read_hex_u8(&interface_path.join("bInterfaceProtocol")).unwrap_or(0);

        // busid: sysfs basename of the USB device dir (e.g. "5-1"). This is the
        // canonical USB/IP busid on Linux; matches what the Android sender's
        // path-derived "1-2" represents semantically (bus + device path).
        let busid = usb_device_path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .or_else(|| usb_uev.get("DEVNAME").cloned())
            .ok_or_else(|| anyhow!("no busid for {}", usb_device_path.display()))?;

        let is_hid = interface_class == 0x03;

        out.push(ExposedHidraw {
            summary: DeviceSummary {
                busid,
                vendor_id: id_vendor,
                product_id: id_product,
                manufacturer,
                product,
                serial,
                device_class,
                device_subclass,
                device_protocol,
                is_hid,
            },
            hidraw_path: dev_path,
            interface_class,
            interface_protocol,
        });
    }
    Ok(out)
}

fn find_usb_device_ancestor(start: &Path) -> Result<PathBuf> {
    // Walk up until we find a dir whose `uevent` has DEVTYPE=usb_device.
    let mut cur = start.to_path_buf();
    for _ in 0..16 {
        if let Ok(uev) = read_uevent(&cur.join("uevent")) {
            if uev.get("DEVTYPE").map(|s| s.as_str()) == Some("usb_device") {
                return Ok(cur);
            }
        }
        match cur.parent() {
            Some(p) => cur = p.to_path_buf(),
            None => break,
        }
    }
    bail!("no usb_device ancestor for {}", start.display());
}

fn read_uevent(p: &Path) -> Result<BTreeMap<String, String>> {
    let s = fs::read_to_string(p)
        .with_context(|| format!("read {}", p.display()))?;
    let mut m = BTreeMap::new();
    for line in s.lines() {
        if let Some((k, v)) = line.split_once('=') {
            m.insert(k.to_string(), v.to_string());
        }
    }
    Ok(m)
}

fn read_str(p: &Path) -> Result<String> {
    Ok(fs::read_to_string(p)
        .with_context(|| format!("read {}", p.display()))?
        .trim()
        .to_string())
}

fn read_hex(p: &Path) -> Result<u16> {
    let s = read_str(p)?;
    u16::from_str_radix(&s, 16).with_context(|| format!("parse {} = {s:?}", p.display()))
}

fn read_hex_u8(p: &Path) -> Result<u8> {
    let s = read_str(p)?;
    u8::from_str_radix(&s, 16).with_context(|| format!("parse {} = {s:?}", p.display()))
}

fn parse_vidpid(s: &str) -> Result<(u16, u16)> {
    let (v, p) = s.split_once(':').ok_or_else(|| anyhow!("expected vvvv:pppp"))?;
    Ok((u16::from_str_radix(v, 16)?, u16::from_str_radix(p, 16)?))
}
