//! zerowire-cli — Linux receiver entry point.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

use zerowire_cli::discovery::discover;
use zerowire_cli::receiver::{
    run_receive, run_simulate, run_simulate_source, run_simulate_source_with, ReceiveOpts,
};
use zerowire_cli::tls::{self as ztls, PskIdentity};
use zerowire_cli::usbip_receiver::{run_usbip_receive, UsbipReceiveOpts};
use zerowire_cli::wire::{recv_control, send_control};

use zerowire_protocol::control::{ControlMessage, DeviceSummary};

const CLIENT_NAME: &str = "zerowire-cli/0.2.0";
const SUPPORTS: &[&str] = &["usbip/1.1.1", "hid-fastlane/1"];

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ReceiveMode {
    /// HID fast lane — synthesize a virtual mouse/keyboard. v0.1 path.
    Hid,
    /// General USB passthrough via vhci-hcd (real) or transcript log
    /// (`--simulate-usbip`).
    Usbip,
}

#[derive(Parser, Debug)]
#[command(
    name = "zerowire-cli",
    about = "zerowire Linux receiver — discovery, control, HID fast lane, USB/IP",
    version
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Discover zerowire senders on the LAN via mDNS.
    Discover {
        #[arg(short, long, default_value_t = 5)]
        timeout: u64,
    },
    /// Connect to a sender by `host:port` and print its HELLO_ACK.
    Connect {
        target: String,
        #[arg(long, default_value = CLIENT_NAME)]
        name: String,
    },
    /// Connect + LIST_DEVICES; prints the device inventory.
    List { target: String },
    /// Open a session against a discovered sender.
    Receive {
        /// HID fast lane (default) or general USB/IP passthrough.
        #[arg(long, value_enum, default_value_t = ReceiveMode::Hid)]
        mode: ReceiveMode,
        /// Display name of the sender (TXT `n=`) to discover.
        #[arg(long)]
        sender: Option<String>,
        /// Skip mDNS, dial `host:port` directly.
        #[arg(long)]
        target: Option<String>,
        /// `busid` of the device on the sender to bind. Defaults to the
        /// first matching device the sender lists.
        #[arg(long)]
        busid: Option<String>,
        /// Synthesize a virtual mouse instead of dialing a sender. HID mode
        /// only.
        #[arg(long)]
        simulate: bool,
        /// HID-mode loopback: dial `--target` but write report bytes to a
        /// log file instead of pushing them through uinput. Used by
        /// tests/hid_loopback.sh.
        #[arg(long)]
        simulate_source: Option<PathBuf>,
        /// USB/IP-mode loopback: dial `--target`, walk the usbip handshake,
        /// and record the relayed URBs to this transcript file instead of
        /// touching vhci-hcd. Used by tests/usbip_loopback.sh.
        #[arg(long)]
        simulate_usbip: Option<PathBuf>,
        /// Pairing code for TLS 1.3 PSK auth. When set, the connection is
        /// wrapped in TLS using the deterministic-cert identity derived
        /// from this code. Same code must be supplied on the sender.
        #[arg(long)]
        psk: Option<String>,
        /// mDNS browse timeout in seconds.
        #[arg(long, default_value_t = 5)]
        timeout: u64,
        /// Write a JSON-line trace of every protocol event to this path.
        /// `tail -f` it during hardware bring-up. See
        /// `docs/hardware-verify.md` for the recipe.
        #[arg(long)]
        diagnose: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Discover { timeout } => cmd_discover(Duration::from_secs(timeout)),
        Cmd::Connect { target, name } => {
            let ack = cmd_connect(&target, &name)?;
            println!("{}", serde_json::to_string_pretty(&ack)?);
            Ok(())
        }
        Cmd::List { target } => {
            let devices = cmd_list(&target)?;
            print_devices(&devices);
            Ok(())
        }
        Cmd::Receive {
            mode,
            sender,
            target,
            busid,
            simulate,
            simulate_source,
            simulate_usbip,
            psk,
            timeout,
            diagnose,
        } => {
            let stop = Arc::new(AtomicBool::new(false));
            install_ctrlc(stop.clone())?;
            if simulate {
                return run_simulate(stop);
            }
            match mode {
                ReceiveMode::Hid => {
                    if let Some(log_path) = simulate_source {
                        let tgt = target.unwrap_or_else(|| "127.0.0.1:47823".to_string());
                        if psk.is_some() {
                            return run_simulate_source_maybe_tls(&tgt, &log_path, psk.as_deref());
                        }
                        if let Some(p) = &diagnose {
                            eprintln!("(diagnose log → {})", p.display());
                            return run_simulate_source_with(&tgt, &log_path, Some(p.as_path()));
                        }
                        return run_simulate_source(&tgt, &log_path);
                    }
                    if let Some(p) = &diagnose {
                        eprintln!("(diagnose log → {})", p.display());
                    }
                    run_receive(
                        ReceiveOpts {
                            sender_name: sender.unwrap_or_else(|| "Pixel".to_string()),
                            discover_timeout: Duration::from_secs(timeout),
                            busid,
                            direct_target: target,
                            client_name: CLIENT_NAME.into(),
                            diagnose_log: diagnose,
                        },
                        stop,
                    )
                }
                ReceiveMode::Usbip => {
                    let tgt = target.unwrap_or_else(|| "127.0.0.1:47823".to_string());
                    if psk.is_some() {
                        return run_usbip_receive_tls(
                            &tgt,
                            busid,
                            simulate_usbip,
                            psk.as_deref(),
                            stop,
                        );
                    }
                    run_usbip_receive(
                        UsbipReceiveOpts {
                            target: tgt,
                            busid,
                            client_name: CLIENT_NAME.into(),
                            simulate_transcript: simulate_usbip,
                        },
                        stop,
                    )
                }
            }
        }
    }
}

