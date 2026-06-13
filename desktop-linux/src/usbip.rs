//! Linux `vhci-hcd` host-side wrapper for general USB passthrough.
//!
//! When the sender exports a non-HID device (mass storage, MIDI, printer,
//! generic class) we need the receiver kernel to present it as a real USB
//! device so existing drivers (e.g. `usb-storage`, `snd-usb-audio`) can pick
//! it up. That's exactly what `vhci-hcd` is for: a virtual USB host
//! controller whose ports are driven by remote URBs delivered over a TCP
//! socket.
//!
//! The kernel-level "attach a socket to a vhci port" API is the sysfs file
//! `/sys/devices/platform/vhci_hcd.0/attach`:
//!
//! ```text
//! echo "<port> <socket_fd> <devid> <speed>" > attach
//! ```
//!
//! From that point on the kernel owns the FD; userspace can close its dup
//! and the URBs flow through `/sys/devices/platform/vhci_hcd.0/status` /
//! debugfs etc. Detach via:
//!
//! ```text
//! echo "<port>" > /sys/devices/platform/vhci_hcd.0/detach
//! ```
//!
//! # What's actually implemented vs. simulated
//!
//! * **Real attach** (`attach_socket`) — implemented, gated behind
//!   `simulate=false`. Needs `vhci-hcd` loaded (`modprobe vhci-hcd`) and
//!   root or matching udev rules on the sysfs nodes. We **do not** load the
//!   kernel module ourselves; we surface a clean error and ask the user to
//!   modprobe.
//! * **Simulated attach** (`SimulatedAttach`) — pure userspace stand-in
//!   that lets the rest of the receiver / mock-sender flow run end-to-end
//!   on CI hosts where vhci-hcd isn't available. The "simulated" receiver
//!   writes a transcript file the test asserts on.
//!
//! Both paths consume the same `UsbipAttachInfo` from the protocol crate so
//! the mock sender doesn't have to know which side it's talking to.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
#[cfg(test)]
use anyhow::anyhow;

use zerowire_protocol::usbip::UsbipAttachInfo;

const VHCI_ROOT: &str = "/sys/devices/platform/vhci_hcd.0";
const ATTACH_PATH: &str = "/sys/devices/platform/vhci_hcd.0/attach";
const DETACH_PATH: &str = "/sys/devices/platform/vhci_hcd.0/detach";
const STATUS_PATH: &str = "/sys/devices/platform/vhci_hcd.0/status";

/// Find the lowest-numbered free port on the local `vhci-hcd`. Returns
/// `Err` if the driver isn't loaded.
///
/// The `status` file looks like this (kernel doc):
///
/// ```text
/// hub port sta spd dev      sockfd local_busid
/// hs  0000 004 000 00000000 000000 0-0
/// hs  0001 006 002 00010001 000003 1-2
/// ```
///
/// We treat `sta == 4` (`VDEV_ST_NULL`) as free.
pub fn find_free_port() -> Result<u8> {
    let txt = std::fs::read_to_string(STATUS_PATH).with_context(|| {
        format!(
            "reading {STATUS_PATH} — is vhci-hcd loaded? Try `sudo modprobe vhci-hcd`."
        )
    })?;
    for line in txt.lines().skip(1) {
        let mut cols = line.split_whitespace();
        let _hub = match cols.next() {
            Some(v) => v,
            None => continue,
        };
        let port: u8 = match cols.next().and_then(|p| p.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        let sta: u32 = match cols.next().and_then(|s| s.parse().ok()) {
            Some(s) => s,
            None => continue,
        };
        // 4 = VDEV_ST_NULL (free), 5 = VDEV_ST_NOTASSIGNED (free-ish).
        if sta == 4 || sta == 5 {
            return Ok(port);
        }
    }
    bail!("no free vhci-hcd ports (all {STATUS_PATH} rows are in use)");
}

/// Hand a connected TCP socket to the kernel by writing to vhci's `attach`
/// file. Returns the port number used.
///
/// After this call the kernel owns `socket`; the caller MUST NOT continue
/// reading/writing it. We don't close the local copy here because the
/// kernel keeps a refcount via the file descriptor — closing on our side
/// after a successful attach is the documented pattern.
pub fn attach_socket<F: AsRawFd>(socket: &F, info: &UsbipAttachInfo) -> Result<u8> {
    let port = find_free_port()?;
    let line = format!(
        "{port} {fd} {devid} {speed}",
        port = port,
        fd = socket.as_raw_fd(),
        devid = info.devid,
        speed = info.speed
    );
    let mut f = OpenOptions::new()
        .write(true)
        .open(ATTACH_PATH)
        .with_context(|| {
            format!(
                "opening {ATTACH_PATH} — need CAP_SYS_ADMIN or matching udev rules."
            )
        })?;
    f.write_all(line.as_bytes())
        .with_context(|| format!("writing to {ATTACH_PATH} ({line:?})"))?;
    log::info!(
        "vhci-hcd attached busid={} on port={port} devid={:#x} speed={}",
        info.busid,
        info.devid,
        info.speed
    );
    Ok(port)
}

/// Detach the given vhci-hcd port. Idempotent (a failed detach for an
/// already-free port just logs).
pub fn detach_port(port: u8) -> Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .open(DETACH_PATH)
        .with_context(|| format!("opening {DETACH_PATH}"))?;
    let line = format!("{port}");
    if let Err(e) = f.write_all(line.as_bytes()) {
        log::warn!("vhci-hcd detach port={port} failed (probably already free): {e}");
    } else {
        log::info!("vhci-hcd detached port={port}");
    }
    Ok(())
}

