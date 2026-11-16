//! [`KnownHostsStore`] backed by the viewer's known-hosts file (see
//! `trust::KnownHosts`), which the launched viewers write.

use std::net::SocketAddr;

use anyhow::Result;
use discovery::MacAddress;
use net::Fingerprint;

use crate::paths;
use crate::trust::{KnownHost, KnownHosts};

use super::ports::KnownHostsStore;

pub struct KnownHostsFile;

impl KnownHostsFile {
    /// The file as it is now. A load warning changes nothing for a change:
    /// every change re-reads the file itself.
    fn open() -> Result<KnownHosts> {
        Ok(KnownHosts::load(paths::known_hosts_file()?).0)
    }
}

impl KnownHostsStore for KnownHostsFile {
    fn load(&self) -> (Vec<KnownHost>, Option<String>) {
        match paths::known_hosts_file() {
            Ok(path) => {
                let (hosts, warning) = KnownHosts::load(path);
                (hosts.hosts().to_vec(), warning)
            }
            Err(e) => (Vec::new(), Some(format!("{e:#}"))),
        }
    }

    fn forget(&self, fingerprint: &Fingerprint) -> Result<()> {
        Self::open()?.forget(fingerprint)
    }

    fn set_favorite(&self, fingerprint: &Fingerprint, favorite: bool) -> Result<()> {
        Self::open()?.set_favorite(fingerprint, favorite)
    }

    fn note_announced(
        &self,
        fingerprint: &Fingerprint,
        address: Option<SocketAddr>,
        mac: Option<MacAddress>,
    ) -> Result<()> {
        Self::open()?.note_announced(fingerprint, address, mac)
    }
}
