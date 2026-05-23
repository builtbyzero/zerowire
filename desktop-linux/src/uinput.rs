//! Linux `uinput` backend — raw ioctls, no extra deps beyond `libc`.
//!
//! We open `/dev/uinput`, configure event bits with a series of `UI_SET_*BIT`
//! ioctls, write a `uinput_setup` struct, fire `UI_DEV_CREATE`, then push
//! `input_event` records via plain `write(2)`. Tear-down is `UI_DEV_DESTROY`
//! + `close(2)`. This is the same path `python-uinput`, `evdev`, and
//! `input-linux` all wrap; we just don't pull in 200kloc to do it.
//!
//! Requires either:
//!   * the binary running as root, OR
//!   * a udev rule + group membership granting RW on `/dev/uinput`
//!     (see `udev/99-zerowire-uinput.rules` in the repo).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::SystemTime;

use anyhow::{bail, Context, Result};

// ---------------- ioctl / type constants ----------------
//
// From <linux/input-event-codes.h>, <linux/input.h>, <linux/uinput.h>.
// We hand-roll the numbers to avoid a bindgen dep.

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;

const SYN_REPORT: u16 = 0;

pub const REL_X: u16 = 0x00;
pub const REL_Y: u16 = 0x01;
pub const REL_WHEEL: u16 = 0x08;
pub const REL_HWHEEL: u16 = 0x06;

pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;
pub const BTN_SIDE: u16 = 0x113;
pub const BTN_EXTRA: u16 = 0x114;

// uinput ioctl numbers (UI_DEV_CREATE = _IO('U', 1) etc.).
// _IO macro expansion: dir=0(_IOC_NONE) size=0 type='U'(0x55) nr=N
//   => (0 << 30) | (0 << 16) | (0x55 << 8) | N
// For UI_SET_EVBIT etc.: _IOW('U', N, int) ⇒ dir=1, size=4
//   => (1 << 30) | (4 << 16) | (0x55 << 8) | N
const UI_SET_EVBIT: u64 = ioctl_iow(0x55, 100, 4);
const UI_SET_KEYBIT: u64 = ioctl_iow(0x55, 101, 4);
const UI_SET_RELBIT: u64 = ioctl_iow(0x55, 102, 4);
const UI_DEV_SETUP: u64 = ioctl_iow(0x55, 3, std::mem::size_of::<UinputSetup>());
const UI_DEV_CREATE: u64 = ioctl_io(0x55, 1);
const UI_DEV_DESTROY: u64 = ioctl_io(0x55, 2);

const fn ioctl_io(typ: u8, nr: u8) -> u64 {
    (0u64 << 30) | (0u64 << 16) | ((typ as u64) << 8) | (nr as u64)
}
const fn ioctl_iow(typ: u8, nr: u8, size: usize) -> u64 {
    (1u64 << 30) | ((size as u64) << 16) | ((typ as u64) << 8) | (nr as u64)
}

// ---------------- structs (must match kernel layout) ----------------

#[repr(C)]
#[derive(Default)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
struct InputEvent {
    /// Microseconds-resolution timestamp (we fill in monotonic-ish via SystemTime).
    tv_sec: libc::time_t,
    tv_usec: libc::suseconds_t,
    type_: u16,
    code: u16,
    value: i32,
}

// ---------------- public API ----------------

/// A virtual input device backed by `/dev/uinput`.
pub struct UinputDevice {
    file: File,
    /// `true` until `destroy()` runs; we use it from Drop.
    live: bool,
}

/// What to expose. We keep this coarse — finer-grain capabilities can be
/// added per-device as the protocol grows.
#[derive(Clone, Debug)]
pub enum DeviceProfile {
    /// Relative-mode mouse: REL_X, REL_Y, wheel, 5 buttons.
    Mouse {
        name: String,
        vendor_id: u16,
        product_id: u16,
    },
    /// Standard 104-key keyboard (all KEY_RESERVED..KEY_MICMUTE codes).
    Keyboard {
        name: String,
        vendor_id: u16,
        product_id: u16,
    },
}

impl UinputDevice {
    /// Open `/dev/uinput` and create a virtual device matching `profile`.
    ///
    /// Returns `Err` if `/dev/uinput` is missing (kernel module not loaded)
    /// or unwritable (no permission).
    pub fn create(profile: &DeviceProfile) -> Result<Self> {
        let path = Path::new("/dev/uinput");
        if !path.exists() {
            bail!(
                "/dev/uinput is missing — load the kernel module: \
                 sudo modprobe uinput"
            );
        }
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .with_context(|| {
                "opening /dev/uinput (need root or `uinput` group + udev rule; \
                 see desktop-linux/udev/README.md)"
                    .to_string()
            })?;
        let fd = file.as_raw_fd();

        // Tell the kernel which event types and codes we'll emit.
        match profile {
            DeviceProfile::Mouse { .. } => {
                set_evbit(fd, EV_KEY)?;
                set_evbit(fd, EV_REL)?;
                set_evbit(fd, EV_SYN)?;
                for btn in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA] {
                    set_keybit(fd, btn)?;
                }
                for rel in [REL_X, REL_Y, REL_WHEEL, REL_HWHEEL] {
                    set_relbit(fd, rel)?;
                }
            }
            DeviceProfile::Keyboard { .. } => {
                set_evbit(fd, EV_KEY)?;
                set_evbit(fd, EV_SYN)?;
                // Allow all keys 1..=255. This is generous but it's exactly
                // what evtest-style virtual keyboards do.
                for k in 1u16..=255 {
                    set_keybit(fd, k)?;
                }
            }
        }

