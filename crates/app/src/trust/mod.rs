//! What each side remembers so the PIN is needed only once per pair of PCs.
//!
//! - [`AuthorizedDevices`] (host): viewers admitted once with the PIN,
//!   recognised afterwards by the fingerprint of their TLS client
//!   certificate. Never by anything the viewer *says* in `Hello`.
//! - [`KnownHosts`] (viewer): hosts that admitted this viewer, pinned by
//!   their full certificate fingerprint on the next connection.
//!
//! Both are TOML files in the per-user configuration folder (see
//! `crate::paths`), handled by `store`. The PIN itself is never stored.

mod authorized;
mod known_hosts;
mod store;
mod timestamp;

pub use authorized::{AuthorizedDevice, AuthorizedDevices};
pub use known_hosts::{KnownHost, KnownHosts};
pub use timestamp::Timestamp;



/// Longest name we keep for a peer (it is shown in logs and the UI only).
const MAX_PEER_NAME: usize = 64;



/// A name a peer gave itself, made safe to keep and show: control
/// characters removed, at most [`MAX_PEER_NAME`] characters. It remains a
/// label: trust never depends on it.
pub fn clean_peer_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(MAX_PEER_NAME)
        .collect()
}

/// `#[serde(with = ...)]` for a [`Fingerprint`] as its 64 hex characters.
mod fingerprint_text {
    use net::Fingerprint;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(fingerprint: &Fingerprint, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(fingerprint)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Fingerprint, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn peer_names_are_cleaned_and_bounded() {
        assert_eq!(clean_peer_name("PORTATILE\n\u{1b}[31m"), "PORTATILE[31m");
        assert_eq!(
            clean_peer_name(&"\u{e8}".repeat(100)).chars().count(),
            MAX_PEER_NAME
        );
        assert_eq!(
            clean_peer_name("Jacob's PC \u{2014} salotto"),
            "Jacob's PC \u{2014} salotto"
        );
    }
}
