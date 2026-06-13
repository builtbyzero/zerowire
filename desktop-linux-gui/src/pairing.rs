//! Pairing payload encoded into the QR code.
//!
//! Mirrors the Kotlin `PairingPayload` in
//! `android-sender/.../QrPairing.kt` — the format MUST stay in sync.
//!
//! ```
//!   zerowire://pair?host=<HOST>&port=<PORT>&code=<PAIRING_CODE>
//! ```

use anyhow::Result;
use rand::Rng;

#[derive(Clone, Debug)]
pub struct PairingPayload {
    pub host: String,
    pub port: u16,
    pub code: String,
}

impl PairingPayload {
    /// Generate a fresh pairing payload — picks our LAN-facing IP if it
    /// can be detected, otherwise falls back to `127.0.0.1` so loopback
    /// runs still work for tests.
    pub fn generate(port: u16) -> Result<Self> {
        let host = local_ip_address::local_ip()
            .map(|ip| ip.to_string())
            .unwrap_or_else(|_| "127.0.0.1".to_string());
        let code = random_code();
        Ok(Self { host, port, code })
    }

    pub fn encode(&self) -> String {
        format!(
            "zerowire://pair?host={}&port={}&code={}",
            urlencode(&self.host),
            self.port,
            urlencode(&self.code),
        )
    }

    pub fn local_endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// 12 alphanumeric chars; cryptographically random.
///
/// Wider than the v0.1 6-digit display code because the QR carries it
/// for us — no user types it — so we can afford more entropy. HKDF
/// pulls the PSK from this string identically on both sides.
fn random_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    (0..12)
        .map(|_| {
            let idx = rng.gen_range(0..ALPHABET.len());
            ALPHABET[idx] as char
        })
        .collect()
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let safe = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'~');
        if safe {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{:02X}", b));
        }
    }
    out
}

#[allow(dead_code)]
pub fn ensure_default_port_present(payload: &str) -> Result<()> {
    // Quick sanity that a payload we just rendered round-trips a port.
    if !payload.contains("&port=") {
        anyhow::bail!("payload missing port: {payload}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_round_trip() {
        let p = PairingPayload::generate(47823).unwrap();
        let enc = p.encode();
        assert!(enc.starts_with("zerowire://pair?host="));
        assert!(enc.contains("&port=47823"));
        assert!(enc.contains("&code="));
    }

    #[test]
    fn code_uses_safe_alphabet() {
        for _ in 0..20 {
            let code = random_code();
            assert_eq!(code.len(), 12);
            assert!(code.chars().all(|c| c.is_ascii_alphanumeric()));
            // Confusables intentionally absent.
            for c in ['I', 'O', '0', '1'] {
                assert!(!code.contains(c), "unsafe char {c} in {code}");
            }
        }
    }
}
