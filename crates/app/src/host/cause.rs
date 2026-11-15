//! Typed reasons the sharing service reports, created where the problem
//! happens (the bind, the monitor lookup, the session, the input injector)
//! so a front-end can decide and word them without reading messages. The
//! `Display` is the English text of the terminal; nothing reads it back.

use std::fmt;
use std::io;

/// OS error of a UDP bind on a port that is taken (`WSAEADDRINUSE`).
const WSAEADDRINUSE: i32 = 10048;

/// Why sharing could not start or ended on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostFailure {
    PortInUse {
        port: u16,
    },
    MonitorNotFound {
        index: usize,
    },
    /// A thread of the service panicked.
    Crashed(String),
    Failed(String),
}

impl fmt::Display for HostFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PortInUse { port } => write!(f, "UDP port {port} is already in use"),
            Self::MonitorNotFound { index } => write!(f, "monitor {index} not found"),
            Self::Crashed(detail) => write!(f, "the sharing service crashed: {detail}"),
            Self::Failed(detail) => f.write_str(detail),
        }
    }
}

impl std::error::Error for HostFailure {}

impl HostFailure {
    /// Types the error of binding the listening port: a port that is taken
    /// is [`Self::PortInUse`], whatever else wraps it. The `io::Error`
    /// where the bind failed is the source of truth, not its message.
    pub fn from_bind(error: anyhow::Error, port: u16) -> anyhow::Error {
        if error.chain().any(is_address_in_use) {
            Self::PortInUse { port }.into()
        } else {
            error.context(format!("listening on UDP port {port}"))
        }
    }

    /// The failure inside `error`, or [`Self::Failed`] with its text.
    pub fn of(error: &anyhow::Error) -> Self {
        error
            .downcast_ref::<Self>()
            .cloned()
            .unwrap_or_else(|| Self::Failed(format!("{error:#}")))
    }
}

fn is_address_in_use(cause: &(dyn std::error::Error + 'static)) -> bool {
    cause.downcast_ref::<io::Error>().is_some_and(|e| {
        e.kind() == io::ErrorKind::AddrInUse || e.raw_os_error() == Some(WSAEADDRINUSE)
    })
}

/// A viewing session that ended abnormally on our side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFailure(pub String);

impl fmt::Display for SessionFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Something non-fatal worth showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostWarning {
    /// Viewers can still connect by IP.
    LanAnnouncementFailed(String),
    /// The authorized devices could not be read: every viewer needs the PIN.
    AuthorizedDevicesUnreadable(String),
    DeviceNotRemembered {
        name: String,
        detail: String,
    },
    DeviceRemovalNotSaved(String),
    /// Too many wrong PINs arrived lately: the PIN was replaced (the new
    /// one came with `PinChanged`).
    PinRotatedAfterWrongAttempts,
    /// Injecting a viewer's input failed; `suppressed` similar failures
    /// were not reported since the last warning.
    InputFailed {
        detail: String,
        suppressed: u32,
    },
    /// The encoder refused to change its bitrate while streaming: this
    /// session stays at the rate it had, whatever the network does.
    BitrateFixed(String),
}

impl fmt::Display for HostWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LanAnnouncementFailed(detail) => {
                write!(f, "LAN announcement failed: {detail}")
            }
            Self::AuthorizedDevicesUnreadable(detail) => {
                write!(f, "every viewer needs the PIN again: {detail}")
            }
            Self::DeviceNotRemembered { name, detail } => {
                write!(f, "cannot remember \"{name}\": {detail}")
            }
            Self::DeviceRemovalNotSaved(detail) => {
                write!(f, "cannot save the removal: {detail}")
            }
            Self::PinRotatedAfterWrongAttempts => {
                f.write_str("the PIN was changed after too many wrong attempts")
            }
            Self::InputFailed {
                detail,
                suppressed: 0,
            } => write!(f, "remote input failed: {detail}"),
            Self::InputFailed { detail, suppressed } => write!(
                f,
                "remote input failed: {detail} ({suppressed} similar failures not shown)"
            ),
            Self::BitrateFixed(detail) => write!(
                f,
                "the bitrate cannot follow the network, it stays fixed: {detail}"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::{Context, anyhow};

    use super::*;

    fn bind_error(kind: io::ErrorKind) -> anyhow::Error {
        anyhow::Error::new(io::Error::new(kind, "bind failed")).context("binding UDP 0.0.0.0:7420")
    }

    #[test]
    fn a_taken_port_is_recognised_where_it_is_bound() {
        let error = HostFailure::from_bind(bind_error(io::ErrorKind::AddrInUse), 7420);
        assert_eq!(
            HostFailure::of(&error),
            HostFailure::PortInUse { port: 7420 }
        );
    }

    #[test]
    fn the_os_error_code_counts_too() {
        let raw = anyhow::Error::new(io::Error::from_raw_os_error(WSAEADDRINUSE))
            .context("binding UDP 0.0.0.0:7420");
        let error = HostFailure::from_bind(raw, 7420);
        assert_eq!(
            HostFailure::of(&error),
            HostFailure::PortInUse { port: 7420 }
        );
    }

    #[test]
    fn other_bind_errors_keep_their_text_and_say_where() {
        let error = HostFailure::from_bind(bind_error(io::ErrorKind::PermissionDenied), 7420);
        assert!(error.downcast_ref::<HostFailure>().is_none());
        let text = format!("{error:#}");
        assert!(text.starts_with("listening on UDP port 7420: "), "{text}");
        assert!(text.contains("bind failed"), "{text}");
    }

    #[test]
    fn the_message_of_an_error_decides_nothing() {
        let lookalike = anyhow!("address already in use (10048)");
        let error = HostFailure::from_bind(lookalike, 7420);
        assert!(matches!(HostFailure::of(&error), HostFailure::Failed(_)));
    }

    #[test]
    fn failures_survive_context() {
        let error = anyhow::Error::new(HostFailure::MonitorNotFound { index: 3 })
            .context("starting the service");
        assert_eq!(
            HostFailure::of(&error),
            HostFailure::MonitorNotFound { index: 3 }
        );
        let untyped = Err::<(), _>(anyhow!("disk full"))
            .context("saving")
            .unwrap_err();
        assert_eq!(
            HostFailure::of(&untyped),
            HostFailure::Failed("saving: disk full".into())
        );
    }

    #[test]
    fn messages_for_terminals() {
        assert_eq!(
            HostFailure::PortInUse { port: 7420 }.to_string(),
            "UDP port 7420 is already in use"
        );
        assert_eq!(
            HostFailure::MonitorNotFound { index: 2 }.to_string(),
            "monitor 2 not found"
        );
        assert_eq!(
            HostFailure::Crashed("boom".into()).to_string(),
            "the sharing service crashed: boom"
        );
        let input = |suppressed| HostWarning::InputFailed {
            detail: "SendInput failed".into(),
            suppressed,
        };
        assert_eq!(
            input(0).to_string(),
            "remote input failed: SendInput failed"
        );
        assert_eq!(
            input(2).to_string(),
            "remote input failed: SendInput failed (2 similar failures not shown)"
        );
        assert_eq!(
            HostWarning::DeviceNotRemembered {
                name: "PC".into(),
                detail: "disk full".into()
            }
            .to_string(),
            "cannot remember \"PC\": disk full"
        );
        assert_eq!(
            HostWarning::PinRotatedAfterWrongAttempts.to_string(),
            "the PIN was changed after too many wrong attempts"
        );
        assert_eq!(
            HostWarning::BitrateFixed("driver refused".into()).to_string(),
            "the bitrate cannot follow the network, it stays fixed: driver refused"
        );
    }
}
