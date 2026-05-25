//! `zerowire receive` — the end-to-end HID-fast-lane glue.
//!
//! Flow per session:
//!
//! 1. mDNS-discover senders, pick one by name, dial it.
//! 2. Send `Hello`, expect `HelloAck`.
//! 3. `ListDevices` → pick the first `is_hid` device (or honour `--busid`).
//! 4. `Attach { mode = "hid" }` → expect `AttachOk`.
//! 5. Open a `HidOp::Bind { busid, want: "input" }` frame on the HID channel.
//! 6. Receive `BindAck` carrying `BindAckMeta` + report descriptor.
//! 7. Create a virtual mouse / keyboard via `uinput` based on `BindAckMeta.kind`.
//! 8. Loop on `HidOp::ReportIn` packets, decode the boot-mouse or boot-keyboard
//!    report, push the appropriate uinput events. `Unbind` or socket close ⇒
//!    tear down.
//!
//! Simulation mode (`--simulate`) skips everything from step 1 onwards and just
//! synthesizes mouse jitter into a real `uinput` device. Useful when you want
//! to confirm the receiver wiring without a phone in the room.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;
use zerowire_protocol::{
    control::{ControlMessage, DeviceSummary},
    envelope::{Channel, Envelope, HEADER_LEN},
    hid::{
        encode_bind_ack_body, parse_bind_ack_body, BindAckMeta, BindRequest, DeviceKind, HidFrame,
        HidOp,
    },
};

use crate::discovery::{discover, pick_by_name};
use crate::uinput::{
    DeviceProfile, UinputDevice, BTN_EXTRA, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, BTN_SIDE,
};
use crate::wire::{recv_control, recv_envelope, send_control, send_envelope};

/// Options for `run_receive`.
pub struct ReceiveOpts {
    pub sender_name: String,
    pub discover_timeout: Duration,
    pub busid: Option<String>,
    /// When set, also dial a TCP target directly instead of going via mDNS.
    pub direct_target: Option<String>,
    pub client_name: String,
    /// When set, write a JSON-line trace of every protocol event to this
    /// path. Each line is one event; safe to `tail -f`. Used during
    /// hardware bring-up (see docs/hardware-verify.md).
    pub diagnose_log: Option<std::path::PathBuf>,
}

