//! mDNS for host mode (design §8.3, §8.6): a host advertises `_privatevox._tcp.local.`; a client lists what it finds
//! and the user adds a host by hand from that list. Nothing found is used until added.

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const SERVICE_TYPE: &str = "_privatevox._tcp.local.";

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct FoundHost {
    pub name: String,
    pub address: String,
    pub port: u16,
    pub version: String,
}

pub fn txt_properties(name: &str, port: u16, version: &str) -> Vec<(String, String)> {
    vec![("name".into(), name.into()), ("port".into(), port.to_string()), ("version".into(), version.into())]
}

/// One entry per address:port; the latest name for an address wins.
pub fn merge(found: Vec<FoundHost>) -> Vec<FoundHost> {
    let mut by_key: HashMap<(String, u16), FoundHost> = HashMap::new();
    let mut order: Vec<(String, u16)> = Vec::new();
    for h in found {
        let key = (h.address.clone(), h.port);
        if !by_key.contains_key(&key) {
            order.push(key.clone());
        }
        by_key.insert(key, h);
    }
    order.into_iter().filter_map(|k| by_key.remove(&k)).collect()
}

/// Drop entries that are this machine (its own LAN addresses).
pub fn without_self(found: Vec<FoundHost>, own_addresses: &[String]) -> Vec<FoundHost> {
    found.into_iter().filter(|h| !own_addresses.iter().any(|a| a == &h.address)).collect()
}

/// Keeps the host's service registered while it lives.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// Register this host on the LAN. `addresses` are its IPv4 addresses (from `host::lan_addresses`).
pub fn advertise(name: &str, port: u16, version: &str, addresses: &[String]) -> anyhow::Result<Advertiser> {
    let daemon = ServiceDaemon::new()?;
    let instance: String = name.chars().filter(|c| *c != '.').collect::<String>().trim().to_string();
    let instance = if instance.is_empty() { "PrivateVox".to_string() } else { instance };
    let host_label: String = instance.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let host_name = format!("{host_label}.local.");
    let ips = addresses.join(",");
    // enable_addr_auto: the daemon re-announces when interfaces change (Wi-Fi switch), so the advertised address stays right.
    let info = ServiceInfo::new(SERVICE_TYPE, &instance, &host_name, ips.as_str(), port, &txt_properties(name, port, version)[..])?.enable_addr_auto();
    let fullname = info.get_fullname().to_string();
    daemon.register(info)?;
    log::info!("mdns: advertising {fullname} on port {port}");
    Ok(Advertiser { daemon, fullname })
}

/// Browse for hosts for `wait`, then stop. Includes this machine when it advertises; callers filter with `without_self`.
pub fn browse(wait: Duration) -> anyhow::Result<Vec<FoundHost>> {
    let daemon = ServiceDaemon::new()?;
    let rx = daemon.browse(SERVICE_TYPE)?;
    let deadline = Instant::now() + wait;
    let mut found = Vec::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let name = info.get_property_val_str("name").unwrap_or(info.get_fullname()).to_string();
                let version = info.get_property_val_str("version").unwrap_or("").to_string();
                let port = info.get_port();
                for ip in info.get_addresses_v4() {
                    found.push(FoundHost { name: name.clone(), address: ip.to_string(), port, version: version.clone() });
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = daemon.stop_browse(SERVICE_TYPE);
    let _ = daemon.shutdown();
    Ok(merge(found))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fh(name: &str, address: &str, port: u16) -> FoundHost {
        FoundHost { name: name.into(), address: address.into(), port, version: "0.3.0".into() }
    }

    #[test]
    fn found_hosts_are_keyed_by_address() {
        let merged = merge(vec![fh("PC", "192.168.1.20", 8765), fh("PC", "192.168.1.21", 8765), fh("PC renamed", "192.168.1.20", 8765), fh("PC", "192.168.1.20", 8766)]);
        assert_eq!(merged.len(), 3, "{merged:?}");
        let first = merged.iter().find(|h| h.address == "192.168.1.20" && h.port == 8765).unwrap();
        assert_eq!(first.name, "PC renamed", "the latest name for an address wins");
        assert!(merged.iter().any(|h| h.address == "192.168.1.21"));
        assert!(merged.iter().any(|h| h.port == 8766));
    }

    #[test]
    fn merge_drops_own_addresses() {
        let merged = without_self(vec![fh("Me", "192.168.1.5", 8765), fh("PC", "192.168.1.20", 8765)], &["192.168.1.5".to_string()]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].address, "192.168.1.20");
    }

    #[test]
    fn service_type_and_txt_shape() {
        assert_eq!(SERVICE_TYPE, "_privatevox._tcp.local.");
        let props = txt_properties("Roy's PC", 8765, "0.4.0");
        assert_eq!(props.iter().find(|(k, _)| k == "name").map(|(_, v)| v.as_str()), Some("Roy's PC"));
        assert_eq!(props.iter().find(|(k, _)| k == "port").map(|(_, v)| v.as_str()), Some("8765"));
        assert_eq!(props.iter().find(|(k, _)| k == "version").map(|(_, v)| v.as_str()), Some("0.4.0"));
    }

    /// `cargo test -p privatevox --lib live_advertise_and_browse -- --ignored --nocapture`: real multicast on this machine.
    #[test]
    #[ignore]
    fn live_advertise_and_browse() {
        let own = crate::host::lan_addresses();
        assert!(!own.is_empty(), "no LAN address on this machine");
        let _a = advertise("Live Test Host", 8765, "0.4.0", &own).unwrap();
        let found = browse(std::time::Duration::from_secs(3)).unwrap();
        println!("found: {found:?}");
        assert!(found.iter().any(|h| h.name == "Live Test Host" && h.port == 8765 && own.contains(&h.address)), "{found:?}");
    }
}
