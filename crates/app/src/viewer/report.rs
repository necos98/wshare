//! What `wshare view` tells the desktop interface that launched it.
//!
//! The interface runs the viewer as a child process and decides only from
//! this contract, never from the wording of messages (those are English, for
//! terminals):
//! - a status line on stdout, written only when [`REPORT_ENV`] is set:
//!   `@wshare:awaiting-approval` when the host asked the person at its PC to
//!   accept this one (the handshake goes on once they answered),
//!   `@wshare:connected:<host name>` once the handshake succeeded, or
//!   `@wshare:failed:<cause>[:<detail>]` when the viewer ends because of a
//!   [`FailureCause`];
//! - the exit code: 0 for a normal end, [`FailureCause::exit_code`] after a
//!   failure (101 is a panic). The code alone still names the causes that
//!   carry no detail, e.g. when the process died before printing the line.

/// Environment variable that switches the status lines on (value
/// [`REPORT_ENABLED`]); set by the desktop interface, never by terminals.
pub const REPORT_ENV: &str = "WSHARE_REPORT";
pub const REPORT_ENABLED: &str = "1";

/// Exit code of a failure that has no cause of its own.
pub const GENERIC_EXIT_CODE: i32 = 1;

const LINE_PREFIX: &str = "@wshare:";
const SEPARATOR: char = ':';
const CONNECTED_TAG: &str = "connected";
const FAILED_TAG: &str = "failed";
const AWAITING_APPROVAL_TAG: &str = "awaiting-approval";

/// Why a viewer could not connect or had to end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureCause {
    /// The host could not be reached (refused or reset, not a silence).
    Unreachable,
    /// Nothing answered in time.
    TimedOut,
    PinRequired,
    WrongPin,
    UnsupportedVersion,
    /// The remembered host answered with another certificate.
    HostChanged,
    /// The host stopped sharing or closed the connection.
    HostLeft,
    /// The host ended this session on purpose (it said goodbye).
    HostDisconnected,
    /// The connection broke during the session.
    ConnectionLost,
    /// Another viewer is connected to the host.
    HostBusy,
    /// The host pauses this PC after too many wrong PINs.
    TooManyAttempts,
    /// The person at the host refused this PC.
    Denied,
    /// Nobody at the host answered the request in time.
    ApprovalExpired,
    /// This PC asked the host for approval too often.
    TooManyRequests,
    /// The host refused because it cannot stream its screen; the text is
    /// the host's own explanation.
    HostCannotStream(String),
    Other(String),
}

/// The causes whose exit code identifies them (no detail needed).
const CODED_CAUSES: [FailureCause; 14] = [
    FailureCause::PinRequired,
    FailureCause::WrongPin,
    FailureCause::UnsupportedVersion,
    FailureCause::HostChanged,
    FailureCause::Unreachable,
    FailureCause::TimedOut,
    FailureCause::HostLeft,
    FailureCause::HostDisconnected,
    FailureCause::ConnectionLost,
    FailureCause::HostBusy,
    FailureCause::TooManyAttempts,
    FailureCause::Denied,
    FailureCause::ApprovalExpired,
    FailureCause::TooManyRequests,
];

