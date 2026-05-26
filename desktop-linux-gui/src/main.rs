//! zerowire-gui — Linux desktop receiver UI.
//!
//! This binary is a thin user-facing wrapper around the headless
//! `zerowire-cli` daemon. The same Rust code that drives the CLI's
//! receive path lives in the `zerowire-cli` library crate; this GUI
//! just paints a window and a QR code on top of it.
//!
//! What it does today
//!
//! 1. Generates a random pairing code on launch.
//! 2. Computes a QR payload (`zerowire://pair?host=...&port=...&code=...`)
//!    matching the Android sender's `PairingPayload::parse` format.
//! 3. Renders the QR into the window so the phone can scan it.
//! 4. Shows the current daemon status (waiting / connected / error) and
//!    the list of devices the sender has advertised.
//! 5. Lets the user stop / regenerate the pairing.
//!
//! What it intentionally does NOT do in v0.4
//!
//! * A system-tray icon. The `tray` cargo feature wires up `ksni` for
//!   StatusNotifierItem support, but it's off by default to keep
//!   `cargo build` working on minimal Linuxes — appindicator headers
//!   aren't free.
//! * Auto-mounting recognised mass-storage devices. The plan is to
//!   shell out to `udisksctl` once a vhci-hcd attach lands; for now
//!   the user sees the device and uses it from the kernel like any
//!   USB drive.
//!
//! The daemon path
//!
//! For each session we spawn `zerowire-cli receive --mode usbip --psk <CODE>`
//! as a subprocess and tail its stdout for state. Rationale: keeps the
//! GUI process unprivileged (no /dev/uinput from this binary), and the
//! CLI keeps its existing structured-logging interface. Subprocess
//! plumbing in [`daemon`].

use std::sync::{Arc, Mutex};

use anyhow::Result;
use egui::{ColorImage, TextureHandle};
use qrcode::QrCode;

mod daemon;
mod pairing;

use daemon::{DaemonHandle, DaemonStatus};
use pairing::PairingPayload;

const DEFAULT_PORT: u16 = 47823;
const WINDOW_WIDTH: f32 = 720.0;
const WINDOW_HEIGHT: f32 = 540.0;

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([WINDOW_WIDTH, WINDOW_HEIGHT])
            .with_min_inner_size([520.0, 420.0])
            .with_title("zerowire"),
        ..Default::default()
    };
    eframe::run_native(
        "zerowire",
        native_options,
        Box::new(|cc| Ok(Box::new(App::new(cc)?))),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}

struct App {
    payload: PairingPayload,
    qr_texture: Option<TextureHandle>,
    daemon: Arc<Mutex<Option<DaemonHandle>>>,
    last_status: DaemonStatus,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Result<Self> {
        let payload = PairingPayload::generate(DEFAULT_PORT)?;
        let img = qr_image(&payload.encode())?;
        let texture = cc.egui_ctx.load_texture("pairing-qr", img, Default::default());
        Ok(Self {
            payload,
            qr_texture: Some(texture),
            daemon: Arc::new(Mutex::new(None)),
            last_status: DaemonStatus::Idle,
        })
    }

    fn regenerate_payload(&mut self, ctx: &egui::Context) -> Result<()> {
        self.payload = PairingPayload::generate(self.payload.port)?;
        let img = qr_image(&self.payload.encode())?;
        self.qr_texture = Some(ctx.load_texture("pairing-qr", img, Default::default()));
        // Restart the daemon under the new PSK if it's currently running.
        if self.daemon.lock().unwrap().is_some() {
            self.stop_daemon();
            self.start_daemon();
        }
        Ok(())
    }

    fn start_daemon(&mut self) {
        let payload = self.payload.clone();
        let mut guard = self.daemon.lock().unwrap();
        if guard.is_some() {
            return;
        }
        match DaemonHandle::spawn(&payload) {
            Ok(h) => *guard = Some(h),
            Err(e) => log::error!("daemon spawn failed: {e}"),
        }
    }

    fn stop_daemon(&mut self) {
        let mut guard = self.daemon.lock().unwrap();
        if let Some(mut h) = guard.take() {
            h.stop();
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll the daemon for status; status is shared via Arc<Mutex>.
        {
            let guard = self.daemon.lock().unwrap();
            if let Some(h) = guard.as_ref() {
                self.last_status = h.status();
            } else {
                self.last_status = DaemonStatus::Idle;
            }
        }

        egui::TopBottomPanel::top("title").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("zerowire");
                ui.label(egui::RichText::new("Receive any USB device over WiFi.").weak());
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical(|ui| {
                ui.add_space(8.0);
                self.draw_status(ui);
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);
                self.draw_pairing(ui, ctx);
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);
                self.draw_devices(ui);
            });
        });

        // egui defaults to event-driven repaints; status polling needs
        // a periodic nudge so the daemon's stdout shows up in the UI.
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }

    fn on_exit(&mut self, _ctx: Option<&eframe::glow::Context>) {
        self.stop_daemon();
    }
}

