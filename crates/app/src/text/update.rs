//! The Italian texts of updates: the «Aggiornamenti» section of the
//! settings page and the line on the main page that points to it. A part
//! of [`super`], re-exported there.

use crate::update::UpdateFailure;

pub const UPDATE_TITLE: &str = "Aggiornamenti";
pub const UPDATE_CHECK_ON_START: &str = "Controlla gli aggiornamenti all'avvio";
pub const UPDATE_CHECK: &str = "Controlla ora";
pub const UPDATE_CHECKING: &str = "Controllo degli aggiornamenti…";
pub const UPDATE_UP_TO_DATE: &str = "wshare è aggiornato.";
pub const UPDATE_INSTALL: &str = "Aggiorna e riavvia";
pub const UPDATE_INSTALL_HINT: &str = "Scarica la nuova versione da GitHub, ne verifica la firma, \
     sostituisce wshare.exe e wshare-cli.exe e riapre wshare. La condivisione in corso si \
     interrompe per qualche secondo.";
pub const UPDATE_INSTALLING: &str = "Download e installazione…";
/// The unattended-access service still runs the copy it was given.
pub const UPDATE_SERVICE_OUTDATED: &str = "L'accesso non presidiato usa ancora un'altra versione \
     di wshare: premi «Aggiorna…» in «Accesso non presidiato» per copiarvi questa.";
/// On the main page, under the title: opens the settings.
pub const UPDATE_BADGE_TIP: &str = "Apri le impostazioni per aggiornare";

pub fn update_current(version: &str) -> String {
    format!("Versione installata: {version}")
}

pub fn update_available(version: &str) -> String {
    format!("È disponibile la versione {version}.")
}

/// The line on the main page.
pub fn update_badge(version: &str) -> String {
    format!("Aggiornamento disponibile: {version}")
}

pub fn update_failure(failure: &UpdateFailure) -> String {
    match failure {
        UpdateFailure::Download(detail) => {
            format!("Impossibile scaricare l'aggiornamento: sei connesso a Internet? ({detail})")
        }
        UpdateFailure::BadSignature => "L'aggiornamento trovato non è firmato da wshare ed è \
             stato ignorato."
            .to_string(),
        UpdateFailure::BadManifest(detail) => {
            format!("L'aggiornamento trovato non è utilizzabile: {detail}")
        }
        UpdateFailure::Corrupted(name) => format!(
            "Il file {name} scaricato non corrisponde alla versione firmata: riprova più tardi."
        ),
        UpdateFailure::FolderNotWritable(dir) => format!(
            "wshare non può scrivere nella sua cartella ({}): spostalo in una cartella tua, \
             oppure scarica la nuova versione da GitHub.",
            dir.display()
        ),
        UpdateFailure::Install(detail) => {
            format!("Impossibile sostituire wshare: {detail}. La versione attuale è rimasta.")
        }
    }
}

pub fn update_restart_failed(detail: &str) -> String {
    format!("Aggiornamento installato, ma wshare non è ripartito ({detail}): chiudilo e riaprilo.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_failure_is_explained() {
        let failures = [
            UpdateFailure::Download("timeout".into()),
            UpdateFailure::BadSignature,
            UpdateFailure::BadManifest("x".into()),
            UpdateFailure::Corrupted("wshare.exe".into()),
            UpdateFailure::FolderNotWritable(r"C:\Program Files\wshare".into()),
            UpdateFailure::Install("in uso".into()),
        ];
        for failure in failures {
            assert!(!update_failure(&failure).is_empty());
        }
        assert!(
            update_failure(&UpdateFailure::FolderNotWritable(r"C:\x".into())).contains(r"C:\x")
        );
    }
}