        // UI_DEV_SETUP with our identification block.
        let (name_str, vendor_id, product_id) = match profile {
            DeviceProfile::Mouse { name, vendor_id, product_id } => (name, *vendor_id, *product_id),
            DeviceProfile::Keyboard { name, vendor_id, product_id } => (name, *vendor_id, *product_id),
        };
        let mut setup = UinputSetup {
            id: InputId {
                bustype: 0x03, // BUS_USB
                vendor: vendor_id,
                product: product_id,
                version: 0x0100,
            },
            name: [0u8; 80],
            ff_effects_max: 0,
        };
        let bytes = name_str.as_bytes();
        let n = bytes.len().min(setup.name.len() - 1);
        setup.name[..n].copy_from_slice(&bytes[..n]);

        unsafe {
            if libc::ioctl(fd, UI_DEV_SETUP, &setup as *const _) < 0 {
                bail!("UI_DEV_SETUP failed: {}", std::io::Error::last_os_error());
            }
            if libc::ioctl(fd, UI_DEV_CREATE) < 0 {
                bail!("UI_DEV_CREATE failed: {}", std::io::Error::last_os_error());
            }
        }

        Ok(Self { file, live: true })
    }

    fn write_event(&mut self, type_: u16, code: u16, value: i32) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        let ev = InputEvent {
            tv_sec: now.as_secs() as libc::time_t,
            tv_usec: (now.subsec_micros()) as libc::suseconds_t,
            type_,
            code,
            value,
        };
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &ev as *const _ as *const u8,
                std::mem::size_of::<InputEvent>(),
            )
        };
        self.file.write_all(bytes).context("writing input_event")?;
        Ok(())
    }

    fn sync(&mut self) -> Result<()> {
        self.write_event(EV_SYN, SYN_REPORT, 0)
    }

    // ---------- mouse-shaped helpers ----------

    /// Emit a relative-mode mouse motion + sync.
    pub fn mouse_move(&mut self, dx: i32, dy: i32) -> Result<()> {
        if dx != 0 {
            self.write_event(EV_REL, REL_X, dx)?;
        }
        if dy != 0 {
            self.write_event(EV_REL, REL_Y, dy)?;
        }
        self.sync()
    }

    /// Emit a wheel tick (positive = scroll up).
    pub fn mouse_wheel(&mut self, dv: i32, dh: i32) -> Result<()> {
        if dv != 0 {
            self.write_event(EV_REL, REL_WHEEL, dv)?;
        }
        if dh != 0 {
            self.write_event(EV_REL, REL_HWHEEL, dh)?;
        }
        self.sync()
    }

    /// Press or release a button.
    pub fn mouse_button(&mut self, button: u16, pressed: bool) -> Result<()> {
        self.write_event(EV_KEY, button, if pressed { 1 } else { 0 })?;
        self.sync()
    }

    // ---------- keyboard-shaped helpers ----------

    pub fn key(&mut self, code: u16, pressed: bool) -> Result<()> {
        self.write_event(EV_KEY, code, if pressed { 1 } else { 0 })?;
        self.sync()
    }

    /// Idempotent destroy. After this the device disappears from the kernel.
    pub fn destroy(&mut self) -> Result<()> {
        if !self.live {
            return Ok(());
        }
        let fd = self.file.as_raw_fd();
        unsafe {
            if libc::ioctl(fd, UI_DEV_DESTROY) < 0 {
                bail!("UI_DEV_DESTROY failed: {}", std::io::Error::last_os_error());
            }
        }
        self.live = false;
        Ok(())
    }
}

impl Drop for UinputDevice {
    fn drop(&mut self) {
        let _ = self.destroy();
    }
}

// ---------------- small ioctl wrappers ----------------

fn set_evbit(fd: i32, bit: u16) -> Result<()> {
    unsafe {
        if libc::ioctl(fd, UI_SET_EVBIT, bit as i32) < 0 {
            bail!("UI_SET_EVBIT({}) failed: {}", bit, std::io::Error::last_os_error());
        }
    }
    Ok(())
}

fn set_keybit(fd: i32, bit: u16) -> Result<()> {
    unsafe {
        if libc::ioctl(fd, UI_SET_KEYBIT, bit as i32) < 0 {
            bail!("UI_SET_KEYBIT({}) failed: {}", bit, std::io::Error::last_os_error());
        }
    }
    Ok(())
}

fn set_relbit(fd: i32, bit: u16) -> Result<()> {
    unsafe {
        if libc::ioctl(fd, UI_SET_RELBIT, bit as i32) < 0 {
            bail!("UI_SET_RELBIT({}) failed: {}", bit, std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// We can't actually open /dev/uinput in CI, but we can sanity-check the
    /// ioctl number math: the literal values from /usr/include/linux/uinput.h
    /// are documented and stable.
    #[test]
    fn ioctl_numbers_match_kernel_headers() {
        // UI_DEV_CREATE = _IO('U', 1) = 0x5501
        assert_eq!(UI_DEV_CREATE, 0x5501);
        // UI_DEV_DESTROY = _IO('U', 2) = 0x5502
        assert_eq!(UI_DEV_DESTROY, 0x5502);
        // UI_SET_EVBIT = _IOW('U', 100, int)
        //   = (1 << 30) | (4 << 16) | (0x55 << 8) | 100
        //   = 0x40045564
        assert_eq!(UI_SET_EVBIT, 0x40045564);
        // UI_SET_KEYBIT = 0x40045565
        assert_eq!(UI_SET_KEYBIT, 0x40045565);
        // UI_SET_RELBIT = 0x40045566
        assert_eq!(UI_SET_RELBIT, 0x40045566);
    }

    #[test]
    fn input_event_size_matches_kernel_struct() {
        // sizeof(struct input_event) on 64-bit Linux is 24 bytes
        // (timeval = 16, type+code = 4, value = 4).
        assert_eq!(std::mem::size_of::<InputEvent>(), 24);
    }
}
