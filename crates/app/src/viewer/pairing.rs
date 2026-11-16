//! The viewer's half of "enter the PIN only once": the certificate it
//! presents to hosts, and the host it is about to connect to if that host
//! admitted it before (pinned by fingerprint, connected to without a PIN).
//!
//! None of this is required to connect: a missing or damaged file only
//! means the PIN is asked, so problems are printed as warnings.

use discovery::computer_name;
use net::{Fingerprint, Identity};

use crate::paths;
use crate::trust::{KnownHost, KnownHosts, clean_peer_name};

pub(super) struct Pairing {
    /// `None`: no certificate could be loaded; hosts then always ask for
    /// the PIN, so nothing is remembered either.
    identity: Option<Identity>,
    known_hosts: Option<KnownHosts>,
    /// The remembered host the target id names.
    expected: Option<KnownHost>,
}

impl Pairing {
    /// Loads this PC's viewer certificate (creating it the first time) and
    /// looks up the host announced as `host_id` among the known hosts.
    pub(super) fn load(host_id: Option<&str>) -> Self {
        let identity = paths::viewer_identity_dir()
            .and_then(|dir| Identity::load_or_create(&dir, &computer_name()));
        let identity = identity
            .inspect_err(|e| {
                warn(&format!(
                    "cannot load this PC's certificate, hosts will ask for the PIN: {e:#}"
                ))
            })
            .ok();
        let known_hosts = match paths::known_hosts_file() {
            Ok(path) => {
                let (hosts, warning) = KnownHosts::load(path);
                if let Some(warning) = warning {
                    warn(&format!("remembered hosts unavailable: {warning}"));
                }
                Some(hosts)
            }
            Err(e) => {
                warn(&format!("remembered hosts unavailable: {e:#}"));
                None
            }
        };
        let expected = host_id
            .zip(known_hosts.as_ref())
            .and_then(|(id, hosts)| hosts.find_by_id(id).cloned());
        Self {
            identity,
            known_hosts,
            expected,
        }
    }

    pub(super) fn identity(&self) -> Option<&Identity> {
        self.identity.as_ref()
    }

    /// This PC's viewer certificate, to present again when the session
    /// has to be re-established.
    pub(super) fn into_identity(self) -> Option<Identity> {
        self.identity
    }

    /// The fingerprint to pin, for a host that admitted us before.
    pub(super) fn expected(&self) -> Option<Fingerprint> {
        self.expected.as_ref().map(|host| host.fingerprint)
    }

    /// Remembers the host that just welcomed us, so the next connection
    /// pins it and needs no PIN. Only with a certificate: without one the
    /// host cannot recognise us anyway.
    pub(super) fn remember(&mut self, fingerprint: Fingerprint, host_name: &str) {
        if self.identity.is_none() {
            return;
        }
        let Some(known_hosts) = &mut self.known_hosts else {
            return;
        };
        if let Err(e) = known_hosts.remember(fingerprint, &clean_peer_name(host_name)) {
            warn(&format!(
                "cannot remember this host, the PIN will be asked next time: {e:#}"
            ));
        }
    }
}

fn warn(message: &str) {
    eprintln!("warning: {message}");
}
