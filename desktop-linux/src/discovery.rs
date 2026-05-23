//! mDNS discovery for zerowire senders.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use zerowire_protocol::discovery::{ResolvedSender, SERVICE_TYPE};

/// Browse the LAN for zerowire senders for up to `timeout`.
pub fn discover(timeout: Duration) -> Result<Vec<ResolvedSender>> {
    let mdns = ServiceDaemon::new().context("starting mDNS daemon")?;
    let rx = mdns.browse(SERVICE_TYPE).context("starting mDNS browse")?;
    let deadline = Instant::now() + timeout;
    let mut found: Vec<ResolvedSender> = Vec::new();

    while let Ok(evt) =
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        if let ServiceEvent::ServiceResolved(info) = evt {
            let txt: std::collections::HashMap<String, String> = info
                .get_properties()
                .iter()
                .map(|p| (p.key().to_string(), p.val_str().to_string()))
                .collect();
            let host = info
                .get_addresses()
                .iter()
                .next()
                .map(|ip| ip.to_string())
                .unwrap_or_else(|| info.get_hostname().to_string());
            let port = txt
                .get("port")
                .and_then(|p| p.parse().ok())
                .unwrap_or_else(|| info.get_port());
            found.push(ResolvedSender {
                sender_id: txt.get("id").cloned().unwrap_or_default(),
                name: txt
                    .get("n")
                    .cloned()
                    .unwrap_or_else(|| info.get_fullname().into()),
                host,
                port,
                capabilities: txt
                    .get("caps")
                    .map(|c| c.split(',').map(|s| s.to_string()).collect())
                    .unwrap_or_default(),
                version: txt.get("v").and_then(|v| v.parse().ok()).unwrap_or(1),
            });
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    let _ = mdns.shutdown();
    Ok(found)
}

/// Pick one resolved sender by its TXT `n` (display name), with fuzzy
/// matching: exact > case-insensitive equal > substring.
pub fn pick_by_name(senders: &[ResolvedSender], name: &str) -> Option<ResolvedSender> {
    if let Some(s) = senders.iter().find(|s| s.name == name) {
        return Some(s.clone());
    }
    let lc = name.to_lowercase();
    if let Some(s) = senders
        .iter()
        .find(|s| s.name.to_lowercase() == lc)
    {
        return Some(s.clone());
    }
    senders
        .iter()
        .find(|s| s.name.to_lowercase().contains(&lc))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(name: &str) -> ResolvedSender {
        ResolvedSender {
            sender_id: "id".into(),
            name: name.into(),
            host: "127.0.0.1".into(),
            port: 1,
            capabilities: vec![],
            version: 1,
        }
    }

    #[test]
    fn pick_prefers_exact() {
        let list = vec![s("Pixel 8"), s("pixel 8 pro")];
        assert_eq!(pick_by_name(&list, "Pixel 8").unwrap().name, "Pixel 8");
    }

    #[test]
    fn pick_falls_back_to_case_insensitive() {
        let list = vec![s("Pixel 8")];
        assert_eq!(pick_by_name(&list, "pixel 8").unwrap().name, "Pixel 8");
    }

    #[test]
    fn pick_falls_back_to_substring() {
        let list = vec![s("My Pixel 8 (test)")];
        assert_eq!(
            pick_by_name(&list, "pixel").unwrap().name,
            "My Pixel 8 (test)"
        );
    }

    #[test]
    fn pick_misses() {
        let list = vec![s("Pixel 8")];
        assert!(pick_by_name(&list, "Galaxy").is_none());
    }
}
