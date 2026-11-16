//! The computers of the main list: those announced on the LAN and the ones
//! this PC knows (it connected to them before) but does not see now, shown
//! as not online. Pure.

use std::cmp::Reverse;
use std::net::SocketAddr;

use discovery::{DiscoveredHost, HostList, MacAddress};
use protocol::control::PROTOCOL_VERSION;

use crate::text;
use crate::trust::{KnownHost, Timestamp};

use super::elapsed;
use super::remembered::RememberedHosts;

/// A computer of the list, ready to display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRow {
    pub id: String,
    pub name: String,
    pub resolution: String,
    /// Where to connect: the announced address, else the last one known.
    pub address: Option<SocketAddr>,
    /// Same protocol version as this program (assumed for a computer that
    /// is not online, which cannot say).
    pub compatible: bool,
    /// This PC connects to it without a PIN.
    pub remembered: bool,
    /// Announcing itself on the LAN now.
    pub online: bool,
    /// This PC has connected to it before.
    pub known: bool,
    pub favorite: bool,
    pub last_connected: Option<Timestamp>,
    /// Its hardware address, announced or remembered: what Wake-on-LAN needs.
    pub mac: Option<MacAddress>,
}

impl HostRow {
    fn online(host: &DiscoveredHost, known: Option<&KnownHost>, remembered: bool) -> Self {
        let announced = &host.announcement;
        Self {
            id: announced.id.clone(),
            name: announced.display_name.clone(),
            resolution: format!("{}×{}", announced.width, announced.height),
            address: host
                .preferred_address()
                .or_else(|| known.and_then(|k| k.address)),
            compatible: announced.protocol_version == PROTOCOL_VERSION,
            remembered,
            online: true,
            known: known.is_some(),
            favorite: known.is_some_and(|k| k.favorite),
            last_connected: known.and_then(|k| k.last_connected),
            mac: announced.mac.or_else(|| known.and_then(|k| k.mac)),
        }
    }

    fn offline(known: &KnownHost, remembered: bool) -> Self {
        Self {
            id: known.fingerprint.host_id(),
            name: known.name.clone(),
            resolution: String::new(),
            address: known.address,
            compatible: true,
            remembered,
            online: false,
            known: true,
            favorite: known.favorite,
            last_connected: known.last_connected,
            mac: known.mac,
        }
    }

    pub fn connectable(&self) -> bool {
        self.compatible && self.address.is_some()
    }

    /// Whether the row can be clicked: something can be done with it
    /// (connecting, or for a known computer also waking or removing it).
    pub fn selectable(&self) -> bool {
        self.connectable() || self.known
    }

    /// Whether the row offers Wake-on-LAN: a known computer that is not
    /// answering. It can be woken only if its MAC is known.
    pub fn wakeable(&self) -> bool {
        !self.online
    }

    /// Second line of the row.
    pub fn details(&self, now: Timestamp) -> String {
        if !self.compatible {
            return text::HOST_INCOMPATIBLE.to_string();
        }
        let line = if self.online {
            text::host_details(&self.resolution, self.address, self.remembered)
        } else {
            text::host_offline_details(self.address)
        };
        match self.last_connected {
            Some(when) => format!(
                "{line} · {}",
                text::host_last_connection(&elapsed::ago(when, now))
            ),
            None => line,
        }
    }
}

/// The rows to show, in order: favorites first, then the computers online,
/// then the most recently connected, then by name. `own_id` (this PC's own
/// announcement) is left out.
pub fn rows(hosts: &HostList, remembered: &RememberedHosts, own_id: Option<&str>) -> Vec<HostRow> {
    let others = |id: &str| Some(id) != own_id;
    let online = hosts
        .hosts()
        .into_iter()
        .filter(|host| others(host.id()))
        .map(|host| {
            HostRow::online(
                host,
                remembered.known(host.id()),
                remembered.is_remembered(host.id()),
            )
        });
    let offline = remembered
        .all()
        .iter()
        .filter(|known| !hosts.hosts().iter().any(|host| known.matches_id(host.id())))
        .filter(|known| others(&known.fingerprint.host_id()))
        .map(|known| {
            let remembered = remembered.is_remembered(&known.fingerprint.host_id());
            HostRow::offline(known, remembered)
        });
    let mut rows: Vec<HostRow> = online.chain(offline).collect();
    rows.sort_by_cached_key(|row| {
        (
            Reverse(row.favorite),
            Reverse(row.online),
            Reverse(row.last_connected),
            row.name.to_lowercase(),
            row.id.clone(),
        )
    });
    rows
}

#[cfg(test)]
mod tests {
    use discovery::HostAnnouncement;
    use net::Fingerprint;

    use super::*;

    fn at(seconds: u64) -> Timestamp {
        Timestamp::from_unix_seconds(seconds)
    }

    fn known(byte: u8, name: &str) -> KnownHost {
        KnownHost::new(Fingerprint([byte; 32]), name)
    }

    fn announced(known: &KnownHost, name: &str, mac: Option<&str>) -> DiscoveredHost {
        DiscoveredHost::new(
            HostAnnouncement {
                id: known.fingerprint.host_id(),
                display_name: name.into(),
                port: 7420,
                protocol_version: PROTOCOL_VERSION,
                width: 1920,
                height: 1080,
                mac: mac.and_then(|mac| mac.parse().ok()),
            },
            vec!["192.168.1.9".parse().unwrap()],
        )
    }

    fn list(hosts: &[DiscoveredHost]) -> HostList {
        let mut list = HostList::default();
        for host in hosts {
            list.apply(discovery::DiscoveryEvent::Found(host.clone()));
        }
        list
    }

