//! The errors of `wshare view` that the caller may act on, created where
//! they happen (connect, the host's refusal, the end of the session), and
//! the [`FailureCause`] each one reports. Their `Display` is the English
//! text for terminals; nothing reads it back.

use std::fmt;

use net::{ConnectError, IdentityMismatch};
use protocol::control::RejectReason;

use super::report::FailureCause;
use crate::printable;

/// Longest error text kept, in characters: an error may carry words the
/// host chose (e.g. the reason of the QUIC close it sent), and a few
/// hundred characters say enough of what went wrong.
const MAX_DETAIL_CHARS: usize = 512;

/// A failure of the viewer with a known cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerFailure {
    Rejected(RejectReason),
    /// The remembered host answered with another certificate.
    HostChanged(IdentityMismatch),
    /// Nothing answered in time; the text says where.
    TimedOut(String),
    /// The connection could not be established; the text says why.
    Unreachable(String),
    /// The host stopped sharing or closed the connection.
    HostLeft,
    /// The host ended this session on purpose (it said goodbye).
    HostDisconnected,
    /// The connection broke during the session; the text says how.
    ConnectionLost(String),
}

impl fmt::Display for ViewerFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(reason) => write!(f, "the host refused: {reason}"),
            Self::HostChanged(mismatch) => write!(
                f,
                "{mismatch}. Another computer may be answering at this address; if the \
                 host was reinstalled, forget it in wshare and connect again with the PIN"
            ),
            Self::TimedOut(text) | Self::Unreachable(text) => f.write_str(text),
            Self::HostLeft => f.write_str("the host stopped sharing"),
            Self::HostDisconnected => f.write_str("the host ended the session"),
            Self::ConnectionLost(text) => write!(f, "connection lost: {text}"),
        }
    }
}

impl std::error::Error for ViewerFailure {}

impl ViewerFailure {
    /// The host did not answer our `Hello`.
    pub fn no_answer() -> Self {
        Self::TimedOut("the host did not answer".to_string())
    }

    /// The connection broke because of `error`, whose text may carry the
    /// host's words: kept without control characters and bounded.
    pub fn connection_lost(error: impl fmt::Display) -> Self {
        Self::ConnectionLost(detail(&format!("{error:#}")))
    }

    pub fn cause(&self) -> FailureCause {
        match self {
            Self::Rejected(RejectReason::PinRequired) => FailureCause::PinRequired,
            Self::Rejected(RejectReason::WrongPin) => FailureCause::WrongPin,
            Self::Rejected(RejectReason::UnsupportedVersion { .. }) => {
                FailureCause::UnsupportedVersion
            }
            Self::Rejected(RejectReason::CannotStream(cause)) => {
                FailureCause::HostCannotStream(cause.clone())
            }
            Self::Rejected(RejectReason::Busy) => FailureCause::HostBusy,
            Self::Rejected(RejectReason::TooManyAttempts) => FailureCause::TooManyAttempts,
            Self::Rejected(RejectReason::Denied) => FailureCause::Denied,
            Self::Rejected(RejectReason::Expired) => FailureCause::ApprovalExpired,
            Self::Rejected(RejectReason::TooManyRequests) => FailureCause::TooManyRequests,
            Self::Rejected(RejectReason::PasswordRequired) => FailureCause::PasswordRequired,
            Self::Rejected(RejectReason::WrongPassword) => FailureCause::WrongPassword,
            Self::Rejected(RejectReason::NotAuthorized) => FailureCause::NotAuthorized,
            Self::Rejected(RejectReason::HostStopped) => FailureCause::HostLeft,
            Self::Rejected(
                RejectReason::UnexpectedMessage
                | RejectReason::NoHello
                | RejectReason::LeftBeforeHello
                | RejectReason::UnreadableHello(_)
                | RejectReason::LeftBeforeWelcome(_),
            ) => FailureCause::HandshakeFailed,
            Self::Rejected(RejectReason::Other(text)) => FailureCause::Other(text.clone()),
            Self::HostChanged(_) => FailureCause::HostChanged,
            Self::TimedOut(_) => FailureCause::TimedOut,
            Self::Unreachable(_) => FailureCause::Unreachable,
            Self::HostLeft => FailureCause::HostLeft,
            Self::HostDisconnected => FailureCause::HostDisconnected,
            Self::ConnectionLost(_) => FailureCause::ConnectionLost,
        }
    }
}

/// Types the error of `net::connect`: a pinned host that presented another
/// certificate, a connection that timed out, or one that could not be
/// established. Anything else (a local problem) passes through.
pub fn connect_failure(error: anyhow::Error) -> anyhow::Error {
    let error = match error.downcast::<IdentityMismatch>() {
        Ok(mismatch) => return ViewerFailure::HostChanged(mismatch).into(),
        Err(error) => error,
    };
    match error.downcast_ref::<ConnectError>() {
        Some(ConnectError::TimedOut { .. }) => {
            ViewerFailure::TimedOut(detail(&error.to_string())).into()
        }
        Some(ConnectError::Failed { .. }) => {
            ViewerFailure::Unreachable(detail(&error.to_string())).into()
        }
        None => error,
    }
}

/// What is kept of an error's `text`, which may carry the host's words.
fn detail(text: &str) -> String {
    printable::clean(text, MAX_DETAIL_CHARS)
}

