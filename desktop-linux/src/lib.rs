//! zerowire Linux receiver — library.
//!
//! The CLI binary (`zerowire-cli`) and the mock-sender binary
//! (`zerowire-mock-sender`) both pull in this crate. Everything that touches
//! the wire format, mDNS, or uinput lives here so it can be unit-tested
//! without spinning up a real binary.

pub mod discovery;
pub mod hid_descriptor;
pub mod receiver;
pub mod tls;
pub mod uinput;
pub mod urb_pump;
pub mod urb_driver;
pub mod usbip;
pub mod usbip_receiver;
pub mod wire;
