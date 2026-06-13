//! Subprocess-based wrapper around `zerowire-cli`.
//!
//! The GUI process stays unprivileged: it never touches `/dev/uinput`
//! or `/sys/bus/usb`. Instead it spawns `zerowire-cli receive` and
//! tails its stdout/stderr, mapping log lines into [`DaemonStatus`]
//! transitions that drive the UI.
//!
//! This is intentionally text-based for v0.4. v0.5 will likely promote
//! the IPC to a small JSON-on-stdout protocol so we don't have to
//! grep `INFO ...` strings; the structure here is shaped to make
//! that swap easy.

use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::Result;
use zerowire_protocol::control::DeviceSummary;

use crate::pairing::PairingPayload;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonStatus {
    Idle,
    Waiting,
    Connected { sender: String },
    Error(String),
}

pub struct DaemonHandle {
    child: Option<Child>,
    reader: Option<JoinHandle<()>>,
    shared: Arc<Mutex<Shared>>,
}

#[derive(Default)]
struct Shared {
    status: DaemonStatus,
    devices: Vec<DeviceSummary>,
}

impl Default for DaemonStatus {
    fn default() -> Self { DaemonStatus::Idle }
}

impl DaemonHandle {
    pub fn spawn(payload: &PairingPayload) -> Result<Self> {
        let cli = std::env::var("ZEROWIRE_CLI")
            .unwrap_or_else(|_| "zerowire-cli".to_string());

        // We dial loopback by default for v0.4 — the phone is on the LAN,
        // not on this box. But the daemon needs *something* to dial when
        // the phone hasn't advertised yet. Strategy: discover the phone
        // by mDNS via `--sender`, falling back to the loopback simulator
        // for tests by honoring the `ZEROWIRE_TARGET` env var.
        let mut cmd = Command::new(&cli);
        cmd.arg("receive").arg("--mode").arg("usbip");
        cmd.arg("--psk").arg(&payload.code);
        if let Ok(target) = std::env::var("ZEROWIRE_TARGET") {
            cmd.arg("--target").arg(target);
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");

        let shared = Arc::new(Mutex::new(Shared {
            status: DaemonStatus::Waiting,
            devices: Vec::new(),
        }));

        let shared_a = shared.clone();
        let shared_b = shared.clone();
        std::thread::spawn(move || pump_stdout(stdout, shared_a));
        let reader = std::thread::spawn(move || pump_stderr(stderr, shared_b));

        Ok(Self {
            child: Some(child),
            reader: Some(reader),
            shared,
        })
    }

    pub fn status(&self) -> DaemonStatus {
        self.shared.lock().unwrap().status.clone()
    }

    pub fn devices(&self) -> Vec<DeviceSummary> {
        self.shared.lock().unwrap().devices.clone()
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // SIGTERM first; the CLI installs a signal handler that flips
            // the receive loop's stop flag and exits cleanly. If it
            // doesn't shut down within ~1s we hit it with SIGKILL.
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(j) = self.reader.take() {
            let _ = j.join();
        }
        self.shared.lock().unwrap().status = DaemonStatus::Idle;
    }
}

impl Drop for DaemonHandle {
    fn drop(&mut self) { self.stop(); }
}

/// Tail child stdout and translate log lines into status updates.
///
/// We don't fight to parse every line — the CLI emits free-form `INFO`
/// strings. We grep for a small set of substrings that we *know* the
/// CLI prints at well-defined transitions, and ignore everything else.
fn pump_stdout(stdout: ChildStdout, shared: Arc<Mutex<Shared>>) {
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        update_from_line(&line, &shared);
    }
    finalize_to_idle(&shared);
}

fn pump_stderr(stderr: ChildStderr, shared: Arc<Mutex<Shared>>) {
    let reader = BufReader::new(stderr);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        update_from_line(&line, &shared);
    }
    finalize_to_idle(&shared);
}

fn update_from_line(line: &str, shared: &Arc<Mutex<Shared>>) {
    log::debug!("zerowire-cli: {line}");
    let mut s = shared.lock().unwrap();
    if let Some(name) = line
        .split_once("HELLO_ACK")
        .and_then(|(_, rest)| rest.split_once("name="))
        .and_then(|(_, rest)| rest.split_whitespace().next())
    {
        s.status = DaemonStatus::Connected { sender: name.to_string() };
    } else if line.contains("session ended") {
        s.status = DaemonStatus::Waiting;
    } else if line.contains("ATTACH_DENIED") {
        s.status = DaemonStatus::Error("Receiver denied attach (busid mismatch?)".into());
    } else if line.contains("ERROR") {
        s.status = DaemonStatus::Error(line.to_string());
    }
}

fn finalize_to_idle(shared: &Arc<Mutex<Shared>>) {
    let mut s = shared.lock().unwrap();
    if matches!(s.status, DaemonStatus::Waiting | DaemonStatus::Connected { .. }) {
        s.status = DaemonStatus::Idle;
    }
}
