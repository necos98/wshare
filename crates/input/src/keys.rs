//! Validation of untrusted keyboard scan codes.

/// A validated PS/2 set-1 make code, ready for `KEYEVENTF_SCANCODE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Scancode {
    code: u8,
    extended: bool,
}

impl Scancode {
    /// Validates a scan code received from the network.
    ///
    /// Accepted: `0x01..=0xFF`, minus the bytes that are not keys in set 1
    /// (`0xE0`/`0xE1` prefixes, `0xFF` overrun). Codes >= 0x80 are kept on
    /// purpose: some keys (e.g. Korean Hangul/Hanja, 0xF1/0xF2) only exist
    /// there. Documented exception: `0xE0nn` (prefix folded into the code, as
    /// some capture APIs report it) is accepted as `nn` + extended.
    /// Everything else, including the `0xE1` Pause sequence, is rejected:
    /// it cannot be expressed as one scancode `SendInput` event.
    pub fn new(scancode: u16, extended: bool) -> Option<Self> {
        let [high, code] = scancode.to_be_bytes();
        let extended = match high {
            0x00 => extended,
            0xE0 => true,
            _ => return None,
        };
        match code {
            0x00 | 0xE0 | 0xE1 | 0xFF => None,
            _ => Some(Self { code, extended }),
        }
    }

    pub const fn code(self) -> u8 {
        self.code
    }

    /// The key needs the 0xE0 prefix (`KEYEVENTF_EXTENDEDKEY`).
    pub const fn extended(self) -> bool {
        self.extended
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_and_extended_codes() {
        let a = Scancode::new(0x1E, false).unwrap();
        assert_eq!((a.code(), a.extended()), (0x1E, false));
        let right_ctrl = Scancode::new(0x1D, true).unwrap();
        assert_eq!((right_ctrl.code(), right_ctrl.extended()), (0x1D, true));
        assert!(Scancode::new(0x01, false).is_some());
        assert!(Scancode::new(0xF2, false).is_some());
    }

    #[test]
    fn folds_e0_prefix_into_extended() {
        let up = Scancode::new(0xE048, false).unwrap();
        assert_eq!(up, Scancode::new(0x48, true).unwrap());
    }

    #[test]
    fn rejects_invalid_codes() {
        for bad in [
            0x0000, 0x00E0, 0x00E1, 0x00FF, 0x0100, 0xE11D, 0xE000, 0xFFFF, 0x1E00,
        ] {
            assert!(Scancode::new(bad, false).is_none(), "{bad:#x}");
            assert!(Scancode::new(bad, true).is_none(), "{bad:#x} ext");
        }
    }
}