/// HID-loopback wrapper that optionally wraps the socket in TLS.
fn run_simulate_source_maybe_tls(
    target: &str,
    out_path: &std::path::Path,
    psk: Option<&str>,
) -> Result<()> {
    use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
    let Some(code) = psk else {
        return zerowire_cli::receiver::run_simulate_source(target, out_path);
    };
    // TLS-wrapped path: dial, handshake, then exec the v0.1 loop using the
    // generic helpers in tls.rs.
    let addr: SocketAddr = target
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("no addresses for {target}"))?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    tcp.set_nodelay(true)?;
    let identity = PskIdentity::derive(code)?;
    let cfg = ztls::client_config(&identity)?;
    let mut s = ztls::client_connect(cfg, tcp)?;
    hid_simulate_source_loop(&mut s, out_path)
}

/// USB/IP receive over TLS — small wrapper that re-implements the handshake
/// against any `Read+Write`. Kept here rather than in usbip_receiver because
/// usbip_receiver.rs uses `TcpStream` directly to support the real
/// `vhci-hcd` attach path (which needs a raw fd).
fn run_usbip_receive_tls(
    target: &str,
    busid: Option<String>,
    simulate_transcript: Option<PathBuf>,
    psk: Option<&str>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
    use zerowire_cli::usbip::SimulatedAttach;
    use zerowire_protocol::envelope::Channel;
    use zerowire_protocol::usbip::{UsbipAttachInfo, UsbipFrame};

    let code = psk.expect("only called with psk set");
    let addr: SocketAddr = target
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("no addresses for {target}"))?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    tcp.set_nodelay(true)?;
    let identity = PskIdentity::derive(code)?;
    let cfg = ztls::client_config(&identity)?;
    let mut s = ztls::client_connect(cfg, tcp)?;

    // HELLO
    ztls::send_control(
        &mut s,
        &ControlMessage::Hello {
            version: 1,
            client: CLIENT_NAME.into(),
            supports: SUPPORTS.iter().map(|s| s.to_string()).collect(),
        },
    )?;
    let _ = ztls::recv_control(&mut s)?;

    // LIST_DEVICES
    ztls::send_control(&mut s, &ControlMessage::ListDevices)?;
    let devs = match ztls::recv_control(&mut s)? {
        ControlMessage::DeviceList { devices } => devices,
        other => anyhow::bail!("expected DEVICE_LIST, got {:?}", other),
    };
    let chosen = match busid {
        Some(b) => devs
            .iter()
            .find(|d| d.busid == b)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no busid {b}"))?,
        None => devs
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("sender exposes no devices"))?,
    };
    ztls::send_control(
        &mut s,
        &ControlMessage::Attach {
            busid: chosen.busid.clone(),
            mode: "usbip".into(),
        },
    )?;
    let info: UsbipAttachInfo = match ztls::recv_control(&mut s)? {
        ControlMessage::AttachOkUsbip { info } => info,
        ControlMessage::AttachDenied { busid, reason } => {
            anyhow::bail!("denied {busid}: {reason}")
        }
        other => anyhow::bail!("unexpected reply to ATTACH: {:?}", other),
    };

    let Some(transcript) = simulate_transcript else {
        anyhow::bail!(
            "TLS usbip path currently requires --simulate-usbip; real \
             vhci-hcd attach over TLS needs sk_user_data plumbing that \
             vhci-hcd doesn't currently expose. Use --simulate-usbip to \
             exercise the TLS loop."
        );
    };
    let sim = SimulatedAttach::create(&transcript, &info)?;
    log::info!("tls+usbip simulated session started on port={}", sim.port());

    let mut frames = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let env = match ztls::recv_envelope(&mut s) {
            Ok(e) => e,
            Err(_) => break,
        };
        if env.channel == Channel::Usbip {
            let frame = UsbipFrame::parse(&env.payload)?;
            frames = frames.wrapping_add(1);
            sim.record_urb(frame.import_id, frames, frame.raw.len())?;
        }
    }
    log::info!("tls+usbip session ended ({frames} URBs relayed)");
    Ok(())
}

