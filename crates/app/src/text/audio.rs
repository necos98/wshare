//! The Italian texts of the shared sound: the viewer's "Audio" button, the
//! host setting and its warning. A part of [`super`], re-exported there.

// --- Viewer window ------------------------------------------------------

/// The "Audio" button while the host's sound plays.
pub const BAR_AUDIO_ON: &str = "Audio";
/// The same button while it is muted.
pub const BAR_AUDIO_OFF: &str = "Audio (muto)";
pub const BAR_TIP_AUDIO_ON: &str = "Disattiva l'audio dell'altro computer";
pub const BAR_TIP_AUDIO_OFF: &str = "Riattiva l'audio dell'altro computer";

// --- Desktop interface --------------------------------------------------

pub const SETTING_SHARE_AUDIO: &str = "Condividi audio";
pub const SETTING_SHARE_AUDIO_HINT: &str =
    "Il visualizzatore sente l'audio di questo PC; può disattivarlo dalla sua barra.";

/// The warning of a session whose sound cannot be captured.
pub fn audio_unavailable(detail: &str) -> String {
    format!("Impossibile condividere l'audio: la sessione continua senza ({detail}).")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_warning_says_the_session_goes_on() {
        let text = audio_unavailable("nessun dispositivo");
        assert!(text.contains("continua senza"), "{text}");
        assert!(text.ends_with("(nessun dispositivo)."), "{text}");
    }
}