/// Drive a full receive session against a real sender.
pub fn run_receive(opts: ReceiveOpts, stop: Arc<AtomicBool>) -> Result<()> {
    let mut diag = DiagnoseLog::open(opts.diagnose_log.as_deref())?;
    diag.event("start", json!({ "sender_name": opts.sender_name, "direct_target": opts.direct_target }));

    let target = if let Some(t) = &opts.direct_target {
        t.clone()
    } else {
        log::info!(
            "discovering sender {:?} (timeout {:?})...",
            opts.sender_name,
            opts.discover_timeout
        );
        let senders = discover(opts.discover_timeout)?;
        if senders.is_empty() {
            diag.event("discover_failed", json!({}));
            bail!("no zerowire senders found on the LAN");
        }
        diag.event(
            "discover_ok",
            json!({
                "count": senders.len(),
                "senders": senders.iter().map(|s| {
                    json!({ "name": s.name, "host": s.host, "port": s.port, "caps": s.capabilities })
                }).collect::<Vec<_>>(),
            }),
        );
        let pick = pick_by_name(&senders, &opts.sender_name)
            .ok_or_else(|| anyhow!("no sender matched name {:?}", opts.sender_name))?;
        format!("{}:{}", pick.host, pick.port)
    };
    log::info!("dialing {}", target);
    diag.event("dial", json!({ "target": target }));
    let mut sock = dial(&target)?;
    diag.event("dial_ok", json!({}));

    // 1. Handshake.
    send_control(
        &mut sock,
        &ControlMessage::Hello {
            version: 1,
            client: opts.client_name.clone(),
            supports: vec!["hid-fastlane/1".into()],
        },
    )?;
    match recv_control(&mut sock)? {
        ControlMessage::HelloAck { name, sender_id, supports } => {
            log::info!("handshake ok with {}", name);
            diag.event("hello_ack", json!({ "name": name, "sender_id": sender_id, "supports": supports }));
        }
        ControlMessage::Error { code, message } => {
            diag.event("hello_error", json!({ "code": code, "message": message.clone() }));
            bail!("sender refused: {code}: {message}");
        }
        other => bail!("unexpected reply to HELLO: {:?}", other),
    }

    // 2. LIST_DEVICES → pick busid.
    send_control(&mut sock, &ControlMessage::ListDevices)?;
    let devices: Vec<DeviceSummary> = match recv_control(&mut sock)? {
        ControlMessage::DeviceList { devices } => devices,
        other => bail!("expected DEVICE_LIST, got {:?}", other),
    };
    diag.event(
        "device_list",
        json!({
            "count": devices.len(),
            "devices": devices.iter().map(|d| json!({
                "busid": d.busid,
                "vendor_id": format!("{:04x}", d.vendor_id),
                "product_id": format!("{:04x}", d.product_id),
                "product": d.product,
                "is_hid": d.is_hid,
            })).collect::<Vec<_>>(),
        }),
    );
    let chosen = pick_hid_device(&devices, opts.busid.as_deref())?;
    log::info!("attaching busid={} ({})", chosen.busid, chosen.product.clone().unwrap_or_default());
    diag.event("attach_request", json!({ "busid": chosen.busid, "mode": "hid" }));

    // 3. ATTACH (hid mode).
    send_control(
        &mut sock,
        &ControlMessage::Attach {
            busid: chosen.busid.clone(),
            mode: "hid".into(),
        },
    )?;
    match recv_control(&mut sock)? {
        ControlMessage::AttachOk { busid, import_id } => {
            diag.event("attach_ok", json!({ "busid": busid, "import_id": import_id }));
        }
        ControlMessage::AttachDenied { busid, reason } => {
            diag.event("attach_denied", json!({ "busid": busid.clone(), "reason": reason.clone() }));
            bail!("sender denied attach for {busid}: {reason}")
        }
        other => bail!("unexpected reply to ATTACH: {:?}", other),
    }

    // 4. HID Bind.
    let bind_id = 1u8;
    let bind_req = BindRequest {
        busid: chosen.busid.clone(),
        want: "input".into(),
    };
    let bind_json = serde_json::to_vec(&bind_req)?;
    let bind_frame = HidFrame::new(HidOp::Bind, bind_id, 0, &bind_json);
    send_envelope(&mut sock, Channel::Hid, &bind_frame.encode())?;

    // Loop until we either get a BindAck, the socket dies, or stop is set.
    let mut device: Option<UinputDevice> = None;
    let mut last_seq: u16 = 0;
    let mut reports_seen: u64 = 0;
    let session_start = std::time::Instant::now();
    let mut last_report_log = session_start;
    diag.event("bind_sent", json!({ "bind_id": bind_id, "busid": chosen.busid }));

    while !stop.load(Ordering::Relaxed) {
        let env = match recv_envelope(&mut sock) {
            Ok(e) => e,
            Err(e) => {
                log::warn!("recv error: {e:#}");
                break;
            }
        };
        match env.channel {
            Channel::Keepalive => continue,
            Channel::Control => {
                if let Ok(m) = ControlMessage::from_json(&env.payload) {
                    log::debug!("control while bound: {:?}", m);
                }
                continue;
            }
            Channel::Hid => {
                let frame = HidFrame::parse(&env.payload).context("parsing HID frame")?;
                match frame.op {
                    HidOp::BindAck => {
                        let (meta, descriptor) = parse_bind_ack_body(frame.body)
                            .context("parsing BindAck body")?;
                        log::info!(
                            "bind_ack: kind={:?} name={:?} rd_len={}",
                            meta.kind,
                            meta.name,
                            descriptor.len()
                        );
                        diag.event(
                            "bind_ack",
                            json!({
                                "kind": format!("{:?}", meta.kind),
                                "name": meta.name,
                                "vendor_id": format!("{:04x}", meta.vendor_id),
                                "product_id": format!("{:04x}", meta.product_id),
                                "report_descriptor_len": descriptor.len(),
                            }),
                        );
                        device = Some(open_device(&meta)?);
                        diag.event(
                            "uinput_created",
                            json!({ "name": format!("zerowire: {}", meta.name) }),
                        );
                    }
                    HidOp::ReportIn => {
                        if frame.seq != last_seq.wrapping_add(1) && last_seq != 0 {
                            log::debug!(
                                "seq jump: {} -> {} (drops or reorder)",
                                last_seq,
                                frame.seq
                            );
                            diag.event(
                                "seq_jump",
                                json!({ "prev": last_seq, "got": frame.seq }),
                            );
                        }
                        last_seq = frame.seq;
                        if let Some(d) = device.as_mut() {
                            apply_report(d, frame.body)?;
                            reports_seen += 1;
                            // First few reports get full byte dumps; after
                            // that we summarize once a second so the diag
                            // log doesn't explode under a 1kHz gaming mouse.
                            let now = std::time::Instant::now();
                            if reports_seen <= 5 {
                                diag.event(
                                    "report",
                                    json!({
                                        "seq": frame.seq,
                                        "len": frame.body.len(),
                                        "bytes": frame.body,
                                    }),
                                );
                            } else if now.duration_since(last_report_log)
                                >= std::time::Duration::from_secs(1)
                            {
                                let elapsed = now.duration_since(session_start).as_secs_f64();
                                let rate = (reports_seen as f64) / elapsed.max(0.001);
                                diag.event(
                                    "report_rate",
                                    json!({
                                        "reports_total": reports_seen,
                                        "elapsed_s": elapsed,
                                        "reports_per_s": rate,
                                        "last_seq": frame.seq,
                                        "last_len": frame.body.len(),
                                    }),
                                );
                                last_report_log = now;
                            }
                        } else {
                            log::warn!("report before bind_ack; ignoring");
                            diag.event("report_before_bind_ack", json!({ "seq": frame.seq }));
                        }
                    }
                    HidOp::Unbind => {
                        log::info!("sender unbound: {}", String::from_utf8_lossy(frame.body));
                        diag.event("unbind", json!({ "from_sender": true, "reports_seen": reports_seen }));
                        break;
                    }
                    other => {
                        log::debug!("unhandled HID op {:?}", other);
                        diag.event("unhandled_hid_op", json!({ "op": format!("{:?}", other) }));
                    }
                }
            }
            Channel::Usbip => log::debug!("ignoring USB/IP envelope in HID receiver"),
        }
    }

    // explicit drop so we destroy the uinput device before the socket
    drop(device);
    Ok(())
}

