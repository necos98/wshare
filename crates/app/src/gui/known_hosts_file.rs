//! [`KnownHostsStore`] backed by the viewer's known-hosts file (see
//! `trust::KnownHosts`), which the launched viewers write.

use anyhow::Result;
use net::Fingerprint;

use crate::paths;
use crate::trust::{KnownHost, KnownHosts};

use super::ports::KnownHostsStore;

pub struct KnownHostsFile;

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
        // `forget` re-reads the file itself; a load warning changes nothing.
        KnownHosts::load(paths::known_hosts_file()?)
            .0
            .forget(fingerprint)
    }
}