/// The HID `simulate-source` loop, re-implemented against a generic
/// Read+Write so it can run over a TLS stream too. This is a literal
/// translation of `receiver::run_simulate_source` minus the dial step.
fn hid_simulate_source_loop<S: std::io::Read + std::io::Write>(
    s: &mut S,
    out_path: &std::path::Path,
) -> Result<()> {
    use std::io::Write;
    use zerowire_protocol::envelope::Channel;
    use zerowire_protocol::hid::{parse_bind_ack_body, BindRequest, HidFrame, HidOp};

    ztls::send_control(
        s,
        &ControlMessage::Hello {
            version: 1,
            client: "zerowire-test/0".into(),
            supports: vec!["hid-fastlane/1".into()],
        },
    )?;
    match ztls::recv_control(s)? {
        ControlMessage::HelloAck { .. } => {}
        other => anyhow::bail!("HELLO_ACK expected, got {:?}", other),
    }
    ztls::send_control(s, &ControlMessage::ListDevices)?;
    let devs = match ztls::recv_control(s)? {
        ControlMessage::DeviceList { devices } => devices,
        other => anyhow::bail!("DEVICE_LIST expected, got {:?}", other),
    };
    let chosen = devs
        .iter()
        .find(|d| d.is_hid)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no HID device"))?;
    ztls::send_control(
        s,
        &ControlMessage::Attach {
            busid: chosen.busid.clone(),
            mode: "hid".into(),
        },
    )?;
    match ztls::recv_control(s)? {
        ControlMessage::AttachOk { .. } => {}
        other => anyhow::bail!("ATTACH_OK expected, got {:?}", other),
    }
    let bind_json = serde_json::to_vec(&BindRequest {
        busid: chosen.busid.clone(),
        want: "input".into(),
    })?;
    let bind = HidFrame::new(HidOp::Bind, 1, 0, &bind_json);
    let env = zerowire_protocol::envelope::Envelope::new(Channel::Hid, &bind.encode()).encode()?;
    s.write_all(&env)?;

    let mut log = std::fs::File::create(out_path)?;
    let mut got_bind_ack = false;
    let mut report_count = 0u32;
    loop {
        let env = match ztls::recv_envelope(s) {
            Ok(e) => e,
            Err(_) => break,
        };
        if env.channel != Channel::Hid {
            continue;
        }
        let f = HidFrame::parse(&env.payload)?;
        match f.op {
            HidOp::BindAck => {
                let (meta, _rd) = parse_bind_ack_body(f.body)?;
                writeln!(log, "bind_ack kind={:?} name={:?}", meta.kind, meta.name)?;
                got_bind_ack = true;
            }
            HidOp::ReportIn => {
                writeln!(log, "report seq={} bytes={:?}", f.seq, f.body)?;
                report_count += 1;
            }
            HidOp::Unbind => {
                writeln!(log, "unbind")?;
                break;
            }
            _ => {}
        }
    }
    writeln!(log, "summary bind_ack={} reports={}", got_bind_ack, report_count)?;
    Ok(())
}

