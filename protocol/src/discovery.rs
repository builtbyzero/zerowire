//! mDNS service constants and TXT-record helpers.

/// Service type registered on the LAN, fully-qualified mDNS form.
pub const SERVICE_TYPE: &str = "_zerowire._tcp.local.";

/// Default TCP port (also published in the SRV record). Picked from the
/// IANA dynamic/ephemeral range, but stable across versions so manual
/// `host:port` setups work too.
pub const DEFAULT_PORT: u16 = 47823;

/// The keys we publish in TXT records.
pub mod txt {
    pub const VERSION: &str = "v";
    pub const SENDER_ID: &str = "id";
    pub const NAME: &str = "n";
    pub const CAPABILITIES: &str = "caps";
    pub const PORT: &str = "port";
}

/// A snapshot of one resolved zerowire service on the LAN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSender {
    pub sender_id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub capabilities: Vec<String>,
    pub version: u8,
}

/// Build the TXT-record map a sender should advertise.
pub fn build_txt(
    sender_id: &str,
    name: &str,
    port: u16,
    capabilities: &[&str],
) -> Vec<(String, String)> {
    vec![
        (txt::VERSION.into(), "1".into()),
        (txt::SENDER_ID.into(), sender_id.into()),
        (txt::NAME.into(), name.into()),
        (txt::PORT.into(), port.to_string()),
        (txt::CAPABILITIES.into(), capabilities.join(",")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txt_contains_required_keys() {
        let txts = build_txt("abc", "Pixel 8", 47823, &["usbip", "hid"]);
        let keys: Vec<&str> = txts.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&txt::VERSION));
        assert!(keys.contains(&txt::SENDER_ID));
        assert!(keys.contains(&txt::NAME));
        assert!(keys.contains(&txt::PORT));
        assert!(keys.contains(&txt::CAPABILITIES));
        let caps = &txts
            .iter()
            .find(|(k, _)| k == txt::CAPABILITIES)
            .unwrap()
            .1;
        assert_eq!(caps, "usbip,hid");
    }
}