impl FailureCause {
    /// Distinct from 0 (success) and 101 (panic).
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::PinRequired => 10,
            Self::WrongPin => 11,
            Self::UnsupportedVersion => 12,
            Self::HostChanged => 13,
            Self::Unreachable => 14,
            Self::TimedOut => 15,
            Self::HostLeft => 16,
            Self::ConnectionLost => 17,
            Self::HostCannotStream(_) => 18,
            Self::HostBusy => 19,
            Self::TooManyAttempts => 20,
            Self::HostDisconnected => 21,
            Self::Denied => 22,
            Self::ApprovalExpired => 23,
            Self::TooManyRequests => 24,
            Self::Other(_) => GENERIC_EXIT_CODE,
        }
    }

    /// The cause an exit code names on its own; `None` for success, panics,
    /// the generic failure and the causes that need their detail.
    pub fn from_exit_code(code: i32) -> Option<Self> {
        CODED_CAUSES
            .into_iter()
            .find(|cause| cause.exit_code() == code)
    }

    fn tag(&self) -> &'static str {
        match self {
            Self::Unreachable => "unreachable",
            Self::TimedOut => "timed-out",
            Self::PinRequired => "pin-required",
            Self::WrongPin => "wrong-pin",
            Self::UnsupportedVersion => "unsupported-version",
            Self::HostChanged => "host-changed",
            Self::HostLeft => "host-left",
            Self::HostDisconnected => "host-disconnected",
            Self::ConnectionLost => "connection-lost",
            Self::HostBusy => "host-busy",
            Self::TooManyAttempts => "too-many-attempts",
            Self::Denied => "denied",
            Self::ApprovalExpired => "approval-expired",
            Self::TooManyRequests => "too-many-requests",
            Self::HostCannotStream(_) => "host-cannot-stream",
            Self::Other(_) => "other",
        }
    }

    fn detail(&self) -> Option<&str> {
        match self {
            Self::HostCannotStream(detail) | Self::Other(detail) => Some(detail),
            _ => None,
        }
    }

    fn from_tag(tag: &str, detail: &str) -> Option<Self> {
        let detail = || detail.to_string();
        match tag {
            "host-cannot-stream" => Some(Self::HostCannotStream(detail())),
            "other" => Some(Self::Other(detail())),
            _ => CODED_CAUSES.into_iter().find(|cause| cause.tag() == tag),
        }
    }
}

/// One status line of the viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerReport {
    /// The host asked the person at its PC to accept this one; the answer
    /// is still to come.
    AwaitingApproval,
    Connected {
        host_name: String,
    },
    Failed(FailureCause),
}

impl ViewerReport {
    /// The stdout line, without the line break.
    pub fn to_line(&self) -> String {
        match self {
            Self::AwaitingApproval => format!("{LINE_PREFIX}{AWAITING_APPROVAL_TAG}"),
            Self::Connected { host_name } => {
                format!(
                    "{LINE_PREFIX}{CONNECTED_TAG}{SEPARATOR}{}",
                    single_line(host_name)
                )
            }
            Self::Failed(cause) => {
                let mut line = format!("{LINE_PREFIX}{FAILED_TAG}{SEPARATOR}{}", cause.tag());
                if let Some(detail) = cause.detail() {
                    line.push(SEPARATOR);
                    line.push_str(&single_line(detail));
                }
                line
            }
        }
    }

    /// The report in `line`; `None` for any other line (ordinary output) or
    /// for a malformed one.
    pub fn parse(line: &str) -> Option<Self> {
        let rest = line.trim_end().strip_prefix(LINE_PREFIX)?;
        if rest == AWAITING_APPROVAL_TAG {
            return Some(Self::AwaitingApproval);
        }
        let (kind, rest) = rest.split_once(SEPARATOR)?;
        match kind {
            CONNECTED_TAG => Some(Self::Connected {
                host_name: rest.to_string(),
            }),
            FAILED_TAG => {
                let (tag, detail) = rest.split_once(SEPARATOR).unwrap_or((rest, ""));
                FailureCause::from_tag(tag, detail).map(Self::Failed)
            }
            _ => None,
        }
    }
}

/// Whether the status lines are wanted, from the value of [`REPORT_ENV`].
pub fn reporting_enabled(env: Option<&str>) -> bool {
    env == Some(REPORT_ENABLED)
}

