//! zerowire-cli — Linux receiver entry point.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};

use zerowire_cli::discovery::discover;
use zerowire_cli::receiver::{run_receive, run_simulate, run_simulate_source, ReceiveOpts};
use zerowire_cli::wire::{recv_control, send_control};

use zerowire_protocol::control::{ControlMessage, DeviceSummary};

const CLIENT_NAME: &str = "zerowire-cli/0.2.0";
const SUPPORTS: &[&str] = &["usbip/1.1.1", "hid-fastlane/1"];

#[derive(Parser, Debug)]
#[command(
    name = "zerowire-cli",
    about = "zerowire Linux receiver — discovery, control, HID fast lane",
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
    /// Open a HID fast-lane session against a discovered sender. Mouse moves
    /// the cursor on this machine via /dev/uinput.
    Receive {
        /// Display name of the sender (TXT `n=`) to discover.
        #[arg(long)]
        sender: Option<String>,
        /// Skip mDNS, dial `host:port` directly.
        #[arg(long)]
        target: Option<String>,
        /// `busid` of the device on the sender to bind. Defaults to the
        /// first `is_hid: true` device the sender lists.
        #[arg(long)]
        busid: Option<String>,
        /// Synthesize a virtual mouse instead of dialing a sender. For
        /// verifying uinput permissions and visual feedback.
        #[arg(long)]
        simulate: bool,
        /// Dial `--target` but write report bytes to a log file instead of
        /// pushing them through uinput. Used by tests/hid_loopback.sh.
        #[arg(long)]
        simulate_source: Option<PathBuf>,
        /// mDNS browse timeout in seconds.
        #[arg(long, default_value_t = 5)]
        timeout: u64,
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
            sender,
            target,
            busid,
            simulate,
            simulate_source,
            timeout,
        } => {
            let stop = Arc::new(AtomicBool::new(false));
            install_ctrlc(stop.clone())?;
            if simulate {
                return run_simulate(stop);
            }
            if let Some(log_path) = simulate_source {
                let tgt = target.unwrap_or_else(|| "127.0.0.1:47823".to_string());
                return run_simulate_source(&tgt, &log_path);
            }
            run_receive(
                ReceiveOpts {
                    sender_name: sender.unwrap_or_else(|| "Pixel".to_string()),
                    discover_timeout: Duration::from_secs(timeout),
                    busid,
                    direct_target: target,
                    client_name: CLIENT_NAME.into(),
                },
                stop,
            )
        }
    }
}

fn install_ctrlc(flag: Arc<AtomicBool>) -> Result<()> {
    // Minimal: just install a SIGINT handler that flips the flag once.
    // We avoid adding ctrlc as a dependency.
    use std::os::raw::c_int;
    extern "C" fn handler(_sig: c_int) {
        FLAG.store(true, Ordering::SeqCst);
    }
    static FLAG: AtomicBool = AtomicBool::new(false);
    // share the atomic via a static — clone the input flag every tick.
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
