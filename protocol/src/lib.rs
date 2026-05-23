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

pub use envelope::{Channel, Envelope, EnvelopeError, MAGIC, PROTOCOL_VERSION};