impl App {
    fn draw_status(&mut self, ui: &mut egui::Ui) {
        let (title, body, tint) = match &self.last_status {
            DaemonStatus::Idle => (
                "Idle",
                "Click \"Wait for phone\" to start listening.".to_string(),
                egui::Color32::from_rgb(80, 80, 90),
            ),
            DaemonStatus::Waiting => (
                "Waiting for phone…",
                format!("Scan the QR with the zerowire Android app, or connect manually to {}.", self.payload.local_endpoint()),
                egui::Color32::from_rgb(40, 100, 160),
            ),
            DaemonStatus::Connected { sender } => (
                "Connected",
                format!("Phone: {sender}"),
                egui::Color32::from_rgb(40, 140, 80),
            ),
            DaemonStatus::Error(msg) => (
                "Error",
                msg.clone(),
                egui::Color32::from_rgb(180, 50, 50),
            ),
        };
        egui::Frame::group(ui.style()).fill(tint.linear_multiply(0.18)).show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(title).heading());
                ui.label(body);
                ui.horizontal(|ui| {
                    let is_running = !matches!(self.last_status, DaemonStatus::Idle);
                    if !is_running {
                        if ui.button("Wait for phone").clicked() {
                            self.start_daemon();
                        }
                    } else if ui.button("Stop").clicked() {
                        self.stop_daemon();
                    }
                });
            });
        });
    }

    fn draw_pairing(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(egui::RichText::new("Pairing QR").strong());
                ui.label(format!("Endpoint: {}", self.payload.local_endpoint()));
                ui.monospace(format!("Code: {}", self.payload.code));
                if ui.button("Regenerate pairing").clicked() {
                    if let Err(e) = self.regenerate_payload(ctx) {
                        log::error!("regen: {e}");
                    }
                }
                ui.label(
                    egui::RichText::new(
                        "Open the zerowire app on your phone and scan this QR.",
                    )
                    .weak(),
                );
            });
            ui.add_space(16.0);
            if let Some(tex) = &self.qr_texture {
                ui.image((tex.id(), egui::vec2(220.0, 220.0)));
            }
        });
    }

    fn draw_devices(&self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Attached USB devices").strong());
        let devices = match &self.last_status {
            DaemonStatus::Connected { .. } => self.daemon.lock().unwrap().as_ref().map(|h| h.devices()).unwrap_or_default(),
            _ => Vec::new(),
        };
        if devices.is_empty() {
            ui.label(egui::RichText::new("(none yet — the phone hasn't advertised any devices)").weak());
            return;
        }
        for d in &devices {
            ui.horizontal(|ui| {
                ui.monospace(&d.busid);
                ui.label(format!("{:04x}:{:04x}", d.vendor_id, d.product_id));
                ui.label(d.product.clone().unwrap_or_else(|| "<unnamed>".into()));
                if d.is_hid {
                    ui.label(egui::RichText::new("[HID]").color(egui::Color32::LIGHT_BLUE));
                }
            });
        }
    }
}

/// Render [payload] as a 1-bit PNG-ish QR code into an egui [`ColorImage`].
fn qr_image(payload: &str) -> Result<ColorImage> {
    let code = QrCode::new(payload.as_bytes())?;
    let modules = code.to_colors();
    let width = code.width();
    // Scale up so each module is 6×6 pixels — keeps the QR usable on phone
    // cameras without exploding texture memory.
    let scale = 6;
    let dim = width * scale;
    let mut pixels = vec![0u8; dim * dim * 4];
    for y in 0..width {
        for x in 0..width {
            let dark = modules[y * width + x] == qrcode::Color::Dark;
            let color: [u8; 4] = if dark { [0, 0, 0, 255] } else { [255, 255, 255, 255] };
            for dy in 0..scale {
                for dx in 0..scale {
                    let px = (y * scale + dy) * dim + (x * scale + dx);
                    pixels[px * 4..px * 4 + 4].copy_from_slice(&color);
                }
            }
        }
    }
    Ok(ColorImage::from_rgba_unmultiplied([dim, dim], &pixels))
}
