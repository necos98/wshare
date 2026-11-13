//! Turns what a viewer process wrote on stderr (English, for terminals)
//! into a short Italian message for the interface. Pure.

/// The message for a viewer that exited with failure.
pub fn describe(stderr: &str, code: Option<i32>) -> String {
    match last_error(stderr) {
        Some(error) => translate_error(error),
        None => match code {
            Some(code) => {
                format!("Il visualizzatore si è chiuso inaspettatamente (codice {code}).")
            }
            None => "Il visualizzatore si è chiuso inaspettatamente.".to_string(),
        },
    }
}

/// The text of the last `error: ...` line, or the last non-empty line.
fn last_error(stderr: &str) -> Option<&str> {
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines
        .iter()
        .rev()
        .find_map(|l| l.strip_prefix("error:"))
        .or_else(|| lines.last().copied())
        .map(str::trim)
}

fn translate_error(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if let Some(reason) = error.strip_prefix("the host refused:") {
        let reason = translate_reason(reason.trim());
        return if reason == "PIN errato" {
            "PIN errato.".to_string()
        } else {
            format!("Il computer ha rifiutato la connessione: {reason}.")
        };
    }
    let known = [
        ("did not answer", "Il computer non ha risposto."),
        ("timed out", "Il computer non risponde: è acceso e raggiungibile? Controlla il firewall."),
        ("connecting to", "Impossibile raggiungere il computer: è acceso e raggiungibile?"),
        ("closed the connection", "Il computer ha chiuso la connessione."),
        ("stopped sharing", "Il computer ha interrotto la condivisione."),
        ("connection lost", "Connessione persa."),
    ];
    known
        .iter()
        .find(|(needle, _)| lower.contains(needle))
        .map_or_else(|| format!("Errore: {error}"), |(_, message)| message.to_string())
}

/// Translates a rejection reason chosen by the host (see `handshake`).
pub fn translate_reason(reason: &str) -> String {
    if reason == "wrong PIN" {
        "PIN errato".to_string()
    } else if reason.starts_with("protocol version") {
        "versione di wshare diversa, aggiorna entrambi i computer".to_string()
    } else if let Some(cause) = reason.strip_prefix("host cannot stream:") {
        format!("impossibile trasmettere lo schermo ({})", cause.trim())
    } else {
        reason.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_pin() {
        assert_eq!(describe("error: the host refused: wrong PIN\n", Some(1)), "PIN errato.");
    }

    #[test]
    fn version_mismatch() {
        let stderr = "error: the host refused: protocol version 1 not supported (host uses 2)";
        assert_eq!(
            describe(stderr, Some(1)),
            "Il computer ha rifiutato la connessione: versione di wshare diversa, aggiorna \
             entrambi i computer."
        );
    }

    #[test]
    fn unreachable_host() {
        let stderr = "error: connecting to 10.0.0.9:7420: timed out\n";
        assert!(describe(stderr, Some(1)).starts_with("Il computer non risponde"));
        let stderr = "error: connecting to 10.0.0.9:7420: connection refused";
        assert!(describe(stderr, Some(1)).starts_with("Impossibile raggiungere"));
    }

    #[test]
    fn host_left_during_session() {
        let stderr = "Session: 10 frames shown\nerror: the host stopped sharing\n";
        assert_eq!(describe(stderr, Some(1)), "Il computer ha interrotto la condivisione.");
    }

    #[test]
    fn last_error_line_wins_and_unknown_errors_pass_through() {
        let stderr = "error: first\nnoise\nerror: something odd\n";
        assert_eq!(describe(stderr, Some(1)), "Errore: something odd");
        assert_eq!(describe("panicked at foo\n", Some(101)), "Errore: panicked at foo");
    }

    #[test]
    fn silent_failure_uses_exit_code() {
        assert_eq!(
            describe("  \n", Some(3)),
            "Il visualizzatore si è chiuso inaspettatamente (codice 3)."
        );
        assert_eq!(describe("", None), "Il visualizzatore si è chiuso inaspettatamente.");
    }

    #[test]
    fn rejection_reasons() {
        assert_eq!(translate_reason("wrong PIN"), "PIN errato");
        assert_eq!(
            translate_reason("host cannot stream: no encoder"),
            "impossibile trasmettere lo schermo (no encoder)"
        );
        assert_eq!(translate_reason("busy"), "busy");
    }
}
