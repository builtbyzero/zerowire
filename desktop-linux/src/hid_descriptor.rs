//! Minimal HID report descriptor inspection.
//!
//! We don't want a full HID parser on the receiver — the sender does that
//! work and tells us the device kind in `BindAckMeta`. This module exists
//! for two narrow jobs:
//!
//!   1. As a defence-in-depth check on what the sender claimed.
//!   2. So tests (and the mock-sender) can hand around real-looking
//!      descriptors without copy-pasting magic byte arrays.
//!
//! Reference: USB HID Usage Tables 1.4, §4.

/// Top-level Usage Page byte for "Generic Desktop".
pub const USAGE_PAGE_GENERIC_DESKTOP: u8 = 0x01;
/// Generic Desktop usages we care about.
pub const USAGE_MOUSE: u8 = 0x02;
pub const USAGE_KEYBOARD: u8 = 0x06;
pub const USAGE_GAMEPAD: u8 = 0x05;
pub const USAGE_JOYSTICK: u8 = 0x04;

/// Coarse classification from peeking at the descriptor's first top-level
/// `Usage Page (Generic Desktop) / Usage (..)` pair.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Mouse,
    Keyboard,
    Gamepad,
    Other,
}

/// Best-effort: classify a report descriptor by sniffing the first usage.
/// Returns `Kind::Other` if we can't tell.
pub fn classify(desc: &[u8]) -> Kind {
    let mut page: Option<u8> = None;
    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];
        // bSize encoded in low 2 bits (0=>0, 1=>1, 2=>2, 3=>4 bytes).
        let size = match prefix & 0x03 {
            0 => 0usize,
            1 => 1,
            2 => 2,
            _ => 4,
        };
        let tag = prefix & 0xFC; // type+tag (we don't care about the type bits explicitly)
        if i + 1 + size > desc.len() {
            break;
        }
        let data_le = match size {
            0 => 0u32,
            1 => desc[i + 1] as u32,
            2 => u16::from_le_bytes([desc[i + 1], desc[i + 2]]) as u32,
            _ => u32::from_le_bytes([
                desc[i + 1],
                desc[i + 2],
                desc[i + 3],
                desc[i + 4],
            ]),
        };

        // Global Item: Usage Page = 0b0000_01_00 == 0x04, tag bits high nibble
        // We use the actual tag values from the spec:
        //   Usage Page  : 0x04 (global)
        //   Usage       : 0x08 (local)
        if tag == 0x04 {
            page = Some(data_le as u8);
        } else if tag == 0x08 {
            if page == Some(USAGE_PAGE_GENERIC_DESKTOP) {
                return match data_le as u8 {
                    USAGE_MOUSE => Kind::Mouse,
                    USAGE_KEYBOARD => Kind::Keyboard,
                    USAGE_GAMEPAD | USAGE_JOYSTICK => Kind::Gamepad,
                    _ => Kind::Other,
                };
            }
        }
        i += 1 + size;
    }
    Kind::Other
}

/// A textbook boot-mouse report descriptor (5 buttons + X + Y + wheel).
pub fn mouse_descriptor() -> Vec<u8> {
    vec![
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x02, // Usage (Mouse)
        0xA1, 0x01, // Collection (Application)
        0x09, 0x01, //   Usage (Pointer)
        0xA1, 0x00, //   Collection (Physical)
        0x05, 0x09, //     Usage Page (Button)
        0x19, 0x01, //     Usage Minimum (1)
        0x29, 0x05, //     Usage Maximum (5)
        0x15, 0x00, //     Logical Minimum (0)
        0x25, 0x01, //     Logical Maximum (1)
        0x95, 0x05, //     Report Count (5)
        0x75, 0x01, //     Report Size (1)
        0x81, 0x02, //     Input (Data,Var,Abs)
        0x95, 0x01, //     Report Count (1)
        0x75, 0x03, //     Report Size (3)   <- padding
        0x81, 0x03, //     Input (Cnst,Var,Abs)
        0x05, 0x01, //     Usage Page (Generic Desktop)
        0x09, 0x30, //     Usage (X)
        0x09, 0x31, //     Usage (Y)
        0x09, 0x38, //     Usage (Wheel)
        0x15, 0x81, //     Logical Minimum (-127)
        0x25, 0x7F, //     Logical Maximum (127)
        0x75, 0x08, //     Report Size (8)
        0x95, 0x03, //     Report Count (3)
        0x81, 0x06, //     Input (Data,Var,Rel)
        0xC0, //   End Collection
        0xC0, // End Collection
    ]
}

/// A short keyboard descriptor (just enough for the classifier).
pub fn keyboard_descriptor() -> Vec<u8> {
    vec![
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x06, // Usage (Keyboard)
        0xA1, 0x01, // Collection (Application)
        0xC0, // End Collection
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_mouse() {
        assert_eq!(classify(&mouse_descriptor()), Kind::Mouse);
    }

    #[test]
    fn classifies_keyboard() {
        assert_eq!(classify(&keyboard_descriptor()), Kind::Keyboard);
    }

    #[test]
    fn classifies_other_when_empty() {
        assert_eq!(classify(&[]), Kind::Other);
    }

    #[test]
    fn classifies_other_when_garbage() {
        assert_eq!(classify(&[0xFF, 0xFF, 0xFF]), Kind::Other);
    }
}