fn pick_hid_device(devs: &[DeviceSummary], busid: Option<&str>) -> Result<DeviceSummary> {
    if let Some(b) = busid {
        return devs
            .iter()
            .find(|d| d.busid == b)
            .cloned()
            .ok_or_else(|| anyhow!("sender does not expose busid {b}"));
    }
    devs.iter()
        .find(|d| d.is_hid)
        .cloned()
        .ok_or_else(|| anyhow!("sender exposes no HID device; pass --busid to override"))
}

fn dial(target: &str) -> Result<TcpStream> {
    let addr: SocketAddr = target
        .to_socket_addrs()
        .with_context(|| format!("resolving {target}"))?
        .next()
        .ok_or_else(|| anyhow!("no addresses for {target}"))?;
    let sock = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .with_context(|| format!("connecting to {addr}"))?;
    // Reads on the session loop block indefinitely; per-op timeouts are
    // enforced by the keepalive logic, not the socket.
    sock.set_nodelay(true)?;
    Ok(sock)
}

fn open_device(meta: &BindAckMeta) -> Result<UinputDevice> {
    let profile = match meta.kind {
        DeviceKind::Mouse => DeviceProfile::Mouse {
            name: format!("zerowire: {}", meta.name),
            vendor_id: meta.vendor_id,
            product_id: meta.product_id,
        },
        DeviceKind::Keyboard => DeviceProfile::Keyboard {
            name: format!("zerowire: {}", meta.name),
            vendor_id: meta.vendor_id,
            product_id: meta.product_id,
        },
        DeviceKind::Gamepad | DeviceKind::Other => {
            bail!(
                "device kind {:?} is not yet supported on the Linux receiver; \
                 file an issue with the report descriptor",
                meta.kind
            )
        }
    };
    UinputDevice::create(&profile)
}

