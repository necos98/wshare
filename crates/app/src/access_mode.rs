//! How an unknown viewer may be let in: with the PIN, with a click of
//! the person at the host, or either. Authorized devices need neither.

use serde::{Deserialize, Serialize};

/// The «Accesso» setting. `PinOnly` is the default and the only mode of
/// `wshare host`, which has nobody to ask.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    #[default]
    PinOnly,
    /// Every unknown viewer is asked about; a PIN is not looked at.
    Confirmation,
    /// A viewer that sends the PIN is checked as usual; one that sends
    /// none is asked about.
    PinOrConfirmation,
}

impl AccessMode {
    pub const ALL: [Self; 3] = [Self::PinOnly, Self::Confirmation, Self::PinOrConfirmation];

    /// Whether a PIN can let an unknown viewer in.
    pub fn takes_pin(self) -> bool {
        !matches!(self, Self::Confirmation)
    }

    /// Whether an unknown viewer can be let in by a click at the host.
    pub fn asks_confirmation(self) -> bool {
        !matches!(self, Self::PinOnly)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_the_pin_alone() {
        assert_eq!(AccessMode::default(), AccessMode::PinOnly);
    }

    #[test]
    fn each_mode_says_what_lets_a_viewer_in() {
        let ways = |mode: AccessMode| (mode.takes_pin(), mode.asks_confirmation());
        assert_eq!(ways(AccessMode::PinOnly), (true, false));
        assert_eq!(ways(AccessMode::Confirmation), (false, true));
        assert_eq!(ways(AccessMode::PinOrConfirmation), (true, true));
    }

    #[test]
    fn modes_are_written_in_snake_case() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Holder {
            access: AccessMode,
        }
        for (mode, text) in [
            (AccessMode::PinOnly, "pin_only"),
            (AccessMode::Confirmation, "confirmation"),
            (AccessMode::PinOrConfirmation, "pin_or_confirmation"),
        ] {
            let written = toml::to_string(&Holder { access: mode }).unwrap();
            assert_eq!(written.trim(), format!("access = \"{text}\""));
            assert_eq!(toml::from_str::<Holder>(&written).unwrap().access, mode);
        }
    }
}
