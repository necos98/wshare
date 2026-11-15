//! Viewer side of the interface state: which listed computers this PC can
//! connect to without a PIN. Pure.

use std::collections::HashSet;

use crate::trust::KnownHost;

#[derive(Debug, Clone, Default)]
pub struct RememberedHosts {
    known: Vec<KnownHost>,
    /// Ids of known hosts that asked for the PIN again (e.g. this PC was
    /// removed from their list) and have not admitted it since.
    pin_again: HashSet<String>,
    /// Why the remembered hosts could not be read.
    warning: Option<String>,
}

impl RememberedHosts {
    /// Replaces the list with what the store holds now.
    pub fn set(&mut self, known: Vec<KnownHost>, warning: Option<String>) {
        self.known = known;
        self.warning = warning;
    }

    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    /// The known host announcing itself as `id`.
    pub fn known(&self, id: &str) -> Option<&KnownHost> {
        self.known.iter().find(|host| host.matches_id(id))
    }

    /// Whether connecting to `id` should need no PIN.
    pub fn is_remembered(&self, id: &str) -> bool {
        self.known(id).is_some() && !self.pin_again.contains(id)
    }

    /// The host refused to connect without a PIN.
    pub fn pin_required(&mut self, id: &str) {
        self.pin_again.insert(id.to_string());
    }

    /// The host admitted this PC (with the PIN or without).
    pub fn connected(&mut self, id: &str) {
        self.pin_again.remove(id);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use net::Fingerprint;

    use super::*;
    use crate::trust::host_id;

    pub(crate) fn known(byte: u8, name: &str) -> KnownHost {
        KnownHost {
            fingerprint: Fingerprint([byte; 32]),
            name: name.into(),
        }
    }

    #[test]
    fn known_hosts_are_remembered_until_they_ask_for_the_pin() {
        let office = known(0xab, "UFFICIO");
        let id = host_id(&office.fingerprint);
        let mut hosts = RememberedHosts::default();
        assert!(!hosts.is_remembered(&id));
        hosts.set(vec![office.clone()], None);
        assert!(hosts.is_remembered(&id));
        assert_eq!(hosts.known(&id), Some(&office));

        hosts.pin_required(&id);
        assert!(!hosts.is_remembered(&id), "the PIN field comes back");
        assert!(hosts.known(&id).is_some(), "still known");
        hosts.connected(&id);
        assert!(hosts.is_remembered(&id));
    }

    #[test]
    fn unknown_ids_and_warnings() {
        let mut hosts = RememberedHosts::default();
        hosts.set(vec![known(1, "A")], Some("damaged".into()));
        assert!(!hosts.is_remembered("ffffffffffffffff"));
        assert!(!hosts.is_remembered(""));
        assert_eq!(hosts.warning(), Some("damaged"));
        hosts.set(vec![], None);
        assert_eq!(hosts.warning(), None);
    }
}
