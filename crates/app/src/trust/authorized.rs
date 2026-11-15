//! Host side: the viewers admitted once with the PIN, who connect without
//! it from then on.
//!
//! The list is loaded once per sharing service and kept in memory, so a
//! removal takes effect for the very next connection; every change is
//! written back to the file at once.

use std::path::PathBuf;

use anyhow::Result;
use net::Fingerprint;
use serde::{Deserialize, Serialize};

use super::store;
use super::timestamp::Timestamp;

const HEADER: &str = "wshare: computers allowed to view this screen without a PIN.\n\
                      Delete an entry (or use \"Rimuovi\" in wshare) to ask for the PIN again.";

/// A viewer this host recognises by its certificate fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizedDevice {
    #[serde(with = "super::fingerprint_text")]
    pub fingerprint: Fingerprint,
    /// The name the viewer gave at its last access (a label only).
    pub name: String,
    pub first_access: Timestamp,
    pub last_access: Timestamp,
}

/// Layout of the file.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Contents {
    #[serde(default, rename = "viewer")]
    viewers: Vec<AuthorizedDevice>,
}

#[derive(Debug)]
pub struct AuthorizedDevices {
    path: PathBuf,
    devices: Vec<AuthorizedDevice>,
}

impl AuthorizedDevices {
    /// Loads the list from `path`. A damaged file counts as empty (every
    /// viewer is asked for the PIN again); the second value says why.
    pub fn load(path: PathBuf) -> (Self, Option<String>) {
        let loaded = store::load::<Contents>(&path);
        let devices = loaded.value.viewers;
        (Self { path, devices }, loaded.warning)
    }

    /// Oldest first.
    pub fn list(&self) -> &[AuthorizedDevice] {
        &self.devices
    }

    pub fn contains(&self, fingerprint: &Fingerprint) -> bool {
        self.devices.iter().any(|d| &d.fingerprint == fingerprint)
    }

    /// Records an admission of the viewer with `fingerprint`: adds it, or
    /// refreshes its name and last access. The change holds in memory even
    /// if saving fails (the error says so).
    pub fn record_access(
        &mut self,
        fingerprint: Fingerprint,
        name: &str,
        now: Timestamp,
    ) -> Result<()> {
        match self
            .devices
            .iter_mut()
            .find(|d| d.fingerprint == fingerprint)
        {
            Some(device) => {
                device.name = name.to_string();
                device.last_access = now;
            }
            None => self.devices.push(AuthorizedDevice {
                fingerprint,
                name: name.to_string(),
                first_access: now,
                last_access: now,
            }),
        }
        self.save()
    }

    /// Forgets the viewer: its next connection needs the PIN. Returns
    /// whether it was in the list.
    pub fn remove(&mut self, fingerprint: &Fingerprint) -> Result<bool> {
        let before = self.devices.len();
        self.devices.retain(|d| &d.fingerprint != fingerprint);
        if self.devices.len() == before {
            return Ok(false);
        }
        self.save().map(|()| true)
    }

    fn save(&self) -> Result<()> {
        let contents = Contents {
            viewers: self.devices.clone(),
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
        Identity::generate("viewer").unwrap().fingerprint()
    }

    const T1: Timestamp = Timestamp::from_unix_seconds(1_700_000_000);
    const T2: Timestamp = Timestamp::from_unix_seconds(1_700_086_400);

    #[test]
    fn records_survive_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authorized.toml");
        let (laptop, desk) = (fingerprint(), fingerprint());
        let (mut devices, warning) = AuthorizedDevices::load(path.clone());
        assert_eq!(warning, None);
        assert!(devices.list().is_empty());
        devices.record_access(laptop, "PORTATILE", T1).unwrap();
        devices.record_access(desk, "UFFICIO", T1).unwrap();
        devices.record_access(laptop, "PORTATILE-2", T2).unwrap();

        let (reloaded, warning) = AuthorizedDevices::load(path);
        assert_eq!(warning, None);
        assert_eq!(
            reloaded.list(),
            [
                AuthorizedDevice {
                    fingerprint: laptop,
                    name: "PORTATILE-2".into(),
                    first_access: T1,
                    last_access: T2,
                },
                AuthorizedDevice {
                    fingerprint: desk,
                    name: "UFFICIO".into(),
                    first_access: T1,
                    last_access: T1,
                },
            ]
        );
        assert!(reloaded.contains(&laptop));
        assert!(!reloaded.contains(&fingerprint()));
    }

    #[test]
    fn file_is_readable_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authorized.toml");
        let laptop = fingerprint();
        let (mut devices, _) = AuthorizedDevices::load(path.clone());
        devices.record_access(laptop, "PORTATILE", T1).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("[[viewer]]"), "{text}");
        assert!(text.contains(&laptop.to_string()), "{text}");
        assert!(text.contains("2023-11-14T22:13:20Z"), "{text}");
        assert!(text.starts_with("# wshare"), "{text}");
    }

    #[test]
    fn removal_is_immediate_and_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authorized.toml");
        let laptop = fingerprint();
        let (mut devices, _) = AuthorizedDevices::load(path.clone());
        devices.record_access(laptop, "PORTATILE", T1).unwrap();
        assert!(devices.remove(&laptop).unwrap());
        assert!(!devices.contains(&laptop));
        assert!(!devices.remove(&laptop).unwrap(), "already gone");
        assert!(!AuthorizedDevices::load(path).0.contains(&laptop));
    }

    #[test]
    fn damaged_file_trusts_nobody() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authorized.toml");
        let laptop = fingerprint();
        let (mut devices, _) = AuthorizedDevices::load(path.clone());
        devices.record_access(laptop, "PORTATILE", T1).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        // A truncated fingerprint makes the whole file invalid.
        fs::write(&path, text.replace(&laptop.to_string(), "abcd")).unwrap();
        let (devices, warning) = AuthorizedDevices::load(path);
        assert!(devices.list().is_empty());
        assert!(warning.unwrap().contains("authorized.toml"));
    }

    #[test]
    fn a_failed_save_keeps_the_change_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("authorized.toml");
        fs::create_dir(&path).unwrap(); // a directory where the file belongs
        let (mut devices, _) = AuthorizedDevices::load(path);
        let laptop = fingerprint();
        assert!(devices.record_access(laptop, "PORTATILE", T1).is_err());
        assert!(devices.contains(&laptop));
    }
}