    fn remembering(hosts: Vec<KnownHost>) -> RememberedHosts {
        let mut remembered = RememberedHosts::default();
        remembered.set(hosts, None);
        remembered
    }

    fn names(rows: &[HostRow]) -> Vec<&str> {
        rows.iter().map(|row| row.name.as_str()).collect()
    }

    #[test]
    fn a_known_host_that_is_not_announced_is_offline_at_its_last_address() {
        let mut office = known(1, "UFFICIO");
        office.address = Some("10.0.0.5:7420".parse().unwrap());
        office.mac = "aa:bb:cc:00:11:22".parse().ok();
        let rows = rows(&list(&[]), &remembering(vec![office.clone()]), None);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.id, office.fingerprint.host_id());
        assert!(!row.online && row.known && row.remembered && row.wakeable());
        assert!(row.connectable(), "the last address is tried");
        assert_eq!(row.address, office.address);
        assert_eq!(row.mac, office.mac);
        assert_eq!(
            row.details(at(100)),
            "Non in rete · ultimo indirizzo 10.0.0.5:7420"
        );
    }

    #[test]
    fn without_a_last_address_an_offline_host_cannot_be_connected() {
        let row = HostRow::offline(&known(1, "UFFICIO"), true);
        assert!(!row.connectable());
        assert_eq!(row.details(at(0)), "Non in rete · nessun indirizzo noto");
    }

    #[test]
    fn the_last_connection_is_shown_relative_to_now() {
        let mut office = known(1, "UFFICIO");
        office.address = Some("10.0.0.5:7420".parse().unwrap());
        office.last_connected = Some(at(1_000));
        let row = HostRow::offline(&office, true);
        assert_eq!(
            row.details(at(1_000 + 5 * 60)),
            "Non in rete · ultimo indirizzo 10.0.0.5:7420 · ultima connessione 5 minuti fa"
        );
    }

    #[test]
    fn an_announced_known_host_is_one_online_row_with_the_announced_data() {
        let mut office = known(1, "UFFICIO");
        office.address = Some("10.0.0.5:7420".parse().unwrap());
        office.mac = "aa:bb:cc:00:11:22".parse().ok();
        office.favorite = true;
        let announced = announced(&office, "UFFICIO", None);
        let rows = rows(
            &list(&[announced]),
            &remembering(vec![office.clone()]),
            None,
        );
        assert_eq!(rows.len(), 1, "not listed twice");
        let row = &rows[0];
        assert!(row.online && row.known && row.favorite && !row.wakeable());
        assert_eq!(row.address, Some("192.168.1.9:7420".parse().unwrap()));
        assert_eq!(row.mac, office.mac, "an older host keeps the known MAC");
    }

    #[test]
    fn the_announced_mac_wins_over_the_remembered_one() {
        let mut office = known(1, "UFFICIO");
        office.mac = "aa:bb:cc:00:11:22".parse().ok();
        let announced = announced(&office, "UFFICIO", Some("aa:bb:cc:00:11:33"));
        let rows = rows(&list(&[announced]), &remembering(vec![office]), None);
        assert_eq!(rows[0].mac, "aa:bb:cc:00:11:33".parse().ok());
    }

    #[test]
    fn a_host_announced_without_an_address_falls_back_to_the_last_one() {
        let mut office = known(1, "UFFICIO");
        office.address = Some("10.0.0.5:7420".parse().unwrap());
        let host = DiscoveredHost::new(announced(&office, "UFFICIO", None).announcement, vec![]);
        let rows = rows(&list(&[host]), &remembering(vec![office.clone()]), None);
        assert_eq!(rows[0].address, office.address);
    }

    #[test]
    fn own_computer_is_left_out_online_and_offline() {
        let me = known(1, "QUESTO-PC");
        let id = me.fingerprint.host_id();
        let remembered = remembering(vec![me.clone()]);
        assert!(rows(&list(&[]), &remembered, Some(&id)).is_empty());
        let online = list(&[announced(&me, "QUESTO-PC", None)]);
        assert!(rows(&online, &remembered, Some(&id)).is_empty());
    }

    #[test]
    fn rows_sort_favorites_then_online_then_recent_then_name() {
        let mut offline_old = known(1, "Zeta");
        offline_old.last_connected = Some(at(100));
        let mut offline_recent = known(2, "Yota");
        offline_recent.last_connected = Some(at(900));
        let mut offline_favorite = known(3, "Xi");
        offline_favorite.favorite = true;
        let online_b = known(4, "beta");
        let online_a = known(5, "Alfa");
        let online = list(&[
            announced(&online_b, "beta", None),
            announced(&online_a, "Alfa", None),
        ]);
        let remembered = remembering(vec![
            offline_old,
            offline_recent,
            offline_favorite,
            online_b,
            online_a,
        ]);
        let rows = rows(&online, &remembered, None);
        assert_eq!(names(&rows), ["Xi", "Alfa", "beta", "Yota", "Zeta"]);
    }

    #[test]
    fn a_recent_connection_ranks_above_a_never_connected_host_of_the_same_kind() {
        let mut recent = known(1, "Zeta");
        recent.last_connected = Some(at(5));
        let never = known(2, "Alfa");
        let rows = rows(&list(&[]), &remembering(vec![never, recent]), None);
        assert_eq!(names(&rows), ["Zeta", "Alfa"]);
    }

    #[test]
    fn an_incompatible_host_says_so() {
        let office = known(1, "UFFICIO");
        let mut host = announced(&office, "UFFICIO", None);
        host.announcement.protocol_version = PROTOCOL_VERSION + 1;
        let rows = rows(&list(&[host]), &remembering(vec![]), None);
        assert!(!rows[0].connectable());
        assert!(rows[0].details(at(0)).contains("non compatibile"));
    }
}