/// Decode one HID input report and push it through `device`.
///
/// We only support the **boot** report formats for now:
///
/// * Mouse: `[buttons:u8, dx:i8, dy:i8, wheel:i8 (optional)]`
/// * Keyboard: `[modifier:u8, reserved:u8, key0..key5:u8]`
///
/// Non-boot devices (e.g. high-DPI mice that emit 16-bit deltas, gaming
/// keyboards with NKRO blobs) will need their report descriptor parsed; that
/// lands in a follow-up.
fn apply_report(device: &mut UinputDevice, body: &[u8]) -> Result<()> {
    if body.is_empty() {
        return Ok(());
    }
    // Heuristic: 3- or 4-byte reports starting with a button bitmap are mice;
    // 8-byte reports are boot keyboards. The sender is supposed to keep the
    // shape consistent within a binding, so this lookup is cheap.
    if body.len() <= 5 {
        return apply_mouse_report(device, body);
    }
    if body.len() == 8 {
        return apply_keyboard_report(device, body);
    }
    // unknown shape: best-effort try mouse
    apply_mouse_report(device, body)
}

fn apply_mouse_report(device: &mut UinputDevice, body: &[u8]) -> Result<()> {
    let buttons = body[0];
    let dx = if body.len() >= 2 { body[1] as i8 as i32 } else { 0 };
    let dy = if body.len() >= 3 { body[2] as i8 as i32 } else { 0 };
    let wheel = if body.len() >= 4 { body[3] as i8 as i32 } else { 0 };

    // We track edges by maintaining the last-known button bitmap in a hidden
    // static (per-process) — fine for one bound device. If we ever bind
    // multiple mice we'll move this onto the receiver session.
    static mut PREV: u8 = 0;
    // Safety: single-threaded receiver session per process today.
    let prev = unsafe { PREV };
    let edges = prev ^ buttons;
    let press_or_release = |i: u8, code: u16, device: &mut UinputDevice| -> Result<()> {
        if edges & (1 << i) != 0 {
            device.mouse_button(code, buttons & (1 << i) != 0)?;
        }
        Ok(())
    };
    press_or_release(0, BTN_LEFT, device)?;
    press_or_release(1, BTN_RIGHT, device)?;
    press_or_release(2, BTN_MIDDLE, device)?;
    press_or_release(3, BTN_SIDE, device)?;
    press_or_release(4, BTN_EXTRA, device)?;
    unsafe {
        PREV = buttons;
    }

    if dx != 0 || dy != 0 {
        device.mouse_move(dx, dy)?;
    }
    if wheel != 0 {
        device.mouse_wheel(wheel, 0)?;
    }
    Ok(())
}

fn apply_keyboard_report(device: &mut UinputDevice, body: &[u8]) -> Result<()> {
    // Modifier bits → Linux KEY_* codes.
    const KEY_LEFTCTRL: u16 = 29;
    const KEY_LEFTSHIFT: u16 = 42;
    const KEY_LEFTALT: u16 = 56;
    const KEY_LEFTMETA: u16 = 125;
    const KEY_RIGHTCTRL: u16 = 97;
    const KEY_RIGHTSHIFT: u16 = 54;
    const KEY_RIGHTALT: u16 = 100;
    const KEY_RIGHTMETA: u16 = 126;

    static mut PREV_MOD: u8 = 0;
    static mut PREV_KEYS: [u8; 6] = [0; 6];
    let modifier = body[0];
    let keys = [body[2], body[3], body[4], body[5], body[6], body[7]];
    let (prev_mod, prev_keys) = unsafe { (PREV_MOD, PREV_KEYS) };

    let modifier_bits = [
        (0, KEY_LEFTCTRL),
        (1, KEY_LEFTSHIFT),
        (2, KEY_LEFTALT),
        (3, KEY_LEFTMETA),
        (4, KEY_RIGHTCTRL),
        (5, KEY_RIGHTSHIFT),
        (6, KEY_RIGHTALT),
        (7, KEY_RIGHTMETA),
    ];
    for (bit, code) in modifier_bits {
        let was = prev_mod & (1 << bit) != 0;
        let now = modifier & (1 << bit) != 0;
        if was != now {
            device.key(code, now)?;
        }
    }

    // Released keys: present in prev_keys but not in keys.
    for &k in &prev_keys {
        if k != 0 && !keys.contains(&k) {
            if let Some(code) = hid_usage_to_linux_key(k) {
                device.key(code, false)?;
            }
        }
    }
    // Newly pressed keys.
    for &k in &keys {
        if k != 0 && !prev_keys.contains(&k) {
            if let Some(code) = hid_usage_to_linux_key(k) {
                device.key(code, true)?;
            }
        }
    }
    unsafe {
        PREV_MOD = modifier;
        PREV_KEYS = keys;
    }
    Ok(())
}