fn install_ctrlc(flag: Arc<AtomicBool>) -> Result<()> {
    use std::os::raw::c_int;
    extern "C" fn handler(_sig: c_int) {
        FLAG.store(true, Ordering::SeqCst);
    }
    static FLAG: AtomicBool = AtomicBool::new(false);
    std::thread::spawn(move || loop {
        if FLAG.load(Ordering::Relaxed) {
            flag.store(true, Ordering::SeqCst);
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    });
    unsafe {
        libc::signal(libc::SIGINT, handler as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handler as *const () as libc::sighandler_t);
    }
    Ok(())
}

fn cmd_discover(timeout: Duration) -> Result<()> {
    eprintln!("(discovering _zerowire._tcp.local. for {:?} ...)", timeout);
    let found = discover(timeout)?;
    if found.is_empty() {
        println!("(no senders found)");
    } else {
        for s in &found {
            println!(
                "{} @ {}:{}  id={}  v={}  caps={}",
                s.name,
                s.host,
                s.port,
                s.sender_id,
                s.version,
                s.capabilities.join(",")
            );
        }
    }
    Ok(())
}

fn cmd_connect(target: &str, client_name: &str) -> Result<ControlMessage> {
    use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
    let addr: SocketAddr = target
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("no addresses for {target}"))?;
    let mut sock = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    sock.set_read_timeout(Some(Duration::from_secs(10)))?;
    sock.set_write_timeout(Some(Duration::from_secs(10)))?;
    sock.set_nodelay(true)?;
    send_control(
        &mut sock,
        &ControlMessage::Hello {
            version: 1,
            client: client_name.into(),
            supports: SUPPORTS.iter().map(|s| s.to_string()).collect(),
        },
    )?;
    Ok(recv_control(&mut sock)?)
}

fn cmd_list(target: &str) -> Result<Vec<DeviceSummary>> {
    use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
    let addr: SocketAddr = target
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("no addresses for {target}"))?;
    let mut sock = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    sock.set_read_timeout(Some(Duration::from_secs(10)))?;
    sock.set_write_timeout(Some(Duration::from_secs(10)))?;
    sock.set_nodelay(true)?;
    send_control(
        &mut sock,
        &ControlMessage::Hello {
            version: 1,
            client: CLIENT_NAME.into(),
            supports: SUPPORTS.iter().map(|s| s.to_string()).collect(),
        },
    )?;
    let _ack = recv_control(&mut sock)?;
    send_control(&mut sock, &ControlMessage::ListDevices)?;
    match recv_control(&mut sock)? {
        ControlMessage::DeviceList { devices } => Ok(devices),
        other => anyhow::bail!("expected DEVICE_LIST, got {:?}", other),
    }
}

fn print_devices(devices: &[DeviceSummary]) {
    if devices.is_empty() {
        println!("(no devices exposed by sender)");
        return;
    }
    println!("{:<10}  {:<10}  {:<4}  {}", "busid", "vid:pid", "hid", "name");
    println!("{:-<10}  {:-<10}  {:-<4}  {:-<30}", "", "", "", "");
    for d in devices {
        println!(
            "{:<10}  {:04x}:{:04x}   {:<4}  {}",
            d.busid,
            d.vendor_id,
            d.product_id,
            if d.is_hid { "yes" } else { "no" },
            d.product.clone().unwrap_or_else(|| "<unnamed>".into()),
        );
    }
}
