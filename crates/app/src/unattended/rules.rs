//! Who unattended access lets in. Pure, unit-tested.
//!
//! Nobody is at the host to read a PIN or click «Accetta», so the PIN and
//! the access mode play no part: only a device authorized earlier (its TLS
//! certificate, see `trust`) is admitted, and, when a password is set, only
//! with that password. A device that is not authorized is refused without
//! learning whether a password is set, and its password is never checked.

use protocol::control::RejectReason;

use crate::handshake::ViewerTrust;

/// What became of the password a viewer sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordCheck {
    /// Unattended access has no password.
    NotSet,
    /// A password is set and the viewer sent none.
    Missing,
    Right,
    Wrong,
    /// Not checked, because the verdict does not depend on it (see
    /// [`needs_password_check`]): it is never a reason to admit.
    Skipped,
}

/// Whether the password a viewer sent must be checked: only for an
/// authorized device whose address is not paused. Checking is slow on
/// purpose, so nobody else can make the host spend that time.
pub fn needs_password_check(trust: ViewerTrust, paused: bool) -> bool {
    trust == ViewerTrust::Authorized && !paused
}

/// The verdict on a viewer of unattended access (`Ok`: admitted).
/// `paused`: too many wrong passwords came from its address lately.
pub fn verdict(
    trust: ViewerTrust,
    password: PasswordCheck,
    paused: bool,
) -> Result<(), RejectReason> {
    if trust == ViewerTrust::Unknown {
        return Err(RejectReason::NotAuthorized);
    }
    match password {
        PasswordCheck::NotSet => Ok(()),
        _ if paused => Err(RejectReason::TooManyAttempts),
        PasswordCheck::Right => Ok(()),
        PasswordCheck::Missing | PasswordCheck::Skipped => Err(RejectReason::PasswordRequired),
        PasswordCheck::Wrong => Err(RejectReason::WrongPassword),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERY_CHECK: [PasswordCheck; 5] = [
        PasswordCheck::NotSet,
        PasswordCheck::Missing,
        PasswordCheck::Right,
        PasswordCheck::Wrong,
        PasswordCheck::Skipped,
    ];

    #[test]
    fn a_device_that_is_not_authorized_is_never_admitted() {
        for password in EVERY_CHECK {
            for paused in [false, true] {
                assert_eq!(
                    verdict(ViewerTrust::Unknown, password, paused),
                    Err(RejectReason::NotAuthorized),
                    "{password:?}, paused {paused}"
                );
            }
        }
        assert!(!needs_password_check(ViewerTrust::Unknown, false));
    }

    #[test]
    fn without_a_password_an_authorized_device_is_admitted() {
        assert_eq!(
            verdict(ViewerTrust::Authorized, PasswordCheck::NotSet, false),
            Ok(())
        );
        assert_eq!(
            verdict(ViewerTrust::Authorized, PasswordCheck::NotSet, true),
            Ok(()),
            "nothing to guess, nothing to pause"
        );
    }

    #[test]
    fn with_a_password_only_the_right_one_admits() {
        let authorized = |password| verdict(ViewerTrust::Authorized, password, false);
        assert_eq!(authorized(PasswordCheck::Right), Ok(()));
        assert_eq!(
            authorized(PasswordCheck::Missing),
            Err(RejectReason::PasswordRequired)
        );
        assert_eq!(
            authorized(PasswordCheck::Wrong),
            Err(RejectReason::WrongPassword)
        );
        assert_eq!(
            authorized(PasswordCheck::Skipped),
            Err(RejectReason::PasswordRequired),
            "an unchecked password never admits"
        );
    }

    #[test]
    fn a_paused_address_is_refused_without_checking_its_password() {
        assert!(!needs_password_check(ViewerTrust::Authorized, true));
        for password in [
            PasswordCheck::Missing,
            PasswordCheck::Right,
            PasswordCheck::Wrong,
            PasswordCheck::Skipped,
        ] {
            assert_eq!(
                verdict(ViewerTrust::Authorized, password, true),
                Err(RejectReason::TooManyAttempts),
                "{password:?}"
            );
        }
    }

    #[test]
    fn only_an_authorized_device_that_is_not_paused_gets_its_password_checked() {
        assert!(needs_password_check(ViewerTrust::Authorized, false));
    }
}
