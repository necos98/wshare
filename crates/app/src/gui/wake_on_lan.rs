//! Wake-on-LAN: the magic packet and where it is sent.
//!
//! A computer that is off (and set up for it in its BIOS and network card)
//! wakes when its network card sees, in any UDP datagram, six bytes of
//! `0xFF` followed by its own MAC sixteen times. The datagram is broadcast
//! on the LAN to port 9: to the whole link, and to the broadcast address of
//! the network the computer was last seen on.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};

use anyhow::{Context, Result, bail};
use discovery::{MAC_LEN, MacAddress};

use super::ports::WakeOnLan;

/// The "discard" port, by convention the one magic packets go to.
pub const WAKE_PORT: u16 = 9;
const SYNC_LEN: usize = 6;
const MAC_REPEATS: usize = 16;
/// Size of a magic packet.
pub const MAGIC_PACKET_LEN: usize = SYNC_LEN + MAC_REPEATS * MAC_LEN;

/// The datagram that wakes the card with `mac`.
pub fn magic_packet(mac: MacAddress) -> [u8; MAGIC_PACKET_LEN] {
    let mut packet = [0xff; MAGIC_PACKET_LEN];
    for repeat in packet[SYNC_LEN..].chunks_exact_mut(MAC_LEN) {
        repeat.copy_from_slice(&mac.octets());
    }
    packet
}

/// An IPv4 address of this PC and the mask of its network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalNetwork {
    pub address: Ipv4Addr,
    pub mask: Ipv4Addr,
}

impl LocalNetwork {
    fn contains(&self, other: Ipv4Addr) -> bool {
        let mask = u32::from(self.mask);
        u32::from(self.address) & mask == u32::from(other) & mask
    }

    fn broadcast(&self, member: Ipv4Addr) -> Ipv4Addr {
        let mask = u32::from(self.mask);
        Ipv4Addr::from(u32::from(member) | !mask)
    }
}

/// Where to send the packet: the limited broadcast (the link the system
/// routes it to) and, when `last_address` lies in one of this PC's
/// networks, that network's broadcast address too. IPv6 has no broadcast.
pub fn targets(last_address: Option<IpAddr>, networks: &[LocalNetwork]) -> Vec<SocketAddrV4> {
    let limited = Ipv4Addr::BROADCAST;
    let directed = match last_address {
        Some(IpAddr::V4(last)) => networks
            .iter()
            .find(|network| network.contains(last))
            .map(|network| network.broadcast(last)),
        _ => None,
    };
    std::iter::once(limited)
        .chain(directed.filter(|&address| address != limited))
        .map(|address| SocketAddrV4::new(address, WAKE_PORT))
        .collect()
}

/// The IPv4 networks of this PC's interfaces, loopback excluded. When they
/// cannot be listed only the limited broadcast remains, which still wakes
/// a computer on the main link.
fn local_networks() -> Vec<LocalNetwork> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|interface| match interface.addr {
            if_addrs::IfAddr::V4(v4) if !v4.ip.is_loopback() => Some(LocalNetwork {
                address: v4.ip,
                mask: v4.netmask,
            }),
            _ => None,
        })
        .collect()
}

pub struct UdpWakeOnLan;

impl WakeOnLan for UdpWakeOnLan {
    fn wake(&self, mac: MacAddress, last_address: Option<IpAddr>) -> Result<()> {
        let targets = targets(last_address, &local_networks());
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).context("opening a socket")?;
        socket
            .set_broadcast(true)
            .context("allowing broadcast on the socket")?;
        send_all(&socket, &magic_packet(mac), &targets)
    }
}

/// Sends `packet` to every target. It is enough that one goes out; if none
/// does, the first error is the answer.
fn send_all(socket: &UdpSocket, packet: &[u8], targets: &[SocketAddrV4]) -> Result<()> {
    let mut first_error: Option<(SocketAddr, io::Error)> = None;
    let mut sent = false;
    for target in targets {
        match socket.send_to(packet, target) {
            Ok(_) => sent = true,
            Err(e) => {
                first_error.get_or_insert((SocketAddr::V4(*target), e));
            }
        }
    }
    match (sent, first_error) {
        (true, _) => Ok(()),
        (false, Some((target, e))) => Err(e).with_context(|| format!("sending to {target}")),
        (false, None) => bail!("there is nowhere to send the signal"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac() -> MacAddress {
        "aa:bb:cc:00:11:22".parse().unwrap()
    }

    fn ip(text: &str) -> Ipv4Addr {
        text.parse().unwrap()
    }

    fn network(address: &str, mask: &str) -> LocalNetwork {
        LocalNetwork {
            address: ip(address),
            mask: ip(mask),
        }
    }

    #[test]
    fn the_magic_packet_is_six_ff_then_the_mac_sixteen_times() {
        let packet = magic_packet(mac());
        assert_eq!(packet.len(), 102);
        assert_eq!(packet[..6], [0xff; 6]);
        for repeat in packet[6..].chunks(6) {
            assert_eq!(repeat, [0xaa, 0xbb, 0xcc, 0x00, 0x11, 0x22]);
        }
        assert_eq!(packet[6..].chunks(6).count(), 16);
    }

    #[test]
    fn the_limited_broadcast_is_always_a_target_on_port_9() {
        let targets = targets(None, &[]);
        assert_eq!(targets, [SocketAddrV4::new(Ipv4Addr::BROADCAST, 9)]);
    }

    #[test]
    fn the_last_network_adds_its_directed_broadcast() {
        let networks = [
            network("10.0.0.2", "255.0.0.0"),
            network("192.168.1.20", "255.255.255.0"),
        ];
        let last = Some(IpAddr::V4(ip("192.168.1.77")));
        assert_eq!(
            targets(last, &networks),
            [
                SocketAddrV4::new(Ipv4Addr::BROADCAST, 9),
                SocketAddrV4::new(ip("192.168.1.255"), 9),
            ]
        );
    }

    #[test]
    fn a_network_that_is_not_ours_or_not_ipv4_adds_nothing() {
        let networks = [network("192.168.1.20", "255.255.255.0")];
        let elsewhere = Some(IpAddr::V4(ip("172.16.0.5")));
        let v6 = Some(IpAddr::V6("fd00::5".parse().unwrap()));
        for last in [elsewhere, v6, None] {
            assert_eq!(targets(last, &networks).len(), 1, "{last:?}");
        }
    }

    #[test]
    fn directed_broadcast_follows_the_mask() {
        let networks = [network("172.16.5.9", "255.255.0.0")];
        let last = Some(IpAddr::V4(ip("172.16.200.3")));
        assert_eq!(
            targets(last, &networks)[1],
            SocketAddrV4::new(ip("172.16.255.255"), 9)
        );
    }

    #[test]
    fn the_signal_reaches_a_listening_socket() {
        let listener = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let target = match listener.local_addr().unwrap() {
            SocketAddr::V4(address) => address,
            SocketAddr::V6(_) => unreachable!("bound to an IPv4 address"),
        };
        send_all(&sender, &magic_packet(mac()), &[target]).unwrap();
        let mut received = [0u8; 256];
        let len = listener.recv(&mut received).unwrap();
        assert_eq!(received[..len], magic_packet(mac()));
    }

    #[test]
    fn sending_with_no_target_is_an_error() {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        assert!(send_all(&socket, &magic_packet(mac()), &[]).is_err());
    }
}
