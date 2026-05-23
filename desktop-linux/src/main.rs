//! zerowire — Linux receiver CLI (walking skeleton).
//!
//! ```text
//!   zerowire-cli discover [--timeout 5]
//!   zerowire-cli connect  <host:port> [--name <client-name>]
//!   zerowire-cli list     <host:port>          # discover + handshake + LIST_DEVICES
//! ```
//!
//! What's *not* here yet (deliberately — see ARCHITECTURE.md §6.2):
//!   * TLS / PSK auth   (current handshake is plaintext for the skeleton)
//!   * `vhci-hcd` attach (we just print what we'd attach)
//!   * Tray UI / daemon

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use mdns_sd::{ServiceDaemon, ServiceEvent};

use zerowire_protocol::{
    control::{ControlMessage, DeviceSummary},
    discovery::{ResolvedSender, SERVICE_TYPE},
    envelope::{Channel, Envelope, HEADER_LEN},
};

const CLIENT_NAME: &str = "zerowire-cli/0.1.0";
const SUPPORTS: &[&str] = &["usbip/1.1.1", "hid-fastlane/1"];

#[derive(Parser, Debug)]
#[command(
    name = "zerowire-cli",
    about = "zerowire Linux receiver — discovery + handshake walking skeleton",
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
        /// Seconds to listen before giving up.
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
            if devices.is_empty() {
                println!("(no devices exposed by sender)");
            } else {
                println!("{:<10}  {:<10}  {:<4}  {}", "busid", "vid:pid", "hid", "name");
                println!("{:-<10}  {:-<10}  {:-<4}  {:-<30}", "", "", "", "");
                for d in &devices {
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
            Ok(())
        }
    }
}

// ---------------- discover ----------------

fn cmd_discover(timeout: Duration) -> Result<()> {
    let mdns = ServiceDaemon::new().context("starting mDNS daemon")?;
    let rx = mdns
        .browse(SERVICE_TYPE)
        .context("starting mDNS browse")?;
    eprintln!("(discovering {} for {:?} ...)", SERVICE_TYPE, timeout);

    let deadline = std::time::Instant::now() + timeout;
    let mut found: Vec<ResolvedSender> = Vec::new();
    while let Ok(evt) = rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
        if let ServiceEvent::ServiceResolved(info) = evt {
            let txt: std::collections::HashMap<String, String> = info
                .get_properties()
                .iter()
                .map(|p| (p.key().to_string(), p.val_str().to_string()))
                .collect();
            let host = info
                .get_addresses()
                .iter()
                .next()
                .map(|ip| ip.to_string())
                .unwrap_or_else(|| info.get_hostname().to_string());
            let port = txt
                .get("port")
                .and_then(|p| p.parse().ok())
                .unwrap_or_else(|| info.get_port());
            found.push(ResolvedSender {
                sender_id: txt.get("id").cloned().unwrap_or_default(),
                name: txt.get("n").cloned().unwrap_or_else(|| info.get_fullname().into()),
                host,
                port,
                capabilities: txt
                    .get("caps")
                    .map(|c| c.split(',').map(|s| s.to_string()).collect())
                    .unwrap_or_default(),
                version: txt
                    .get("v")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1),
            });
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
    }

    let _ = mdns.shutdown();
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

// ---------------- connect / list ----------------

fn cmd_connect(target: &str, client_name: &str) -> Result<ControlMessage> {
    let mut sock = dial(target)?;
    let ack = handshake(&mut sock, client_name)?;
    Ok(ack)
}

fn cmd_list(target: &str) -> Result<Vec<DeviceSummary>> {
    let mut sock = dial(target)?;
    let _ack = handshake(&mut sock, CLIENT_NAME)?;
    send_control(&mut sock, &ControlMessage::ListDevices)?;
    match recv_control(&mut sock)? {
        ControlMessage::DeviceList { devices } => Ok(devices),
        other => bail!("expected DEVICE_LIST, got {:?}", other),
    }
}

fn dial(target: &str) -> Result<TcpStream> {
    let addr: SocketAddr = target
        .to_socket_addrs()
        .with_context(|| format!("resolving {target}"))?
        .next()
        .ok_or_else(|| anyhow!("no addresses for {target}"))?;
    let sock = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .with_context(|| format!("connecting to {addr}"))?;
    sock.set_read_timeout(Some(Duration::from_secs(10)))?;
    sock.set_write_timeout(Some(Duration::from_secs(10)))?;
    sock.set_nodelay(true)?;
    Ok(sock)
}

fn handshake(sock: &mut TcpStream, client_name: &str) -> Result<ControlMessage> {
    send_control(
        sock,
        &ControlMessage::Hello {
            version: 1,
            client: client_name.to_string(),
            supports: SUPPORTS.iter().map(|s| s.to_string()).collect(),
        },
    )?;
    match recv_control(sock)? {
        ack @ ControlMessage::HelloAck { .. } => Ok(ack),
        ControlMessage::Error { code, message } => bail!("sender refused: {code}: {message}"),
        other => bail!("unexpected response to HELLO: {:?}", other),
    }
}

fn send_control(sock: &mut TcpStream, msg: &ControlMessage) -> Result<()> {
    let body = msg.to_json()?;
    let bytes = Envelope::new(Channel::Control, &body).encode()?;
    sock.write_all(&bytes)?;
    Ok(())
}

fn recv_control(sock: &mut TcpStream) -> Result<ControlMessage> {
    let mut header = [0u8; HEADER_LEN];
    sock.read_exact(&mut header)?;
    let len = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
    let mut full = Vec::with_capacity(HEADER_LEN + len);
    full.extend_from_slice(&header);
    full.resize(HEADER_LEN + len, 0);
    sock.read_exact(&mut full[HEADER_LEN..])?;
    let (env, _) = Envelope::parse(&full)?.ok_or_else(|| anyhow!("incomplete envelope"))?;
    match env.channel {
        Channel::Control => Ok(ControlMessage::from_json(env.payload)?),
        other => bail!("expected control channel, got {:?}", other),
    }
}
