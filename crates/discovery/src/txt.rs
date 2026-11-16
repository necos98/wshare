//! [`HostAnnouncement`] and its DNS-SD TXT record codec.

use std::collections::HashMap;
use std::fmt;

use crate::MacAddress;

/// TXT key: stable host id.
const KEY_ID: &str = "id";
/// TXT key: display name (UTF-8).
const KEY_NAME: &str = "n";
/// TXT key: protocol version.
const KEY_VERSION: &str = "v";
/// TXT key: primary screen width.
const KEY_WIDTH: &str = "w";
/// TXT key: primary screen height.
const KEY_HEIGHT: &str = "h";
/// TXT key: hardware address of the announcing interface (optional, for Wake-on-LAN).
const KEY_MAC: &str = "m";

/// Maximum length of a host id (ASCII alphanumeric).
pub(crate) const MAX_ID_LEN: usize = 32;
/// Maximum display name length in bytes carried in TXT (one TXT string is <= 255 bytes).
const MAX_TXT_NAME_LEN: usize = 200;

/// What a host announces about itself on the LAN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostAnnouncement {
    /// Stable unique id of the host (e.g. hex prefix of its TLS certificate fingerprint).
    /// Must be 1..=32 ASCII alphanumeric characters. Distinguishes PCs with the same name.
    pub id: String,
    /// Human readable name, usually the computer name.
    pub display_name: String,
    /// QUIC (UDP) port the host listens on. Carried in the SRV record, not in TXT.
    pub port: u16,
    /// Wire protocol version. Hosts with a different version are still listed.
    pub protocol_version: u16,
    /// Primary screen width in pixels (informational).
    pub width: u32,
    /// Primary screen height in pixels (informational).
    pub height: u32,
    /// Hardware address of the interface the host announces on, so a viewer can wake it
    /// with Wake-on-LAN. `None` when unknown or announced by an older version.
    pub mac: Option<MacAddress>,
}

/// Why a TXT record could not be decoded into a [`HostAnnouncement`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxtError {
    /// A required key is absent.
    Missing(&'static str),
    /// A key is present but its value is malformed.
    Invalid {
        /// The offending key.
        key: &'static str,
        /// The value found.
        value: String,
    },
}

impl fmt::Display for TxtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(key) => write!(f, "TXT key `{key}` is missing"),
            Self::Invalid { key, value } => {
                write!(f, "TXT key `{key}` has invalid value {value:?}")
            }
        }
    }
}

impl std::error::Error for TxtError {}

/// Returns true if `id` is a valid host id (1..=32 ASCII alphanumeric characters).
pub(crate) fn is_valid_id(id: &str) -> bool {
    (1..=MAX_ID_LEN).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Truncates `s` to at most `max` bytes on a char boundary.
pub(crate) fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

impl HostAnnouncement {
    /// Checks the fields that must hold before announcing (currently: the id format).
    pub fn validate(&self) -> Result<(), TxtError> {
        if !is_valid_id(&self.id) {
            return Err(TxtError::Invalid {
                key: KEY_ID,
                value: self.id.clone(),
            });
        }
        Ok(())
    }

    /// Encodes everything but the port (which goes in SRV) as TXT properties.
    /// The display name is truncated to fit in a single TXT string.
    pub fn to_txt(&self) -> HashMap<String, String> {
        let mut txt = HashMap::from([
            (KEY_ID.to_owned(), self.id.clone()),
            (
                KEY_NAME.to_owned(),
                truncate_utf8(&self.display_name, MAX_TXT_NAME_LEN).to_owned(),
            ),
            (KEY_VERSION.to_owned(), self.protocol_version.to_string()),
            (KEY_WIDTH.to_owned(), self.width.to_string()),
            (KEY_HEIGHT.to_owned(), self.height.to_string()),
        ]);
        if let Some(mac) = self.mac {
            txt.insert(KEY_MAC.to_owned(), mac.to_string());
        }
        txt
    }

    /// Decodes TXT properties plus the SRV `port`. Unknown keys are ignored (forward
    /// compatible); a different `protocol_version` is not an error. The MAC is optional: when
    /// absent (older hosts) or malformed the host is still usable, only not wakeable.
    pub fn from_txt(port: u16, txt: &HashMap<String, String>) -> Result<Self, TxtError> {
        let get = |key: &'static str| {
            txt.get(key)
                .map(String::as_str)
                .ok_or(TxtError::Missing(key))
        };
        let id = get(KEY_ID)?;
        if !is_valid_id(id) {
            return Err(TxtError::Invalid {
                key: KEY_ID,
                value: id.to_owned(),
            });
        }
        Ok(Self {
            id: id.to_owned(),
            display_name: get(KEY_NAME)?.to_owned(),
            port,
            protocol_version: parse(KEY_VERSION, get(KEY_VERSION)?)?,
            width: parse(KEY_WIDTH, get(KEY_WIDTH)?)?,
            height: parse(KEY_HEIGHT, get(KEY_HEIGHT)?)?,
            mac: txt.get(KEY_MAC).and_then(|mac| mac.parse().ok()),
        })
    }
}

