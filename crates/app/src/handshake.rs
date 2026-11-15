//! Session admission policy. Pure logic, unit-tested.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

use protocol::control::{ControlMessage, PROTOCOL_VERSION, RejectReason};

use crate::trust::clean_peer_name;

/// What the host knows about the viewer before reading its `Hello`, from
/// the TLS handshake alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerTrust {
    /// Its client certificate is in the list of authorized devices.
    Authorized,
    /// Unknown certificate, or none at all.
    Unknown,
}

/// How an admitted viewer proved it may connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmittedBy {
    /// Its certificate was authorized earlier; no PIN was needed.
    Device,
    Pin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelloVerdict {
    Accept { viewer_name: String, by: AdmittedBy },
    Reject { reason: RejectReason },
}

/// Decides whether the first control message of a session admits the
/// viewer: the version must match; then an authorized device needs no PIN
/// (whatever PIN it sends), anyone else needs the PIN valid right now.
pub fn check_hello(
    message: &ControlMessage,
    expected_pin: &str,
    trust: ViewerTrust,
) -> HelloVerdict {
    let ControlMessage::Hello {
        version,
        viewer_name,
        pin,
    } = message
    else {
        return reject(RejectReason::Other("expected Hello".into()));
    };
    if *version != PROTOCOL_VERSION {
        return reject(unsupported_version());
    }
    let by = match (trust, pin.as_deref().map(str::trim)) {
        (ViewerTrust::Authorized, _) => AdmittedBy::Device,
        (ViewerTrust::Unknown, None | Some("")) => return reject(RejectReason::PinRequired),
        (ViewerTrust::Unknown, Some(pin)) if pins_match(pin, expected_pin) => AdmittedBy::Pin,
        (ViewerTrust::Unknown, Some(_)) => return reject(RejectReason::WrongPin),
    };
    HelloVerdict::Accept {
        viewer_name: clean_peer_name(viewer_name),
        by,
    }
}

/// The answer to a viewer of another protocol version, including one whose
/// `Hello` cannot even be decoded.
pub fn unsupported_version() -> RejectReason {
    RejectReason::UnsupportedVersion {
        host: PROTOCOL_VERSION,
    }
}

fn reject(reason: RejectReason) -> HelloVerdict {
    HelloVerdict::Reject { reason }
}

/// Compares without an early exit, so response time doesn't leak how many
/// leading digits were right.
fn pins_match(given: &str, expected: &str) -> bool {
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A random 6-digit PIN. `RandomState` is seeded from the OS random
/// generator, which is plenty for a short-lived pairing code.
pub fn generate_pin() -> String {
    let random = RandomState::new().build_hasher().finish();
    format!("{:06}", random % 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIN: &str = "123456";

    fn hello(version: u16, pin: Option<&str>) -> ControlMessage {
        ControlMessage::Hello {
            version,
            viewer_name: "laptop".into(),
            pin: pin.map(String::from),
        }
    }

    fn check(message: &ControlMessage, trust: ViewerTrust) -> HelloVerdict {
        check_hello(message, PIN, trust)
    }

    fn accepted(by: AdmittedBy) -> HelloVerdict {
        HelloVerdict::Accept {
            viewer_name: "laptop".into(),
            by,
        }
    }

    fn rejected(reason: RejectReason) -> HelloVerdict {
        HelloVerdict::Reject { reason }
    }

    #[test]
    fn unknown_viewer_with_the_right_pin_is_admitted_by_pin() {
        assert_eq!(
            check(&hello(PROTOCOL_VERSION, Some(PIN)), ViewerTrust::Unknown),
            accepted(AdmittedBy::Pin)
        );
    }

    #[test]
    fn pin_whitespace_from_typing_is_ignored() {
        assert_eq!(
            check(
                &hello(PROTOCOL_VERSION, Some(" 123456\n")),
                ViewerTrust::Unknown
            ),
            accepted(AdmittedBy::Pin)
        );
    }

    #[test]
    fn authorized_viewer_needs_no_pin() {
        for pin in [None, Some(""), Some("000000"), Some(PIN)] {
            assert_eq!(
                check(&hello(PROTOCOL_VERSION, pin), ViewerTrust::Authorized),
                accepted(AdmittedBy::Device),
                "{pin:?}"
            );
        }
    }

    #[test]
    fn unknown_viewer_without_a_pin_is_asked_for_one() {
        for pin in [None, Some(""), Some("  ")] {
            assert_eq!(
                check(&hello(PROTOCOL_VERSION, pin), ViewerTrust::Unknown),
                rejected(RejectReason::PinRequired),
                "{pin:?}"
            );
        }
    }

    #[test]
    fn unknown_viewer_with_a_wrong_pin_is_rejected() {
        for pin in ["000000", "12345", "1234567", "\u{e9}23456"] {
            assert_eq!(
                check(&hello(PROTOCOL_VERSION, Some(pin)), ViewerTrust::Unknown),
                rejected(RejectReason::WrongPin),
                "{pin:?}"
            );
        }
    }

    #[test]
    fn version_is_checked_even_for_authorized_viewers() {
        for trust in [ViewerTrust::Authorized, ViewerTrust::Unknown] {
            assert_eq!(
                check(&hello(PROTOCOL_VERSION + 1, Some(PIN)), trust),
                rejected(unsupported_version())
            );
        }
    }

    #[test]
    fn a_protocol_3_hello_that_decodes_is_refused_for_its_version() {
        // Protocol 3 sent the PIN as a plain string; an empty one decodes
        // as "no PIN" today. It must be refused for its version, not asked
        // for a PIN it could not understand.
        for trust in [ViewerTrust::Authorized, ViewerTrust::Unknown] {
            assert_eq!(
                check(&hello(3, None), trust),
                rejected(unsupported_version())
            );
        }
    }

    #[test]
    fn other_first_messages_are_rejected() {
        assert_eq!(
            check(&ControlMessage::RequestKeyframe, ViewerTrust::Authorized),
            rejected(RejectReason::Other("expected Hello".into()))
        );
    }

    #[test]
    fn viewer_names_are_cleaned() {
        let message = ControlMessage::Hello {
            version: PROTOCOL_VERSION,
            viewer_name: format!("{}\n", "x".repeat(1000)),
            pin: Some("1".into()),
        };
        let HelloVerdict::Accept { viewer_name, .. } =
            check_hello(&message, "1", ViewerTrust::Unknown)
        else {
            panic!()
        };
        assert_eq!(viewer_name, clean_peer_name(&"x".repeat(1000)));
    }

    #[test]
    fn generated_pins_are_six_digits_and_vary() {
        let pins: Vec<String> = (0..20).map(|_| generate_pin()).collect();
        assert!(
            pins.iter()
                .all(|p| p.len() == 6 && p.bytes().all(|b| b.is_ascii_digit()))
        );
        assert!(pins.iter().any(|p| p != &pins[0]));
    }
}
