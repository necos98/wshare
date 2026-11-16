//! LAN discovery of wshare hosts via mDNS / DNS-SD (pure Rust, `mdns-sd`).
//!
//! * A host (an instance that accepts connections) creates an [`Advertiser`] from a
//!   [`HostAnnouncement`]; the announcement is withdrawn when the advertiser is dropped.
//! * A viewer creates a [`Browser`] and feeds its [`DiscoveryEvent`]s into a [`HostList`],
//!   which holds the live, sorted list of hosts for the UI.
//!
//! Pure logic (TXT codec, naming, address selection, event translation, list state) lives in
//! modules that need no network; only [`Advertiser`] and [`Browser`] touch sockets.
//!
//! # Interfaces and addresses
//! Hosts advertise on every non-loopback interface and let `mdns-sd` track address changes
//! (`enable_addr_auto`). `mdns-sd` answers on each interface only with the addresses that
//! belong to that interface's subnet, so addresses of virtual adapters (Hyper-V/WSL vEthernet,
//! VirtualBox, VPNs) are not leaked onto the physical LAN, and a viewer only learns addresses
//! that are on-link for the interface the answer arrived on. No adapter-name heuristics are
//! used (adapter names are localized and user-editable). The viewer then picks an address
//! with [`DiscoveredHost::preferred_address`] (IPv4 before IPv6, private before public,
//! never loopback/unspecified/link-local) and can fall back to the other entries of
//! [`DiscoveredHost::candidate_addresses`].
//!
//! # Names
//! The mDNS instance name is `"{display_name}-{id}"` and the A/AAAA host name is
//! `"{sanitized-computer-name}-{id}.local."`. Including the id keeps both unique when two PCs
//! share a computer name, and keeps our host name from colliding with the `<name>.local`
//! record that Windows' own mDNS responder publishes.

mod address;
mod daemon;
mod event;
mod host_list;
mod mac;
mod names;
mod txt;

pub use address::DiscoveredHost;
pub use daemon::{Advertiser, Browser};
pub use event::DiscoveryEvent;
pub use host_list::HostList;
pub use mac::{MAC_LEN, MacAddress, MacParseError};
pub use names::{computer_name, host_name, instance_name, sanitize_dns_label};
pub use txt::{HostAnnouncement, TxtError};

/// DNS-SD service type announced by wshare hosts.
pub const SERVICE_TYPE: &str = "_wshare._udp.local.";