/// Cheap probe so the CLI can fail fast with a clear message instead of
/// halfway through a session.
pub fn vhci_available() -> bool {
    Path::new(VHCI_ROOT).exists() && Path::new(STATUS_PATH).exists()
}

// ---------------- Simulated attach (CI / dev hosts) ----------------

/// Pure-userspace transcript of what a real `vhci-hcd` attach would log.
/// Used by the receiver's `--simulate-usbip` mode so the path is exercised
/// end-to-end on CI hosts that don't have the kernel module available.
///
/// On `Drop`, writes a final `detach` line — mirrors the real cleanup.
pub struct SimulatedAttach {
    transcript: PathBuf,
    port: u8,
    busid: String,
}

impl SimulatedAttach {
    /// Pretend to attach `info` to a fake port. Records the attach decision
    /// to `transcript`.
    pub fn create<P: Into<PathBuf>>(transcript: P, info: &UsbipAttachInfo) -> Result<Self> {
        let path = transcript.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening transcript {}", path.display()))?;
        // Fake port number: lowest 8 bits of import_id, just so different
        // attaches in the same run get different "ports" in the log.
        let port = (info.import_id & 0xFF) as u8;
        writeln!(
            f,
            "sim-attach port={port} busid={} import_id={} devid={:#x} speed={} vid={:04x} pid={:04x}",
            info.busid, info.import_id, info.devid, info.speed, info.vendor_id, info.product_id
        )
        .ok();
        if let Some(d) = &info.descriptor_hex {
            writeln!(f, "sim-descriptor {} bytes={}", info.busid, d.len() / 2).ok();
        }
        Ok(Self {
            transcript: path,
            port,
            busid: info.busid.clone(),
        })
    }

    /// Append a relayed-URB line. The mock USB/IP sender just shovels a
    /// counter through this so the loopback test can assert "≥ N relayed
    /// frames".
    pub fn record_urb(&self, import_id: u32, seq: u32, bytes: usize) -> Result<()> {
        let mut f = OpenOptions::new()
            .append(true)
            .open(&self.transcript)
            .with_context(|| format!("appending {}", self.transcript.display()))?;
        writeln!(
            f,
            "sim-urb port={} import_id={import_id} seq={seq} bytes={bytes}",
            self.port
        )
        .ok();
        Ok(())
    }

    pub fn port(&self) -> u8 {
        self.port
    }
    pub fn busid(&self) -> &str {
        &self.busid
    }
}

