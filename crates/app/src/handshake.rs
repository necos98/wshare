//! Session admission policy and codec mapping. Pure logic, unit-tested.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

use protocol::control::{ControlMessage, PROTOCOL_VERSION};

/// Longest viewer name we keep (it is shown in logs/UI only).
const MAX_VIEWER_NAME: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelloVerdict {
    Accept { viewer_name: String },
    Reject { reason: String },
}

/// Decides whether the first control message of a session admits the viewer.
pub fn check_hello(message: &ControlMessage, expected_pin: &str) -> HelloVerdict {
    let ControlMessage::Hello { version, viewer_name, pin } = message else {
        return reject("expected Hello");
    };
    if *version != PROTOCOL_VERSION {
        return reject(&format!(
            "protocol version {version} not supported (host uses {PROTOCOL_VERSION})"
        ));
    }
    if !pins_match(pin, expected_pin) {
        return reject("wrong PIN");
    }
    let viewer_name = viewer_name.chars().take(MAX_VIEWER_NAME).collect();
    HelloVerdict::Accept { viewer_name }
}

fn reject(reason: &str) -> HelloVerdict {
    HelloVerdict::Reject { reason: reason.to_string() }
}

/// Compares without an early exit, so response time doesn't leak how many
/// leading digits were right.
fn pins_match(given: &str, expected: &str) -> bool {
    let (a, b) = (given.trim().as_bytes(), expected.as_bytes());
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

    fn hello(version: u16, pin: &str) -> ControlMessage {
        ControlMessage::Hello { version, viewer_name: "laptop".into(), pin: pin.into() }
    }

    #[test]
    fn accepts_right_pin_and_version() {
        assert_eq!(
            check_hello(&hello(PROTOCOL_VERSION, "123456"), "123456"),
            HelloVerdict::Accept { viewer_name: "laptop".into() }
        );
    }

    #[test]
    fn pin_whitespace_from_typing_is_ignored() {
        assert!(matches!(
            check_hello(&hello(PROTOCOL_VERSION, " 123456\n"), "123456"),
            HelloVerdict::Accept { .. }
        ));
    }

    #[test]
    fn rejects_wrong_pin_wrong_version_and_other_messages() {
        for (message, reason) in [
            (hello(PROTOCOL_VERSION, "000000"), "wrong PIN"),
            (hello(PROTOCOL_VERSION, "12345"), "wrong PIN"),
            (hello(PROTOCOL_VERSION + 1, "123456"), "protocol version"),
            (ControlMessage::RequestKeyframe, "expected Hello"),
        ] {
            match check_hello(&message, "123456") {
                HelloVerdict::Reject { reason: r } => assert!(r.contains(reason), "{r}"),
                other => panic!("{message:?} accepted: {other:?}"),
            }
        }
    }

    #[test]
    fn long_viewer_names_are_truncated() {
        let message = ControlMessage::Hello {
            version: PROTOCOL_VERSION,
            viewer_name: "x".repeat(1000),
            pin: "1".into(),
        };
        let HelloVerdict::Accept { viewer_name } = check_hello(&message, "1") else { panic!() };
        assert_eq!(viewer_name.len(), MAX_VIEWER_NAME);
    }

    #[test]
    fn generated_pins_are_six_digits_and_vary() {
        let pins: Vec<String> = (0..20).map(|_| generate_pin()).collect();
        assert!(pins.iter().all(|p| p.len() == 6 && p.bytes().all(|b| b.is_ascii_digit())));
        assert!(pins.iter().any(|p| p != &pins[0]));
    }

}