fn parse<T: std::str::FromStr>(key: &'static str, value: &str) -> Result<T, TxtError> {
    value.parse().map_err(|_| TxtError::Invalid {
        key,
        value: value.to_owned(),
    })
}

#[cfg(test)]
pub(crate) fn sample(id: &str, name: &str) -> HostAnnouncement {
    HostAnnouncement {
        id: id.to_owned(),
        display_name: name.to_owned(),
        port: 4433,
        protocol_version: 1,
        width: 2560,
        height: 1440,
        mac: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mac_travels_in_the_txt_record() {
        let mut a = sample("a1b2c3d4", "Desk");
        a.mac = "aa:bb:cc:00:11:22".parse().ok();
        let txt = a.to_txt();
        assert_eq!(
            txt.get(KEY_MAC).map(String::as_str),
            Some("aa:bb:cc:00:11:22")
        );
        assert_eq!(HostAnnouncement::from_txt(4433, &txt), Ok(a));
    }

    #[test]
    fn a_record_without_mac_is_still_valid_and_carries_none() {
        let a = sample("abc", "x");
        let txt = a.to_txt();
        assert!(
            !txt.contains_key(KEY_MAC),
            "nothing announced without a MAC"
        );
        // A record as written by a version that has no such key.
        let decoded = HostAnnouncement::from_txt(4433, &txt).unwrap();
        assert_eq!(decoded.mac, None);
    }

    #[test]
    fn a_malformed_mac_is_ignored_not_fatal() {
        for bad in ["", "nope", "00:00:00:00:00:00", "aa:bb:cc:00:11:22:33"] {
            let mut txt = sample("abc", "x").to_txt();
            txt.insert(KEY_MAC.into(), bad.into());
            let decoded = HostAnnouncement::from_txt(4433, &txt).expect(bad);
            assert_eq!(decoded.mac, None, "{bad:?}");
        }
    }

    #[test]
    fn roundtrip() {
        let a = sample("a1b2c3d4", "Büro-PC");
        assert_eq!(HostAnnouncement::from_txt(4433, &a.to_txt()), Ok(a));
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let a = sample("abc", "x");
        let mut txt = a.to_txt();
        txt.insert("future".into(), "1".into());
        assert_eq!(HostAnnouncement::from_txt(4433, &txt), Ok(a));
    }

    #[test]
    fn missing_keys() {
        for key in [KEY_ID, KEY_NAME, KEY_VERSION, KEY_WIDTH, KEY_HEIGHT] {
            let mut txt = sample("abc", "x").to_txt();
            txt.remove(key);
            assert_eq!(
                HostAnnouncement::from_txt(1, &txt),
                Err(TxtError::Missing(key))
            );
        }
    }

    #[test]
    fn invalid_values() {
        let cases = [
            (KEY_ID, ""),
            (KEY_ID, "has-dash"),
            (KEY_ID, "0123456789abcdef0123456789abcdef0"),
            (KEY_VERSION, "70000"),
            (KEY_VERSION, "one"),
            (KEY_WIDTH, "-1"),
            (KEY_HEIGHT, ""),
        ];
        for (key, value) in cases {
            let mut txt = sample("abc", "x").to_txt();
            txt.insert(key.into(), value.into());
            assert_eq!(
                HostAnnouncement::from_txt(1, &txt),
                Err(TxtError::Invalid {
                    key,
                    value: value.into()
                }),
                "{key}={value}"
            );
        }
    }

    #[test]
    fn long_display_name_is_truncated_on_char_boundary() {
        let a = sample("abc", &"é".repeat(150)); // 300 bytes
        let name = a.to_txt().remove(KEY_NAME).unwrap();
        assert_eq!(name.len(), MAX_TXT_NAME_LEN);
        assert!(name.chars().all(|c| c == 'é'));
    }

    #[test]
    fn validate_checks_id() {
        assert!(sample("abc", "x").validate().is_ok());
        assert!(sample("a.b", "x").validate().is_err());
    }
}