impl Drop for SimulatedAttach {
    fn drop(&mut self) {
        if let Ok(mut f) = OpenOptions::new().append(true).open(&self.transcript) {
            let _ = writeln!(f, "sim-detach port={} busid={}", self.port, self.busid);
        }
    }
}

/// Synthesize a plausible 18-byte `usb_device_descriptor` blob from a few
/// fields. Used by the mock USB/IP sender so the receiver has *something*
/// descriptor-shaped to log even in simulation. Layout is little-endian
/// per USB 2.0 §9.6.1.
pub fn synth_device_descriptor(vendor: u16, product: u16, class: u8) -> [u8; 18] {
    let mut d = [0u8; 18];
    d[0] = 0x12; // bLength
    d[1] = 0x01; // bDescriptorType = DEVICE
    d[2] = 0x00;
    d[3] = 0x02; // bcdUSB = 0x0200
    d[4] = class; // bDeviceClass
    d[5] = 0x00; // bDeviceSubClass
    d[6] = 0x00; // bDeviceProtocol
    d[7] = 64; // bMaxPacketSize0
    d[8..10].copy_from_slice(&vendor.to_le_bytes());
    d[10..12].copy_from_slice(&product.to_le_bytes());
    d[12] = 0x00;
    d[13] = 0x01; // bcdDevice = 0x0100
    d[14] = 0; // iManufacturer
    d[15] = 0; // iProduct
    d[16] = 0; // iSerialNumber
    d[17] = 1; // bNumConfigurations
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_starts_with_18_01() {
        let d = synth_device_descriptor(0xBADD, 0xC0DE, 0x08);
        assert_eq!(d[0], 0x12);
        assert_eq!(d[1], 0x01);
        // Vendor low byte, vendor high byte.
        assert_eq!(d[8], 0xDD);
        assert_eq!(d[9], 0xBA);
        assert_eq!(d[10], 0xDE);
        assert_eq!(d[11], 0xC0);
        assert_eq!(d[4], 0x08);
    }

    #[test]
    fn vhci_available_does_not_panic() {
        // Just exercise the path: returns true on a real Linux host with
        // the module loaded, false otherwise. Either way, no crash.
        let _ = vhci_available();
    }

    #[test]
    fn simulated_attach_writes_transcript() -> Result<()> {
        let dir = tempdir()?;
        let path = dir.path().join("vhci.log");
        let info = UsbipAttachInfo {
            busid: "1-2".into(),
            import_id: 7,
            devid: 0x0001_0002,
            speed: 3,
            vendor_id: 0xBADD,
            product_id: 0xC0DE,
            descriptor_hex: Some("deadbeef".into()),
        };
        {
            let sa = SimulatedAttach::create(&path, &info)?;
            sa.record_urb(7, 1, 64)?;
            sa.record_urb(7, 2, 64)?;
        }
        let body = std::fs::read_to_string(&path)?;
        assert!(body.contains("sim-attach"));
        assert!(body.contains("busid=1-2"));
        assert!(body.contains("sim-urb"));
        assert!(body.contains("sim-detach"));
        Ok(())
    }

    /// Tiny self-contained tempdir so we don't add the `tempfile` crate just
    /// for one test.
    struct TmpDir(PathBuf);
    impl TmpDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir() -> Result<TmpDir> {
        let pid = std::process::id();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let p = std::env::temp_dir().join(format!("zerowire-usbip-test-{pid}-{nonce}"));
        std::fs::create_dir_all(&p)?;
        Ok(TmpDir(p))
    }

    /// Compile-only check that `attach_socket` / `detach_port` have the
    /// signatures we claim; never actually runs.
    fn _silence() -> Result<()> {
        if false {
            let info = UsbipAttachInfo {
                busid: "x".into(),
                import_id: 0,
                devid: 0,
                speed: 0,
                vendor_id: 0,
                product_id: 0,
                descriptor_hex: None,
            };
            let socket = std::net::TcpStream::connect("127.0.0.1:1")?;
            let _ = attach_socket(&socket, &info)?;
            let _ = detach_port(0)?;
            let _ = anyhow!("noop");
        }
        Ok(())
    }
}
