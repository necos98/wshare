//! Viewer side: the hosts that admitted this PC, pinned by their full
//! certificate fingerprint on later connections, with what is needed to
//! find them again: the last address, the MAC to wake them and when this
//! PC last connected.
//!
//! Several processes use the file (the desktop interface and each viewer
//! it launches), so every change re-reads it first and only then writes:
//! one process does not undo what another just saved.
//!
//! Every field added after the first version is optional, so a file written
//! by an older version still reads (see `old_files_without_the_new_fields_read`).

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use discovery::MacAddress;
use net::Fingerprint;
use serde::{Deserialize, Serialize};

use super::Timestamp;
use super::store;

const HEADER: &str = "wshare: computers this PC connected to; they are pinned by fingerprint\n\
                      and connected to without a PIN. Delete an entry (or use \"Rimuovi\").";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownHost {
    #[serde(with = "super::fingerprint_text")]
    pub fingerprint: Fingerprint,
    /// The name the host gave at the last connection (a label only).
    pub name: String,
    /// Where it was last seen or connected to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<SocketAddr>,
    /// Hardware address announced by the host, to wake it with Wake-on-LAN.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "mac_text")]
    pub mac: Option<MacAddress>,
    /// When this PC last connected to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_connected: Option<Timestamp>,
    /// Pinned to the top of the list by the user.
    #[serde(default, skip_serializing_if = "is_false")]
    pub favorite: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl KnownHost {
    pub fn new(fingerprint: Fingerprint, name: &str) -> Self {
        Self {
            fingerprint,
            name: name.to_string(),
            address: None,
            mac: None,
            last_connected: None,
            favorite: false,
        }
    }

    /// Whether this is the host announcing itself as `id` on the LAN.
    pub fn matches_id(&self, id: &str) -> bool {
        Fingerprint::is_host_id(id) && self.fingerprint.host_id().eq_ignore_ascii_case(id)
    }

    /// Whether what the host announces now is already stored. An
    /// announcement without an address or a MAC (an older host) says
    /// nothing about them, so it never counts as a difference.
    pub fn knows_announcement(&self, address: Option<SocketAddr>, mac: Option<MacAddress>) -> bool {
        (address.is_none() || self.address == address) && (mac.is_none() || self.mac == mac)
    }
}

/// `#[serde(with = ...)]` for an optional [`MacAddress`] as text. A text
/// that is not a MAC reads as none: it is a convenience, not worth
/// discarding the whole file (and the PIN-less access of every host) for.
mod mac_text {
    use discovery::MacAddress;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(mac: &Option<MacAddress>, s: S) -> Result<S::Ok, S::Error> {
        match mac {
            Some(mac) => s.collect_str(mac),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<MacAddress>, D::Error> {
        let text = Option::<String>::deserialize(d)?;
        Ok(text.and_then(|text| text.parse().ok()))
    }
}

/// Layout of the file.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Contents {
    #[serde(default, rename = "host")]
    hosts: Vec<KnownHost>,
}

#[derive(Debug)]
pub struct KnownHosts {
    path: PathBuf,
    hosts: Vec<KnownHost>,
}

impl KnownHosts {
    /// Loads the list from `path`. A damaged file counts as empty (hosts
    /// are no longer pinned and ask for the PIN again); the second value
    /// says why.
    pub fn load(path: PathBuf) -> (Self, Option<String>) {
        let loaded = store::load::<Contents>(&path);
        let hosts = loaded.value.hosts;
        (Self { path, hosts }, loaded.warning)
    }

    pub fn hosts(&self) -> &[KnownHost] {
        &self.hosts
    }

    pub fn find_by_id(&self, id: &str) -> Option<&KnownHost> {
        self.hosts.iter().find(|h| h.matches_id(id))
    }

    /// Remembers the host with `fingerprint` after a connection to it at
    /// `address`: its name, where it was and when.
    pub fn record_connection(
        &mut self,
        fingerprint: Fingerprint,
        name: &str,
        address: SocketAddr,
        now: Timestamp,
    ) -> Result<()> {
        self.update(|hosts| {
            let host = find_or_add(hosts, fingerprint, name);
            host.name = name.to_string();
            host.address = Some(address);
            host.last_connected = Some(now);
            true
        })
    }

    /// Stores what the host announces now (its address and MAC). Nothing
    /// is written when the host is unknown or already up to date.
    pub fn note_announced(
        &mut self,
        fingerprint: &Fingerprint,
        address: Option<SocketAddr>,
        mac: Option<MacAddress>,
    ) -> Result<()> {
        self.update(|hosts| {
            let Some(host) = hosts.iter_mut().find(|h| &h.fingerprint == fingerprint) else {
                return false;
            };
            if host.knows_announcement(address, mac) {
                return false;
            }
            host.address = address.or(host.address);
            host.mac = mac.or(host.mac);
            true
        })
    }

    /// Pins the host to the top of the list (or takes it off).
    pub fn set_favorite(&mut self, fingerprint: &Fingerprint, favorite: bool) -> Result<()> {
        self.update(|hosts| {
            let Some(host) = hosts
                .iter_mut()
                .find(|h| &h.fingerprint == fingerprint && h.favorite != favorite)
            else {
                return false;
            };
            host.favorite = favorite;
            true
        })
    }

    /// Forgets the host: it is neither pinned nor connected to without a
    /// PIN any more.
    pub fn forget(&mut self, fingerprint: &Fingerprint) -> Result<()> {
        self.update(|hosts| {
            let before = hosts.len();
            hosts.retain(|h| &h.fingerprint != fingerprint);
            hosts.len() != before
        })
    }

    /// Re-reads the file, applies `change` and saves if it reports a
    /// change. A file damaged in the meantime is replaced: what it held
    /// could not be used anyway, and the damage was reported when loading.
    fn update(&mut self, change: impl FnOnce(&mut Vec<KnownHost>) -> bool) -> Result<()> {
        self.hosts = store::load::<Contents>(&self.path).value.hosts;
        if !change(&mut self.hosts) {
            return Ok(());
        }
        let contents = Contents {
            hosts: self.hosts.clone(),
        };
        store::save(&self.path, HEADER, &contents)
    }
}

fn find_or_add<'a>(
    hosts: &'a mut Vec<KnownHost>,
    fingerprint: Fingerprint,
    name: &str,
) -> &'a mut KnownHost {
    let at = hosts
        .iter()
        .position(|h| h.fingerprint == fingerprint)
        .unwrap_or_else(|| {
            hosts.push(KnownHost::new(fingerprint, name));
            hosts.len() - 1
        });
    &mut hosts[at]
}

