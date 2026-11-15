//! Why `wshare view` could not connect, also as an exit code, so the
//! desktop interface (which runs the viewer as a child process) reacts to
//! the cause itself rather than to the wording of an error message.

use std::fmt;

use net::IdentityMismatch;
use protocol::control::RejectReason;

/// Exit code of any other failure.
pub const GENERIC_EXIT_CODE: i32 = 1;

/// A connection failure the caller may act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectFailure {
    Rejected(RejectReason),
    /// The remembered host answered with another certificate.
    HostChanged(IdentityMismatch),
}

impl fmt::Display for ConnectFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(reason) => write!(f, "the host refused: {reason}"),
            Self::HostChanged(mismatch) => write!(
                f,
                "{mismatch}. Another computer may be answering at this address; if the \
                 host was reinstalled, forget it in wshare and connect again with the PIN"
            ),
        }
    }
}

impl std::error::Error for ConnectFailure {}

impl ConnectFailure {
    pub fn kind(&self) -> Option<FailureKind> {
        match self {
            Self::Rejected(RejectReason::PinRequired) => Some(FailureKind::PinRequired),
            Self::Rejected(RejectReason::WrongPin) => Some(FailureKind::WrongPin),
            Self::Rejected(RejectReason::UnsupportedVersion { .. }) => {
                Some(FailureKind::UnsupportedVersion)
            }
            Self::Rejected(RejectReason::Other(_)) => None,
            Self::HostChanged(_) => Some(FailureKind::HostChanged),
        }
    }
}

/// The failures that have their own exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    PinRequired,
    WrongPin,
    UnsupportedVersion,
    HostChanged,
}

impl FailureKind {
    const ALL: [Self; 4] = [
        Self::PinRequired,
        Self::WrongPin,
        Self::UnsupportedVersion,
        Self::HostChanged,
    ];

    /// Distinct from 0 (success), [`GENERIC_EXIT_CODE`] and 101 (panic).
    pub fn exit_code(self) -> i32 {
        match self {
            Self::PinRequired => 10,
            Self::WrongPin => 11,
            Self::UnsupportedVersion => 12,
            Self::HostChanged => 13,
        }
    }

    pub fn from_exit_code(code: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.exit_code() == code)
    }
}

/// The exit code `wshare view` ends with for `error`.
pub fn exit_code(error: &anyhow::Error) -> i32 {
    error
        .downcast_ref::<ConnectFailure>()
        .and_then(ConnectFailure::kind)
        .map_or(GENERIC_EXIT_CODE, FailureKind::exit_code)
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use net::Fingerprint;

    use super::*;

    #[test]
    fn exit_codes_roundtrip_and_are_distinct() {
        for kind in FailureKind::ALL {
            assert_eq!(FailureKind::from_exit_code(kind.exit_code()), Some(kind));
            assert_ne!(kind.exit_code(), GENERIC_EXIT_CODE);
        }
        assert_eq!(FailureKind::from_exit_code(0), None);
        assert_eq!(FailureKind::from_exit_code(GENERIC_EXIT_CODE), None);
        assert_eq!(FailureKind::from_exit_code(101), None);
    }

    #[test]
    fn errors_map_to_exit_codes() {
        let rejected = |reason| anyhow::Error::new(ConnectFailure::Rejected(reason));
        assert_eq!(exit_code(&rejected(RejectReason::PinRequired)), 10);
        assert_eq!(exit_code(&rejected(RejectReason::WrongPin)), 11);
        assert_eq!(
            exit_code(&rejected(RejectReason::UnsupportedVersion { host: 4 })),
            12
        );
        assert_eq!(
            exit_code(&rejected(RejectReason::Other("busy".into()))),
            GENERIC_EXIT_CODE
        );
        let changed = ConnectFailure::HostChanged(IdentityMismatch {
            expected: Fingerprint([1; 32]),
            actual: Fingerprint([2; 32]),
        });
        assert_eq!(exit_code(&anyhow::Error::new(changed)), 13);
        assert_eq!(exit_code(&anyhow!("timed out")), GENERIC_EXIT_CODE);
    }

    #[test]
    fn context_does_not_hide_the_cause() {
        let error = anyhow::Error::new(ConnectFailure::Rejected(RejectReason::PinRequired))
            .context("connecting to UFFICIO");
        assert_eq!(exit_code(&error), 10);
    }

    #[test]
    fn messages_for_terminals() {
        assert_eq!(
            ConnectFailure::Rejected(RejectReason::WrongPin).to_string(),
            "the host refused: wrong PIN"
        );
        let changed = ConnectFailure::HostChanged(IdentityMismatch {
            expected: Fingerprint([1; 32]),
            actual: Fingerprint([2; 32]),
        });
        assert!(
            changed
                .to_string()
                .starts_with("the host's identity changed")
        );
    }
}