/// HID Usage ID (Keyboard/Keypad page, 0x07) → Linux `KEY_*` code.
///
/// Covers the printable ASCII + common navigation block. Missing entries are
/// fine — they just don't fire — and can be filled in as needed.
fn hid_usage_to_linux_key(usage: u8) -> Option<u16> {
    // From `linux/input-event-codes.h` and HID Usage Tables §10 (Keyboard).
    let code = match usage {
        0x04..=0x1D => 30 + (usage - 0x04) as u16, // A-Z → KEY_A..KEY_Z (a few off; see below)
        // a..z map: 0x04=A=KEY_A(30), 0x05=B=KEY_B(48)... NOT linear!
        // override the lazy formula:
        _ => 0,
    };
    if code != 0 {
        return Some(code_for_letter(usage).unwrap_or(code));
    }
    Some(match usage {
        0x1E => 2,  // 1
        0x1F => 3,  // 2
        0x20 => 4,  // 3
        0x21 => 5,  // 4
        0x22 => 6,  // 5
        0x23 => 7,  // 6
        0x24 => 8,  // 7
        0x25 => 9,  // 8
        0x26 => 10, // 9
        0x27 => 11, // 0
        0x28 => 28, // Enter
        0x29 => 1,  // Esc
        0x2A => 14, // Backspace
        0x2B => 15, // Tab
        0x2C => 57, // Space
        0x2D => 12, // -
        0x2E => 13, // =
        0x2F => 26, // [
        0x30 => 27, // ]
        0x31 => 43, // \
        0x33 => 39, // ;
        0x34 => 40, // '
        0x35 => 41, // `
        0x36 => 51, // ,
        0x37 => 52, // .
        0x38 => 53, // /
        0x39 => 58, // CapsLock
        0x3A..=0x45 => 59 + (usage - 0x3A) as u16, // F1..F12
        0x4F => 106, // Right
        0x50 => 105, // Left
        0x51 => 108, // Down
        0x52 => 103, // Up
        _ => return None,
    })
}

/// Letter usage IDs to KEY_* codes (the alphabet is not contiguous in Linux's
/// input-event codes: it follows QWERTY row order).
fn code_for_letter(usage: u8) -> Option<u16> {
    // KEY_A=30, KEY_B=48, KEY_C=46, KEY_D=32, KEY_E=18, KEY_F=33, KEY_G=34,
    // KEY_H=35, KEY_I=23, KEY_J=36, KEY_K=37, KEY_L=38, KEY_M=50, KEY_N=49,
    // KEY_O=24, KEY_P=25, KEY_Q=16, KEY_R=19, KEY_S=31, KEY_T=20, KEY_U=22,
    // KEY_V=47, KEY_W=17, KEY_X=45, KEY_Y=21, KEY_Z=44.
    let map: [u16; 26] = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47,
        17, 45, 21, 44,
    ];
    if (0x04..=0x1D).contains(&usage) {
        Some(map[(usage - 0x04) as usize])
    } else {
        None
    }
}

// ---------------- Simulate mode ----------------

/// Synthesize fake mouse movement into a real uinput device. No network at all.
/// Drawing a slow horizontal sine wave is enough for a human to eyeball.
pub fn run_simulate(stop: Arc<AtomicBool>) -> Result<()> {
    let profile = DeviceProfile::Mouse {
        name: "zerowire: simulated mouse".into(),
        vendor_id: 0xBADD,
        product_id: 0xC0DE,
    };
    log::info!(
        "creating virtual mouse via /dev/uinput \
         (root or `uinput` group required)"
    );
    let mut device = UinputDevice::create(&profile)?;
    log::info!("simulating mouse motion; Ctrl-C to stop");
    let mut tick = 0i64;
    while !stop.load(Ordering::Relaxed) {
        let t = (tick as f64) * 0.05;
        let dx = (t.sin() * 5.0) as i32;
        let dy = (t.cos() * 2.0) as i32;
        if dx != 0 || dy != 0 {
            device.mouse_move(dx, dy)?;
        }
        tick += 1;
        std::thread::sleep(Duration::from_millis(20));
    }
    log::info!("stop requested; destroying virtual device");
    device.destroy()
}