#[cfg(test)]
mod tests {
    use std::fs;

    use net::Identity;

    use super::*;

    fn fingerprint() -> Fingerprint {
        Identity::generate("host").unwrap().fingerprint()
    }

    fn address(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    fn mac(text: &str) -> Option<MacAddress> {
        text.parse().ok()
    }

    fn at(seconds: u64) -> Timestamp {
        Timestamp::from_unix_seconds(seconds)
    }

    fn store_in(dir: &tempfile::TempDir) -> (PathBuf, KnownHosts) {
        let path = dir.path().join("known_hosts.toml");
        let (hosts, warning) = KnownHosts::load(path.clone());
        assert_eq!(warning, None);
        (path, hosts)
    }

    #[test]
    fn remembered_hosts_are_found_by_their_lan_id() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mut hosts) = store_in(&dir);
        let office = fingerprint();
        hosts
            .record_connection(office, "UFFICIO", address("10.0.0.5:7420"), at(100))
            .unwrap();

        let (reloaded, _) = KnownHosts::load(path);
        let id = office.host_id();
        let found = reloaded.find_by_id(&id).expect("known");
        assert_eq!(found.name, "UFFICIO");
        assert!(reloaded.find_by_id(&id.to_uppercase()).is_some());
        assert!(reloaded.find_by_id(&fingerprint().host_id()).is_none());
        assert!(
            reloaded.find_by_id("").is_none(),
            "an empty id matches nothing"
        );
        assert!(
            reloaded.find_by_id(&id[..8]).is_none(),
            "a short prefix matches nothing"
        );
    }

    #[test]
    fn a_connection_stores_name_address_and_time_and_renames() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mut hosts) = store_in(&dir);
        let office = fingerprint();
        hosts
            .record_connection(office, "UFFICIO", address("10.0.0.5:7420"), at(100))
            .unwrap();
        hosts
            .record_connection(office, "UFFICIO-2", address("10.0.0.9:7420"), at(200))
            .unwrap();
        let (reloaded, _) = KnownHosts::load(path);
        assert_eq!(reloaded.hosts().len(), 1);
        let host = &reloaded.hosts()[0];
        assert_eq!(host.name, "UFFICIO-2");
        assert_eq!(host.address, Some(address("10.0.0.9:7420")));
        assert_eq!(host.last_connected, Some(at(200)));
        assert!(!host.favorite);
    }

    #[test]
    fn a_connection_keeps_what_the_user_and_the_announcement_added() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mut hosts) = store_in(&dir);
        let office = fingerprint();
        hosts
            .record_connection(office, "UFFICIO", address("10.0.0.5:7420"), at(1))
            .unwrap();
        hosts
            .note_announced(&office, None, mac("aa:bb:cc:00:11:22"))
            .unwrap();
        hosts.set_favorite(&office, true).unwrap();
        hosts
            .record_connection(office, "UFFICIO", address("10.0.0.5:7420"), at(2))
            .unwrap();
        let host = KnownHosts::load(path).0.hosts()[0].clone();
        assert!(host.favorite);
        assert_eq!(host.mac, mac("aa:bb:cc:00:11:22"));
    }

    #[test]
    fn announcements_update_address_and_mac_only_when_they_differ() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mut hosts) = store_in(&dir);
        let office = fingerprint();
        hosts
            .record_connection(office, "UFFICIO", address("10.0.0.5:7420"), at(1))
            .unwrap();
        hosts
            .note_announced(
                &office,
                Some(address("10.0.0.8:7420")),
                mac("aa:bb:cc:00:11:22"),
            )
            .unwrap();
        let host = KnownHosts::load(path.clone()).0.hosts()[0].clone();
        assert_eq!(host.address, Some(address("10.0.0.8:7420")));
        assert_eq!(host.mac, mac("aa:bb:cc:00:11:22"));
        assert_eq!(host.last_connected, Some(at(1)), "not a connection");

        // An older host announces no MAC: the one known is kept.
        hosts
            .note_announced(&office, Some(address("10.0.0.8:7420")), None)
            .unwrap();
        let host = KnownHosts::load(path).0.hosts()[0].clone();
        assert_eq!(host.mac, mac("aa:bb:cc:00:11:22"));
    }

    #[test]
    fn announcements_of_unknown_hosts_write_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mut hosts) = store_in(&dir);
        hosts
            .note_announced(&fingerprint(), Some(address("10.0.0.8:7420")), None)
            .unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn favorites_are_set_and_unset() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mut hosts) = store_in(&dir);
        let office = fingerprint();
        hosts
            .record_connection(office, "UFFICIO", address("10.0.0.5:7420"), at(1))
            .unwrap();
        hosts.set_favorite(&office, true).unwrap();
        assert!(KnownHosts::load(path.clone()).0.hosts()[0].favorite);
        hosts.set_favorite(&office, false).unwrap();
        assert!(!KnownHosts::load(path).0.hosts()[0].favorite);
    }

    #[test]
    fn forget_removes() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mut hosts) = store_in(&dir);
        let office = fingerprint();
        hosts
            .record_connection(office, "UFFICIO", address("10.0.0.5:7420"), at(1))
            .unwrap();
        hosts.forget(&office).unwrap();
        assert!(hosts.hosts().is_empty());
        assert!(KnownHosts::load(path).0.hosts().is_empty());
    }

    #[test]
    fn changes_from_another_process_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts.toml");
        let (office, home) = (fingerprint(), fingerprint());
        let (mut gui, _) = KnownHosts::load(path.clone());
        let (mut viewer, _) = KnownHosts::load(path.clone());
        viewer
            .record_connection(office, "UFFICIO", address("10.0.0.5:7420"), at(1))
            .unwrap();
        gui.record_connection(home, "CASA", address("10.0.0.6:7420"), at(2))
            .unwrap();
        let (reloaded, _) = KnownHosts::load(path);
        assert_eq!(reloaded.hosts().len(), 2, "{:?}", reloaded.hosts());
    }

    #[test]
    fn old_files_without_the_new_fields_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts.toml");
        let office = fingerprint();
        // Exactly what the previous version wrote.
        fs::write(
            &path,
            format!("[[host]]\nfingerprint = \"{office}\"\nname = \"UFFICIO\"\n"),
        )
        .unwrap();
        let (mut hosts, warning) = KnownHosts::load(path.clone());
        assert_eq!(warning, None);
        assert_eq!(hosts.hosts(), [KnownHost::new(office, "UFFICIO")]);

        // The first change upgrades the file and keeps the entry.
        hosts.set_favorite(&office, true).unwrap();
        let (reloaded, warning) = KnownHosts::load(path);
        assert_eq!(warning, None);
        assert_eq!(reloaded.hosts()[0].name, "UFFICIO");
        assert!(reloaded.hosts()[0].favorite);
    }

    #[test]
    fn the_new_fields_survive_a_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let (path, mut hosts) = store_in(&dir);
        let office = fingerprint();
        hosts
            .record_connection(
                office,
                "UFFICIO",
                address("[fd00::5]:7420"),
                at(1_700_000_000),
            )
            .unwrap();
        hosts
            .note_announced(&office, None, mac("AA-BB-CC-00-11-22"))
            .unwrap();
        hosts.set_favorite(&office, true).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("mac = \"aa:bb:cc:00:11:22\""), "{text}");
        let host = KnownHosts::load(path).0.hosts()[0].clone();
        assert_eq!(host.address, Some(address("[fd00::5]:7420")));
        assert_eq!(host.last_connected, Some(at(1_700_000_000)));
    }

    #[test]
    fn a_malformed_mac_does_not_discard_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts.toml");
        let office = fingerprint();
        fs::write(
            &path,
            format!(
                "[[host]]\nfingerprint = \"{office}\"\nname = \"UFFICIO\"\nmac = \"not a mac\"\n"
            ),
        )
        .unwrap();
        let (hosts, warning) = KnownHosts::load(path);
        assert_eq!(warning, None);
        assert_eq!(hosts.hosts().len(), 1);
        assert_eq!(hosts.hosts()[0].mac, None);
    }

    #[test]
    fn damaged_file_pins_nothing_and_is_replaced_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts.toml");
        fs::write(&path, "[[host]]\nfingerprint = 7\n").unwrap();
        let (mut hosts, warning) = KnownHosts::load(path.clone());
        assert!(hosts.hosts().is_empty());
        assert!(warning.is_some());
        let office = fingerprint();
        hosts
            .record_connection(office, "UFFICIO", address("10.0.0.5:7420"), at(1))
            .unwrap();
        let (reloaded, warning) = KnownHosts::load(path);
        assert_eq!(warning, None);
        assert_eq!(reloaded.hosts().len(), 1);
    }
}
