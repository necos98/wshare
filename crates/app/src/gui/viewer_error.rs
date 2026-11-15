//! Short Italian messages for the interface: why a viewer process failed
//! (from its exit code, else from what it wrote on stderr, which is English
//! for terminals) and why the host refused a viewer. Pure.

use protocol::control::RejectReason;

use crate::viewer::failure::FailureKind;

/// What a viewer process that exited with failure means for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerFailure {
    /// Set when the viewer reported the cause in its exit code.
    pub kind: Option<FailureKind>,
    pub message: String,
}

pub fn describe(stderr: &str, code: Option<i32>) -> ViewerFailure {
    let kind = code.and_then(FailureKind::from_exit_code);
    let message = match (kind, last_error(stderr)) {
        (Some(kind), _) => kind_message(kind).to_string(),
        (None, Some(error)) => translate_error(error),
        (None, None) => match code {
            Some(code) => {
                format!("Il visualizzatore si è chiuso inaspettatamente (codice {code}).")
            }
            None => "Il visualizzatore si è chiuso inaspettatamente.".to_string(),
        },
    };
    ViewerFailure { kind, message }
}

fn kind_message(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::PinRequired => "Questo computer richiede il PIN.",
        FailureKind::WrongPin => "PIN errato.",
        FailureKind::UnsupportedVersion => {
            "Il computer ha rifiutato la connessione: versione di wshare diversa, aggiorna \
             entrambi i computer."
        }
        FailureKind::HostChanged => {
            "L'identità del computer è cambiata: potrebbe essere un altro computer. Se è stato \
             reinstallato, usa «Dimentica» e connettiti di nuovo con il PIN."
        }
    }
}

/// The text of the last `error: ...` line, or the last non-empty line.
fn last_error(stderr: &str) -> Option<&str> {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines
        .iter()
        .rev()
        .find_map(|l| l.strip_prefix("error:"))
        .or_else(|| lines.last().copied())
        .map(str::trim)
}

/// Errors without an exit code of their own (see `viewer::failure`).
fn translate_error(error: &str) -> String {
    if let Some(reason) = error.strip_prefix("the host refused:") {
        return format!(
            "Il computer ha rifiutato la connessione: {}.",
            other_reason(reason.trim())
        );
    }
    let lower = error.to_ascii_lowercase();
    let known = [
        ("did not answer", "Il computer non ha risposto."),
        (
            "timed out",
            "Il computer non risponde: è acceso e raggiungibile? Controlla il firewall.",
        ),
        (
            "connecting to",
            "Impossibile raggiungere il computer: è acceso e raggiungibile?",
        ),
        (
            "closed the connection",
            "Il computer ha chiuso la connessione.",
        ),
        (
            "stopped sharing",
            "Il computer ha interrotto la condivisione.",
        ),
        ("connection lost", "Connessione persa."),
    ];
    known
        .iter()
        .find(|(needle, _)| lower.contains(needle))
        .map_or_else(
            || format!("Errore: {error}"),
            |(_, message)| message.to_string(),
        )
}

/// Why the host refused a viewer, for the host's own notice line.
pub fn reason_text(reason: &RejectReason) -> String {
    match reason {
        RejectReason::PinRequired => "PIN non inserito".to_string(),
        RejectReason::WrongPin => "PIN errato".to_string(),
        RejectReason::UnsupportedVersion { .. } => {
            "versione di wshare diversa, aggiorna entrambi i computer".to_string()
        }
        RejectReason::Other(text) => other_reason(text),
    }
}

/// A free-text reason chosen by the host (see `host::session`).
fn other_reason(text: &str) -> String {
    match text.strip_prefix("host cannot stream:") {
        Some(cause) => format!("impossibile trasmettere lo schermo ({})", cause.trim()),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(stderr: &str, code: Option<i32>) -> String {
        describe(stderr, code).message
    }

    #[test]
    fn structured_failures_come_from_the_exit_code() {
        let pin_required = describe("error: whatever the text says\n", Some(10));
        assert_eq!(pin_required.kind, Some(FailureKind::PinRequired));
        assert_eq!(pin_required.message, "Questo computer richiede il PIN.");
        assert_eq!(message("", Some(11)), "PIN errato.");
        assert!(message("", Some(12)).contains("versione di wshare diversa"));
        let changed = describe("error: the host's identity changed", Some(13));
        assert_eq!(changed.kind, Some(FailureKind::HostChanged));
        assert!(
            changed
                .message
                .starts_with("L'identità del computer è cambiata")
        );
    }

    #[test]
    fn the_text_of_a_rejection_decides_nothing() {
        let failure = describe("error: the host refused: wrong PIN\n", Some(1));
        assert_eq!(failure.kind, None);
        assert_eq!(
            failure.message,
            "Il computer ha rifiutato la connessione: wrong PIN."
        );
    }

    #[test]
    fn other_rejections_keep_the_host_reason() {
        let stderr = "error: the host refused: host cannot stream: no encoder";
        assert_eq!(
            message(stderr, Some(1)),
            "Il computer ha rifiutato la connessione: impossibile trasmettere lo schermo (no \
             encoder)."
        );
    }

    #[test]
    fn unreachable_host() {
        let stderr = "error: connecting to 10.0.0.9:7420: timed out\n";
        assert!(message(stderr, Some(1)).starts_with("Il computer non risponde"));
        let stderr = "error: connecting to 10.0.0.9:7420: connection refused";
        assert!(message(stderr, Some(1)).starts_with("Impossibile raggiungere"));
    }

    #[test]
    fn host_left_during_session() {
        let stderr = "Session: 10 frames shown\nerror: the host stopped sharing\n";
        assert_eq!(
            message(stderr, Some(1)),
            "Il computer ha interrotto la condivisione."
        );
    }

    #[test]
    fn last_error_line_wins_and_unknown_errors_pass_through() {
        let stderr = "error: first\nwarning: noise\nerror: something odd\n";
        assert_eq!(message(stderr, Some(1)), "Errore: something odd");
        assert_eq!(
            message("panicked at foo\n", Some(101)),
            "Errore: panicked at foo"
        );
    }

    #[test]
    fn silent_failure_uses_exit_code() {
        let failure = describe("  \n", Some(3));
        assert_eq!(failure.kind, None);
        assert_eq!(
            failure.message,
            "Il visualizzatore si è chiuso inaspettatamente (codice 3)."
        );
        assert_eq!(
            message("", None),
            "Il visualizzatore si è chiuso inaspettatamente."
        );
    }

    #[test]
    fn rejection_reasons_for_the_host() {
        assert_eq!(reason_text(&RejectReason::WrongPin), "PIN errato");
        assert_eq!(reason_text(&RejectReason::PinRequired), "PIN non inserito");
        assert_eq!(
            reason_text(&RejectReason::UnsupportedVersion { host: 4 }),
            "versione di wshare diversa, aggiorna entrambi i computer"
        );
        assert_eq!(
            reason_text(&RejectReason::Other(
                "host cannot stream: no encoder".into()
            )),
            "impossibile trasmettere lo schermo (no encoder)"
        );
        assert_eq!(reason_text(&RejectReason::Other("busy".into())), "busy");
    }
}