// ---------------- Mock-source mode (no uinput, for CI) ----------------

/// Stand-in for `run_receive` when you don't have permission to open
/// `/dev/uinput`. Connects to a TCP target (typically the mock sender),
/// completes the handshake + bind, and writes received report bytes to
/// `out_path` instead of pushing them through uinput. The integration test
/// asserts the file content.
pub fn run_simulate_source(target: &str, out_path: &std::path::Path) -> Result<()> {
    run_simulate_source_with(target, out_path, None)
}

/// Same as `run_simulate_source` but also writes a JSON-line trace to
/// `diagnose_log` when set. Used by hardware-verify scripts that want both
/// the legacy text log (for asserts) and the diagnostic stream.
pub fn run_simulate_source_with(
    target: &str,
    out_path: &std::path::Path,
    diagnose_log: Option<&std::path::Path>,
) -> Result<()> {
    let mut diag = DiagnoseLog::open(diagnose_log)?;
    diag.event("start", json!({ "target": target, "simulate_source": out_path }));
    let mut sock = dial(target)?;
    diag.event("dial_ok", json!({ "target": target }));

    send_control(
        &mut sock,
        &ControlMessage::Hello {
            version: 1,
            client: "zerowire-test/0".into(),
            supports: vec!["hid-fastlane/1".into()],
        },
    )?;
    match recv_control(&mut sock)? {
        ControlMessage::HelloAck { name, sender_id, supports } => {
            diag.event("hello_ack", json!({ "name": name, "sender_id": sender_id, "supports": supports }));
        }
        other => bail!("HELLO_ACK expected, got {:?}", other),
    }
    send_control(&mut sock, &ControlMessage::ListDevices)?;
    let devs = match recv_control(&mut sock)? {
        ControlMessage::DeviceList { devices } => devices,
        other => bail!("DEVICE_LIST expected, got {:?}", other),
    };
    diag.event(
        "device_list",
        json!({
            "count": devs.len(),
            "devices": devs.iter().map(|d| json!({
                "busid": d.busid, "is_hid": d.is_hid, "product": d.product,
                "vendor_id": format!("{:04x}", d.vendor_id),
                "product_id": format!("{:04x}", d.product_id),
            })).collect::<Vec<_>>(),
        }),
    );
    let chosen = pick_hid_device(&devs, None)?;
    diag.event("attach_request", json!({ "busid": chosen.busid, "mode": "hid" }));
    send_control(
        &mut sock,
        &ControlMessage::Attach {
            busid: chosen.busid.clone(),
            mode: "hid".into(),
        },
    )?;
    match recv_control(&mut sock)? {
        ControlMessage::AttachOk { busid, import_id } => {
            diag.event("attach_ok", json!({ "busid": busid, "import_id": import_id }));
        }
        other => bail!("ATTACH_OK expected, got {:?}", other),
    }
    let bind_json = serde_json::to_vec(&BindRequest {
        busid: chosen.busid.clone(),
        want: "input".into(),
    })?;
    let bind = HidFrame::new(HidOp::Bind, 1, 0, &bind_json);
    send_envelope(&mut sock, Channel::Hid, &bind.encode())?;

    let mut log = std::fs::File::create(out_path)?;
    use std::io::Write;
    let mut got_bind_ack = false;
    let mut report_count = 0u32;
    // Read until socket closes or sender sends Unbind.
    loop {
        let env = match recv_envelope(&mut sock) {
            Ok(e) => e,
            Err(_) => break,
        };
        if env.channel != Channel::Hid {
            continue;
        }
        let f = HidFrame::parse(&env.payload)?;
        match f.op {
            HidOp::BindAck => {
                let (meta, rd) = parse_bind_ack_body(f.body)?;
                writeln!(log, "bind_ack kind={:?} name={:?}", meta.kind, meta.name)?;
                diag.event(
                    "bind_ack",
                    json!({
                        "kind": format!("{:?}", meta.kind),
                        "name": meta.name,
                        "report_descriptor_len": rd.len(),
                    }),
                );
                got_bind_ack = true;
            }
            HidOp::ReportIn => {
                writeln!(log, "report seq={} bytes={:?}", f.seq, f.body)?;
                if report_count < 5 {
                    diag.event("report", json!({ "seq": f.seq, "bytes": f.body }));
                }
                report_count += 1;
            }
            HidOp::Unbind => {
                writeln!(log, "unbind")?;
                diag.event("unbind", json!({ "from_sender": true, "reports_seen": report_count }));
                break;
            }
            _ => {}
        }
    }
    writeln!(log, "summary bind_ack={} reports={}", got_bind_ack, report_count)?;
    diag.event("summary", json!({ "bind_ack": got_bind_ack, "reports": report_count }));
    Ok(())
}