/// Why `wshare view` ended with `error`.
pub fn cause_of(error: &anyhow::Error) -> FailureCause {
    error.downcast_ref::<ViewerFailure>().map_or_else(
        || FailureCause::Other(format!("{error:#}")),
        ViewerFailure::cause,
    )
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use net::Fingerprint;

    use super::*;

    fn mismatch() -> IdentityMismatch {
        IdentityMismatch {
            expected: Fingerprint([1; 32]),
            actual: Fingerprint([2; 32]),
        }
    }

    fn rejected(reason: RejectReason) -> anyhow::Error {
        ViewerFailure::Rejected(reason).into()
    }

    #[test]
    fn rejections_map_to_their_causes() {
        let cases = [
            (RejectReason::PinRequired, FailureCause::PinRequired),
            (RejectReason::WrongPin, FailureCause::WrongPin),
            (
                RejectReason::UnsupportedVersion { host: 4 },
                FailureCause::UnsupportedVersion,
            ),
            (
                RejectReason::CannotStream("no encoder".into()),
                FailureCause::HostCannotStream("no encoder".into()),
            ),
            (RejectReason::Busy, FailureCause::HostBusy),
            (RejectReason::TooManyAttempts, FailureCause::TooManyAttempts),
            (RejectReason::Denied, FailureCause::Denied),
            (RejectReason::Expired, FailureCause::ApprovalExpired),
            (RejectReason::TooManyRequests, FailureCause::TooManyRequests),
            (
                RejectReason::PasswordRequired,
                FailureCause::PasswordRequired,
            ),
            (RejectReason::WrongPassword, FailureCause::WrongPassword),
            (RejectReason::NotAuthorized, FailureCause::NotAuthorized),
            (RejectReason::HostStopped, FailureCause::HostLeft),
            (
                RejectReason::UnexpectedMessage,
                FailureCause::HandshakeFailed,
            ),
            (RejectReason::NoHello, FailureCause::HandshakeFailed),
            (RejectReason::LeftBeforeHello, FailureCause::HandshakeFailed),
            (
                RejectReason::UnreadableHello("x".into()),
                FailureCause::HandshakeFailed,
            ),
            (
                RejectReason::LeftBeforeWelcome("x".into()),
                FailureCause::HandshakeFailed,
            ),
            (
                RejectReason::Other("busy".into()),
                FailureCause::Other("busy".into()),
            ),
        ];
        for (reason, cause) in cases {
            assert_eq!(cause_of(&rejected(reason)), cause);
        }
    }

    #[test]
    fn the_wording_of_a_rejection_decides_nothing() {
        let tricky = rejected(RejectReason::Other("wrong PIN".into()));
        assert_eq!(cause_of(&tricky), FailureCause::Other("wrong PIN".into()));
    }

    #[test]
    fn session_and_connection_failures_map_to_their_causes() {
        let cases = [
            (
                ViewerFailure::HostChanged(mismatch()),
                FailureCause::HostChanged,
            ),
            (ViewerFailure::no_answer(), FailureCause::TimedOut),
            (
                ViewerFailure::Unreachable("refused".into()),
                FailureCause::Unreachable,
            ),
            (ViewerFailure::HostLeft, FailureCause::HostLeft),
            (
                ViewerFailure::HostDisconnected,
                FailureCause::HostDisconnected,
            ),
            (
                ViewerFailure::ConnectionLost("reset".into()),
                FailureCause::ConnectionLost,
            ),
        ];
        for (failure, cause) in cases {
            assert_eq!(cause_of(&failure.into()), cause);
        }
    }

    #[test]
    fn untyped_errors_are_other_with_their_text() {
        assert_eq!(
            cause_of(&anyhow!("timed out")),
            FailureCause::Other("timed out".into())
        );
    }

    #[test]
    fn context_does_not_hide_the_cause() {
        let error = rejected(RejectReason::PinRequired).context("connecting to UFFICIO");
        assert_eq!(cause_of(&error), FailureCause::PinRequired);
    }

    #[test]
    fn connect_errors_are_typed_at_the_source() {
        let addr = "10.0.0.9:7420".parse().unwrap();
        let timed_out = anyhow::Error::new(ConnectError::TimedOut { addr });
        assert_eq!(
            cause_of(&connect_failure(timed_out)),
            FailureCause::TimedOut
        );
        assert_eq!(
            cause_of(&connect_failure(anyhow::Error::new(mismatch()))),
            FailureCause::HostChanged
        );
        let local = connect_failure(anyhow!("building QUIC client crypto"));
        assert!(local.downcast_ref::<ViewerFailure>().is_none());
    }

    #[test]
    fn a_lost_connection_keeps_no_control_characters_of_the_host() {
        assert_eq!(
            ViewerFailure::connection_lost("closed by peer: x\r\n@wshare:connected:EVIL"),
            ViewerFailure::ConnectionLost("closed by peer: x@wshare:connected:EVIL".into())
        );
        let chained = anyhow!("reset").context("sending");
        assert_eq!(
            ViewerFailure::connection_lost(&chained),
            ViewerFailure::ConnectionLost("sending: reset".into())
        );
        let ViewerFailure::ConnectionLost(long) = ViewerFailure::connection_lost("è".repeat(4096))
        else {
            unreachable!("a lost connection")
        };
        assert_eq!(long.chars().count(), MAX_DETAIL_CHARS);
    }

    #[test]
    fn messages_for_terminals() {
        assert_eq!(
            ViewerFailure::Rejected(RejectReason::WrongPin).to_string(),
            "the host refused: wrong PIN"
        );
        assert!(
            ViewerFailure::HostChanged(mismatch())
                .to_string()
                .starts_with("the host's identity changed")
        );
        assert_eq!(
            ViewerFailure::HostLeft.to_string(),
            "the host stopped sharing"
        );
        assert_eq!(
            ViewerFailure::HostDisconnected.to_string(),
            "the host ended the session"
        );
        assert_eq!(
            ViewerFailure::ConnectionLost("reset".into()).to_string(),
            "connection lost: reset"
        );
    }
}