/// Line breaks would end the report early (a host picks its own name).
fn single_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_cause() -> Vec<FailureCause> {
        let mut causes = CODED_CAUSES.to_vec();
        causes.push(FailureCause::HostCannotStream("no encoder".into()));
        causes.push(FailureCause::Other("something: odd".into()));
        causes
    }

    #[test]
    fn every_report_survives_its_line() {
        let mut reports: Vec<ViewerReport> = every_cause()
            .into_iter()
            .map(ViewerReport::Failed)
            .collect();
        reports.push(ViewerReport::Connected {
            host_name: "UFFICIO: sala 2".into(),
        });
        reports.push(ViewerReport::Connected {
            host_name: String::new(),
        });
        reports.push(ViewerReport::AwaitingApproval);
        for report in reports {
            assert_eq!(ViewerReport::parse(&report.to_line()), Some(report));
        }
    }

    #[test]
    fn lines_have_a_fixed_shape() {
        let connected = ViewerReport::Connected {
            host_name: "UFFICIO".into(),
        };
        assert_eq!(connected.to_line(), "@wshare:connected:UFFICIO");
        assert_eq!(
            ViewerReport::Failed(FailureCause::WrongPin).to_line(),
            "@wshare:failed:wrong-pin"
        );
        assert_eq!(
            ViewerReport::Failed(FailureCause::HostBusy).to_line(),
            "@wshare:failed:host-busy"
        );
        assert_eq!(
            ViewerReport::Failed(FailureCause::TooManyAttempts).to_line(),
            "@wshare:failed:too-many-attempts"
        );
        assert_eq!(
            ViewerReport::Failed(FailureCause::HostDisconnected).to_line(),
            "@wshare:failed:host-disconnected"
        );
        assert_eq!(
            ViewerReport::Failed(FailureCause::Denied).to_line(),
            "@wshare:failed:denied"
        );
        assert_eq!(
            ViewerReport::Failed(FailureCause::ApprovalExpired).to_line(),
            "@wshare:failed:approval-expired"
        );
        assert_eq!(
            ViewerReport::Failed(FailureCause::TooManyRequests).to_line(),
            "@wshare:failed:too-many-requests"
        );
        assert_eq!(
            ViewerReport::AwaitingApproval.to_line(),
            "@wshare:awaiting-approval"
        );
        assert_eq!(
            ViewerReport::Failed(FailureCause::HostCannotStream("no encoder".into())).to_line(),
            "@wshare:failed:host-cannot-stream:no encoder"
        );
    }

    #[test]
    fn a_host_name_cannot_break_the_line() {
        let report = ViewerReport::Connected {
            host_name: "A\r\n@wshare:failed:wrong-pin".into(),
        };
        let line = report.to_line();
        assert!(!line.contains(['\r', '\n']));
        assert!(matches!(
            ViewerReport::parse(&line),
            Some(ViewerReport::Connected { .. })
        ));
    }

    #[test]
    fn ordinary_and_malformed_lines_are_not_reports() {
        for line in [
            "",
            "Connected to \"UFFICIO\" (10.0.0.2:7420) - 1920x1080 @ 60 fps H264",
            "Host fingerprint: abc",
            "@wshare:",
            "@wshare:connected",
            "@wshare:failed:no-such-cause",
            "@wshare:failed:",
            "@wshare:other:x",
            " @wshare:connected:A",
        ] {
            assert_eq!(ViewerReport::parse(line), None, "{line:?}");
        }
    }

    #[test]
    fn a_trailing_line_break_is_ignored() {
        assert_eq!(
            ViewerReport::parse("@wshare:failed:host-left\r\n"),
            Some(ViewerReport::Failed(FailureCause::HostLeft))
        );
    }

    #[test]
    fn exit_codes_are_distinct_and_name_the_coded_causes() {
        let mut codes: Vec<i32> = every_cause()
            .iter()
            .filter(|cause| !matches!(cause, FailureCause::Other(_)))
            .map(FailureCause::exit_code)
            .collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), CODED_CAUSES.len() + 1);
        assert!(
            codes
                .iter()
                .all(|code| ![0, 101, GENERIC_EXIT_CODE].contains(code))
        );
        assert_eq!(
            FailureCause::Other(String::new()).exit_code(),
            GENERIC_EXIT_CODE
        );
        for cause in CODED_CAUSES {
            assert_eq!(FailureCause::from_exit_code(cause.exit_code()), Some(cause));
        }
        for code in [0, GENERIC_EXIT_CODE, 18, 101, -1] {
            assert_eq!(FailureCause::from_exit_code(code), None);
        }
    }

    #[test]
    fn reporting_is_opt_in() {
        assert!(reporting_enabled(Some("1")));
        assert!(!reporting_enabled(None));
        assert!(!reporting_enabled(Some("0")));
        assert!(!reporting_enabled(Some("")));
    }
}
