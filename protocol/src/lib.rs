//! zerowire wire protocol — shared definitions.
//!
//! This crate is the source of truth for:
//!   * the zerowire envelope framing,
//!   * USB/IP op codes we relay (kernel.org spec),
//!   * the HID fast-lane sub-protocol,
//!   * control-channel JSON message shapes,
//!   * mDNS service constants.
//!
//! It deliberately has **no I/O** — pure encode/decode. Higher layers (the
//! Android sender, the Linux daemon, etc.) pull this in and wire it to their
//! own transports.

#![forbid(unsafe_code)]

pub mod envelope;
pub mod usbip;
pub mod hid;
pub mod control;
pub mod discovery;
pub mod psk;

pub use envelope::{Channel, Envelope, EnvelopeError, MAGIC, PROTOCOL_VERSION};
pub use hid::{
    encode_bind_ack_body, parse_bind_ack_body, BindAckMeta, BindRequest, DeviceKind, HidError,
    HidFrame, HidOp, HID_HEADER_LEN,
};
pub use psk::{derive_psk, derive_tls_cert_seed, derive_with_info, PSK_INFO, PSK_LEN, PSK_SALT};
