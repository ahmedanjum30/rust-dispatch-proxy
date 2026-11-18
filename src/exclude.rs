//! -exclude-file: connections the user switched off (proxy-gui's tick boxes).
//!
//! The file lists adapter names ("Wi-Fi", "Ethernet 2") or IPv4 addresses,
//! one per line; blank lines and lines starting with # are ignored. Names are
//! the better choice: an adapter's address can change (DHCP), its name
//! doesn't. The proxy checks the file every second, so a change applies
//! without a restart, and connections on the links it keeps carry on.

use crate::Via;
use crate::net::Egress;

#[derive(Default, PartialEq)]
pub(crate) struct Rules(Vec<String>);

impl Rules {
    pub(crate) fn parse(text: &str) -> Rules {
        Rules(
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_lowercase)
                .collect(),
        )
    }

    fn matches(&self, e: &Egress) -> bool {
        let (name, ip) = (e.if_name.to_lowercase(), e.ip.to_string());
        self.0.iter().any(|r| *r == name || *r == ip)
    }

    /// Splits candidate links into the ones to use and the adapters switched
    /// off. Upstream proxies (-tunnel) are never switched off this way.
    pub(crate) fn split(&self, candidates: Vec<(Via, u64)>) -> (Vec<(Via, u64)>, Vec<Egress>) {
        let mut used = Vec::new();
        let mut off = Vec::new();
        for (via, weight) in candidates {
            match via {
                Via::Adapter(e) if self.matches(&e) => off.push(e),
                via => used.push((via, weight)),
            }
        }
        (used, off)
    }
}

/// The file's text; empty when there is no file (nothing switched off).
pub(crate) fn read(path: Option<&str>) -> String {
    path.and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn adapter(name: &str, last: u8) -> (Via, u64) {
        let e = Egress {
            ip: Ipv4Addr::new(192, 168, 1, last),
            if_index: None,
            if_name: name.into(),
        };
        (Via::Adapter(e), 1)
    }

    #[test]
    fn switches_off_by_name_or_address() {
        let rules = Rules::parse("# switched off in proxy-gui\nwi-fi\n\n 192.168.1.3 \n");
        let (used, off) = rules.split(vec![adapter("Wi-Fi", 1), adapter("Ethernet", 2), adapter("Ethernet 2", 3)]);
        assert_eq!(used, vec![adapter("Ethernet", 2)]);
        assert_eq!(off.iter().map(|e| e.if_name.as_str()).collect::<Vec<_>>(), ["Wi-Fi", "Ethernet 2"]);
    }

    #[test]
    fn empty_file_uses_everything() {
        let (used, off) = Rules::parse("").split(vec![adapter("Wi-Fi", 1)]);
        assert_eq!(used.len(), 1);
        assert!(off.is_empty());
        assert_eq!(read(Some("C:/definitely/not/here.txt")), "");
    }
}