// ---------------- helpers exposed for mock-sender ----------------

/// Build a `BindAck` body for a synthetic mouse. Used by the mock sender and
/// by integration tests.
pub fn synth_mouse_bind_ack_body(busid: &str) -> Vec<u8> {
    let meta = BindAckMeta {
        busid: busid.into(),
        kind: DeviceKind::Mouse,
        vendor_id: 0xBADD,
        product_id: 0xC0DE,
        name: "zerowire-mock mouse".into(),
    };
    encode_bind_ack_body(&meta, &crate::hid_descriptor::mouse_descriptor())
}

/// Convenience for the mock sender / loopback test: encode a HID envelope.
pub fn hid_envelope(frame: &HidFrame<'_>) -> Vec<u8> {
    let body = frame.encode();
    Envelope::new(Channel::Hid, &body).encode().expect("HID envelope encodes")
}

// re-export so test code only needs to depend on this crate
pub use zerowire_protocol::envelope::HEADER_LEN as ENVELOPE_HEADER_LEN;

#[allow(unused_imports)]
use std::io::Write as _;

/// JSON-line diagnostic trace for `--diagnose`. Each call writes one line:
///
/// ```json
/// {"ts":"...","event":"hello_ack","data":{...}}
/// ```
///
/// `None` path → no-op (so the hot loop stays cheap when diagnose is off).
struct DiagnoseLog {
    file: Option<std::fs::File>,
}

impl DiagnoseLog {
    fn open(path: Option<&std::path::Path>) -> Result<Self> {
        let file = match path {
            None => None,
            Some(p) => Some(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)
                    .with_context(|| format!("opening diagnose log {}", p.display()))?,
            ),
        };
        let me = Self { file };
        Ok(me)
    }

    fn event(&mut self, name: &str, data: serde_json::Value) {
        let Some(f) = self.file.as_mut() else { return };
        let line = json!({
            "ts": chrono_like_now(),
            "event": name,
            "data": data,
        });
        // Best-effort; diagnostic logging must never panic the session.
        let _ = writeln!(f, "{}", line);
        let _ = f.flush();
    }
}

/// Cheap UTC timestamp without pulling in chrono. Format: RFC3339-ish,
/// `1970-01-01T00:00:00.123456Z`. Good enough for human-readable trace.
fn chrono_like_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let micros = now.subsec_micros();
    // Break down into Y-M-D H:M:S via libc (we already depend on libc).
    let mut tm = libc::tm {
        tm_sec: 0, tm_min: 0, tm_hour: 0, tm_mday: 0, tm_mon: 0, tm_year: 0,
        tm_wday: 0, tm_yday: 0, tm_isdst: 0, tm_gmtoff: 0, tm_zone: std::ptr::null(),
    };
    unsafe { libc::gmtime_r(&secs as *const i64 as *const libc::time_t, &mut tm); }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        micros,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_usage_q_maps_to_key_q() {
        // 0x14 (Q) → KEY_Q = 16
        assert_eq!(hid_usage_to_linux_key(0x14), Some(16));
    }

    #[test]
    fn hid_usage_space_maps_to_key_space() {
        assert_eq!(hid_usage_to_linux_key(0x2C), Some(57));
    }

    #[test]
    fn hid_usage_unknown_returns_none() {
        assert_eq!(hid_usage_to_linux_key(0xFF), None);
    }
}

const _: usize = HEADER_LEN; // ensure import used in non-test builds
