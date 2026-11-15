//! Viewer side: the hosts that admitted this PC, pinned by their full
//! certificate fingerprint on later connections.
//!
//! Several processes use the file (the desktop interface and each viewer
//! it launches), so every change re-reads it first and only then writes:
//! one process does not undo what another just saved.

use std::path::PathBuf;

use anyhow::Result;
use net::Fingerprint;
use serde::{Deserialize, Serialize};

use super::store;

const HEADER: &str = "wshare: computers this PC connected to; they are pinned by fingerprint\n\
                      and connected to without a PIN. Delete an entry (or use \"Dimentica\").";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownHost {
    #[serde(with = "super::fingerprint_text")]
    pub fingerprint: Fingerprint,
    /// The name the host gave at the last connection (a label only).
    pub name: String,
}

impl KnownHost {
    /// Whether this is the host announcing itself as `id` on the LAN.
    pub fn matches_id(&self, id: &str) -> bool {
        Fingerprint::is_host_id(id) && self.fingerprint.host_id().eq_ignore_ascii_case(id)
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

    /// Remembers (or renames) the host with `fingerprint`.
    pub fn remember(&mut self, fingerprint: Fingerprint, name: &str) -> Result<()> {
        self.update(|hosts| {
            match hosts.iter_mut().find(|h| h.fingerprint == fingerprint) {
                Some(host) if host.name == name => return false,
                Some(host) => host.name = name.to_string(),
                None => hosts.push(KnownHost {
                    fingerprint,
                    name: name.to_string(),
                }),
            }
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

#[cfg(test)]
mod tests {
    use std::fs;

    use net::Identity;

    use super::*;

    fn fingerprint() -> Fingerprint {
        Identity::generate("host").unwrap().fingerprint()
    }

    #[test]
    fn remembered_hosts_are_found_by_their_lan_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts.toml");
        let office = fingerprint();
        let (mut hosts, warning) = KnownHosts::load(path.clone());
        assert_eq!(warning, None);
        hosts.remember(office, "UFFICIO").unwrap();

        let (reloaded, _) = KnownHosts::load(path);
        let id = office.host_id();
        let found = reloaded.find_by_id(&id).expect("known");
        assert_eq!(found.name, "UFFICIO");
        assert!(reloaded.find_by_id(&id.to_uppercase()).is_some());
        assert!(
            reloaded
                .find_by_id(&fingerprint().host_id())
                .is_none()
        );
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
    fn remember_renames_and_forget_removes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts.toml");
        let office = fingerprint();
        let (mut hosts, _) = KnownHosts::load(path.clone());
        hosts.remember(office, "UFFICIO").unwrap();
        hosts.remember(office, "UFFICIO-2").unwrap();
        assert_eq!(hosts.hosts().len(), 1);
        assert_eq!(hosts.hosts()[0].name, "UFFICIO-2");
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
        viewer.remember(office, "UFFICIO").unwrap();
        gui.remember(home, "CASA").unwrap();
        let (reloaded, _) = KnownHosts::load(path);
        assert_eq!(reloaded.hosts().len(), 2, "{:?}", reloaded.hosts());
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
        hosts.remember(office, "UFFICIO").unwrap();
        let (reloaded, warning) = KnownHosts::load(path);
        assert_eq!(warning, None);
        assert_eq!(reloaded.hosts().len(), 1);
    }
}
