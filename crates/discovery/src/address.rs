//! [`DiscoveredHost`] and address selection.

use std::net::{IpAddr, SocketAddr};

use crate::HostAnnouncement;

/// A host seen on the LAN: its announcement plus the addresses it resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredHost {
    /// What the host announced.
    pub announcement: HostAnnouncement,
    /// All addresses received, sorted by preference (see [`DiscoveredHost::preferred_address`]),
    /// unusable ones last.
    pub addresses: Vec<IpAddr>,
}

impl DiscoveredHost {
    /// Builds a host, sorting `addresses` by preference and removing duplicates.
    pub fn new(announcement: HostAnnouncement, mut addresses: Vec<IpAddr>) -> Self {
        addresses.sort_by_key(|a| (rank(a), *a));
        addresses.dedup();
        Self {
            announcement,
            addresses,
        }
    }

    /// The stable host id (shorthand for `announcement.id`).
    pub fn id(&self) -> &str {
        &self.announcement.id
    }

    /// Usable socket addresses in preference order: IPv4 private (RFC 1918), other IPv4
    /// (e.g. CGNAT/VPN), IPv6 unique-local, other IPv6. Loopback, unspecified, link-local
    /// and multicast addresses are skipped.
    pub fn candidate_addresses(&self) -> Vec<SocketAddr> {
        let mut usable: Vec<IpAddr> = self
            .addresses
            .iter()
            .copied()
            .filter(|a| rank(a) < UNUSABLE)
            .collect();
        usable.sort_by_key(|a| (rank(a), *a));
        usable
            .into_iter()
            .map(|ip| SocketAddr::new(ip, self.announcement.port))
            .collect()
    }

    /// The best address to connect to, if any (first of [`Self::candidate_addresses`]).
    pub fn preferred_address(&self) -> Option<SocketAddr> {
        self.candidate_addresses().into_iter().next()
    }
}

const UNUSABLE: u8 = 4;

/// Lower is better; [`UNUSABLE`] means never connect to it.
fn rank(addr: &IpAddr) -> u8 {
    match addr {
        IpAddr::V4(v4) => {
            if v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast()
            {
                UNUSABLE
            } else if v4.is_private() {
                0
            } else {
                1
            }
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return rank(&IpAddr::V4(v4));
            }
            if v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unicast_link_local()
                || v6.is_multicast()
            {
                UNUSABLE
            } else if v6.is_unique_local() {
                2
            } else {
                3
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::txt::sample;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn host(addrs: &[&str]) -> DiscoveredHost {
        DiscoveredHost::new(sample("abc", "x"), addrs.iter().map(|s| ip(s)).collect())
    }

    #[test]
    fn prefers_private_ipv4() {
        let h = host(&["fd00::1", "2001:db8::1", "100.64.0.5", "192.168.1.20"]);
        assert_eq!(
            h.preferred_address(),
            Some(SocketAddr::new(ip("192.168.1.20"), 4433))
        );
        let order: Vec<IpAddr> = h.candidate_addresses().iter().map(|s| s.ip()).collect();
        assert_eq!(
            order,
            [
                ip("192.168.1.20"),
                ip("100.64.0.5"),
                ip("fd00::1"),
                ip("2001:db8::1")
            ]
        );
    }

    #[test]
    fn skips_unusable() {
        let h = host(&[
            "127.0.0.1",
            "0.0.0.0",
            "169.254.3.4",
            "::1",
            "::",
            "fe80::1",
            "224.0.0.251",
            "::ffff:127.0.0.1",
        ]);
        assert_eq!(h.preferred_address(), None);
        assert!(h.candidate_addresses().is_empty());
    }

    #[test]
    fn ipv6_when_no_ipv4() {
        let h = host(&["fe80::1", "2001:db8::7"]);
        assert_eq!(
            h.preferred_address(),
            Some(SocketAddr::new(ip("2001:db8::7"), 4433))
        );
    }

    #[test]
    fn addresses_sorted_and_deduped() {
        let a = host(&["10.0.0.2", "fe80::1", "10.0.0.1", "10.0.0.2"]);
        let b = host(&["fe80::1", "10.0.0.1", "10.0.0.2"]);
        assert_eq!(a, b);
        assert_eq!(a.addresses, [ip("10.0.0.1"), ip("10.0.0.2"), ip("fe80::1")]);
    }

    #[test]
    fn no_addresses() {
        assert_eq!(host(&[]).preferred_address(), None);
    }
}
