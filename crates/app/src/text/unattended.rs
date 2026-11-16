//! The Italian texts of unattended access: the settings section that turns
//! it on, the notice of a window whose sharing the service took over, the
//! password a host may ask for, the viewer's Ctrl+Alt+Del button and the
//! indicator on the host. A part of [`super`], re-exported there.

// --- Viewer window ------------------------------------------------------

pub const BAR_SECURE_ATTENTION: &str = "Ctrl+Alt+Canc";
pub const BAR_TIP_SECURE_ATTENTION: &str =
    "Invia Ctrl+Alt+Canc all'altro computer (solo con l'accesso non presidiato)";

// --- Connecting to a host -------------------------------------------------

/// Shown under a host whose unattended access asks for its password.
pub const PASSWORD_AGAIN: &str = "Questo computer richiede la password di accesso non presidiato.";
/// Shown under a host that refused the password just typed.
pub const PASSWORD_WRONG: &str = "Password errata: riprova.";
/// The hint of the field while a password is asked.
pub const PASSWORD_HINT: &str = "Password";
pub const NOT_AUTHORIZED: &str = "Questo computer accetta solo i dispositivi già autorizzati: \
     connettiti una volta con il PIN prima di attivare l'accesso non presidiato.";

// --- Settings -------------------------------------------------------------

pub const UNATTENDED_TITLE: &str = "Accesso non presidiato";
pub const UNATTENDED_HINT: &str = "Usa questo PC anche senza nessuno davanti: schermata di \
     blocco, accesso, richieste UAC e Ctrl+Alt+Canc. Si connettono solo i dispositivi già \
     autorizzati. Serve l'autorizzazione di amministratore.";
pub const UNATTENDED_ON: &str = "Attivo: la condivisione di questo PC è gestita dal servizio.";
pub const UNATTENDED_OFF: &str = "Non attivo.";
pub const UNATTENDED_PASSWORD_LABEL: &str = "Password (facoltativa)";
pub const UNATTENDED_PASSWORD_HINT: &str =
    "Se impostata, va inserita anche dai dispositivi autorizzati.";
pub const UNATTENDED_ENABLE: &str = "Attiva…";
pub const UNATTENDED_UPDATE: &str = "Aggiorna…";
pub const UNATTENDED_UPDATE_HINT: &str = "«Aggiorna» copia di nuovo i dispositivi autorizzati, \
     cambia la password e ricopia le impostazioni usate dal servizio: porta, monitor, \
     fotogrammi, bitrate, codec, appunti e audio.";
/// The window's settings differ from those copied to the service.
pub const UNATTENDED_UPDATE_NEEDED: &str = "Hai cambiato impostazioni usate dall'accesso non \
     presidiato: premi «Aggiorna…» per applicarle al servizio.";
/// Under the sharing settings while the service is installed.
pub const SETTINGS_APPLIED_BY_UPDATE: &str = "Con l'accesso non presidiato attivo, monitor, \
     fotogrammi, bitrate, codec, porta, appunti e audio passano al servizio solo con \
     «Aggiorna…» (in fondo alla pagina).";
/// Next to the settings the service ignores, disabled while it is installed.
pub const SETTINGS_NOT_FOR_UNATTENDED: &str = "Non valgono con l'accesso non presidiato attivo: \
     il servizio consente sempre il controllo remoto ai soli dispositivi autorizzati e non \
     riceve file.";
pub const UNATTENDED_DISABLE: &str = "Disattiva…";
pub const UNATTENDED_WORKING: &str = "In corso: conferma la richiesta di Windows…";
pub const UNATTENDED_ENABLED: &str = "Accesso non presidiato attivato.";
pub const UNATTENDED_DISABLED: &str = "Accesso non presidiato disattivato.";
pub const UNATTENDED_CANCELLED: &str = "Operazione annullata.";

/// Why turning unattended access on or off failed.
pub fn unattended_failed(detail: &str) -> String {
    format!("Operazione non riuscita: {detail}")
}

/// The elevated command failed; its own message stayed in its process.
pub fn unattended_exit_code(code: u32) -> String {
    format!(
        "wshare è terminato con il codice {code}. Per i dettagli esegui «wshare service \
         install» da un terminale aperto come amministratore."
    )
}

/// Why the state of unattended access cannot be read.
pub fn unattended_unknown(detail: &str) -> String {
    format!("Impossibile sapere se l'accesso non presidiato è attivo ({detail}).")
}

// --- Sharing ----------------------------------------------------------------

/// The window does not share this PC itself while the service does.
pub const SHARING_BY_SERVICE: &str = "La condivisione di questo PC è gestita dall'accesso non \
     presidiato (Impostazioni): non serve attivarla qui.";

// --- Indicator on the host -----------------------------------------------

/// The always-visible bar on the host during an unattended session.
pub fn unattended_indicator(viewer: &str) -> String {
    format!("Accesso non presidiato: {viewer} controlla questo PC")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_indicator_names_the_viewer() {
        let text = unattended_indicator("PORTATILE");
        assert!(text.contains("PORTATILE"), "{text}");
        assert!(text.starts_with("Accesso non presidiato"), "{text}");
    }

    #[test]
    fn failures_carry_their_detail() {
        assert!(unattended_failed("negato").ends_with("negato"));
        assert!(unattended_unknown("x").contains("(x)"));
    }
}
