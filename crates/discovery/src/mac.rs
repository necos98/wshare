//! [`MacAddress`]: the hardware address of a network interface, announced by
//! hosts so a viewer can wake them with Wake-on-LAN.

use std::fmt;
use std::str::FromStr;

/// Bytes in a MAC address.
pub const MAC_LEN: usize = 6;
/// Text form: six hex pairs separated by a colon (or a dash on input).
const TEXT_LEN: usize = MAC_LEN * 3 - 1;

/// An Ethernet/Wi-Fi hardware address. Never all zeros or all ones, which
/// no network card has and a magic packet cannot address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacAddress([u8; MAC_LEN]);

/// Why a text is not a [`MacAddress`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacParseError(String);

impl fmt::Display for MacParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} is not a MAC address like aa:bb:cc:dd:ee:ff",
            self.0
        )
    }
}

impl std::error::Error for MacParseError {}

impl MacAddress {
    /// `None` for the all-zero and the broadcast address.
    pub fn new(bytes: [u8; MAC_LEN]) -> Option<Self> {
        let unusable = bytes.iter().all(|&b| b == 0) || bytes.iter().all(|&b| b == 0xff);
        (!unusable).then_some(Self(bytes))
    }

    pub fn octets(self) -> [u8; MAC_LEN] {
        self.0
    }
}

impl fmt::Display for MacAddress {
    /// Lowercase hex pairs separated by colons: `aa:bb:cc:00:11:22`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{g:02x}")
    }
}

impl FromStr for MacAddress {
    type Err = MacParseError;

    /// Accepts the form [`fmt::Display`] writes, with `-` also allowed as
    /// separator and either letter case.
    fn from_str(text: &str) -> Result<Self, MacParseError> {
        let error = || MacParseError(text.to_owned());
        if text.len() != TEXT_LEN || !text.is_ascii() {
            return Err(error());
        }
        let mut bytes = [0u8; MAC_LEN];
        let mut pairs = text.split([':', '-']);
        for byte in &mut bytes {
            let pair = pairs.next().ok_or_else(error)?;
            // `from_str_radix` alone would also take a sign.
            if pair.len() != 2 || !pair.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(error());
            }
            *byte = u8::from_str_radix(pair, 16).map_err(|_| error())?;
        }
        if pairs.next().is_some() {
            return Err(error());
        }
        Self::new(bytes).ok_or_else(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: [u8; MAC_LEN] = [0xaa, 0xbb, 0xcc, 0x00, 0x11, 0x22];

    #[test]
    fn text_roundtrip_in_both_separators_and_cases() {
        let mac = MacAddress::new(SAMPLE).unwrap();
        assert_eq!(mac.to_string(), "aa:bb:cc:00:11:22");
        for text in [
            "aa:bb:cc:00:11:22",
            "AA-BB-CC-00-11-22",
            "aa:BB:cc:00:11:22",
        ] {
            assert_eq!(text.parse(), Ok(mac), "{text}");
        }
    }

    #[test]
    fn malformed_texts_are_refused() {
        for text in [
            "",
            "aa:bb:cc:00:11",
            "aa:bb:cc:00:11:22:33",
            "aa:bb:cc:00:11:2",
            "aabbcc001122",
            "aa:bb:cc:00:11:zz",
            "aa:bb:cc:00:11:+2",
            "aa.bb.cc.00.11.22",
            "aa:bb:cc:00:11:é",
            "00:00:00:00:00:00",
            "ff:ff:ff:ff:ff:ff",
        ] {
            assert!(text.parse::<MacAddress>().is_err(), "{text:?}");
        }
    }

    #[test]
    fn unusable_addresses_are_refused() {
        assert_eq!(MacAddress::new([0; MAC_LEN]), None);
        assert_eq!(MacAddress::new([0xff; MAC_LEN]), None);
    }
}
