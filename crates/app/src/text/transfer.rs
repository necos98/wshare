//! The Italian texts of file transfers: the viewer's "File" menu and
//! progress line, the desktop interface's rows and settings, the
//! notifications. A part of [`super`], re-exported there.

use std::path::Path;

use protocol::transfer::TransferError;

use crate::transfer::{ActiveTransfer, Direction, Ended, Failure, Outcome, Summary};

const KILOBYTE: f64 = 1e3;
const MEGABYTE: f64 = 1e6;
const GIGABYTE: f64 = 1e9;

// --- Viewer window ------------------------------------------------------

pub const BAR_FILES: &str = "File";
pub const BAR_TIP_FILES: &str = "Invia file all'altro computer o annulla i trasferimenti";
pub const FILES_SEND: &str = "Invia file…";
pub const FILES_CANCEL: &str = "Annulla trasferimenti";
pub const FILES_OPEN_RECEIVED: &str = "Apri cartella dei file ricevuti";
pub const FILES_PICK_TITLE: &str = "Scegli i file da inviare";

// --- Desktop interface --------------------------------------------------

pub const SEND_TO_VIEWER: &str = "Invia file al visualizzatore…";
pub const SEND_FROM_VIEWER: &str = "Invia file…";
pub const CANCEL_TRANSFER: &str = "Annulla";
pub const OPEN_FOLDER: &str = "Apri cartella";
pub const SETTINGS_FILES: &str = "File";
pub const SETTING_RECEIVE_FILES: &str = "Consenti ricezione file";
pub const SETTING_RECEIVE_FILES_HINT: &str = "File trascinati, inviati o incollati dal \
     visualizzatore (serve il controllo remoto; per i file incollati anche la \
     condivisione degli appunti)";
pub const SETTING_DOWNLOADS: &str = "Cartella dei file ricevuti";
pub const SETTING_DOWNLOADS_HINT: &str = "Vuota: Download\\wshare";
pub const SETTING_DOWNLOADS_PICK: &str = "Sfoglia…";
pub const DOWNLOADS_PICK_TITLE: &str = "Scegli dove salvare i file ricevuti";
pub const NO_VIEWER_TO_SEND_TO: &str = "Nessun visualizzatore connesso a cui inviare i file.";

pub const FOLDER_NOT_LOCAL: &str = "non è una cartella di questo computer";

pub fn folder_not_opened(detail: &str) -> String {
    format!("Impossibile aprire la cartella: {detail}")
}

pub fn viewer_not_reached(detail: &str) -> String {
    format!("Impossibile raggiungere la finestra del visualizzatore: {detail}")
}

/// The body of the notification of a transfer that ended; `viewer` names
/// the other PC when known.
pub fn transfer_notification(ended: &Ended, viewer: Option<&str>) -> String {
    let mut body = transfer_ended(ended);
    if let Some(viewer) = viewer {
        body.push_str(&format!(" ({viewer})"));
    }
    if let Outcome::Completed {
        folder: Some(folder),
    } = &ended.outcome
    {
        body.push_str(". ");
        body.push_str(&received_in(folder));
    }
    body
}

/// One row of the transfers list: what, which way, how far.
pub fn transfer_row(transfer: &ActiveTransfer) -> String {
    let info = &transfer.info;
    let way = match info.direction {
        Direction::Sending => "↑",
        Direction::Receiving => "↓",
    };
    format!(
        "{way} {} — {}% ({} di {})",
        info.name,
        transfer.percent(),
        size_label(transfer.done_bytes),
        size_label(info.total_bytes)
    )
}

/// A size for people: `512 byte`, `12,3 kB`, `4,5 MB`, `1,2 GB`.
pub fn size_label(bytes: u64) -> String {
    let value = bytes as f64;
    let (amount, unit) = if value >= GIGABYTE {
        (value / GIGABYTE, "GB")
    } else if value >= MEGABYTE {
        (value / MEGABYTE, "MB")
    } else if value >= KILOBYTE {
        (value / KILOBYTE, "kB")
    } else {
        return format!("{bytes} byte");
    };
    format!("{amount:.1} {unit}").replace('.', ",")
}

/// The line that shows the transfers in progress.
pub fn transfer_summary(summary: &Summary) -> String {
    let what = match (&summary.name, summary.direction) {
        (Some(name), Some(Direction::Receiving)) => format!("Ricezione di «{name}»"),
        (Some(name), _) => format!("Invio di «{name}»"),
        (None, _) => format!("{} trasferimenti", summary.transfers),
    };
    format!(
        "{what}: {}% ({} di {})",
        summary.percent(),
        size_label(summary.done_bytes),
        size_label(summary.total_bytes)
    )
}

/// What a transfer that ended tells the user.
pub fn transfer_ended(ended: &Ended) -> String {
    let name = &ended.info.name;
    match &ended.outcome {
        Outcome::Completed { .. } => match ended.info.direction {
            Direction::Receiving => format!("Ricevuto «{name}»"),
            Direction::Sending => format!("Inviato «{name}»"),
        },
        Outcome::Cancelled { by_peer: false } => format!("Trasferimento di «{name}» annullato"),
        Outcome::Cancelled { by_peer: true } => {
            format!("Trasferimento di «{name}» annullato dall'altro computer")
        }
        Outcome::Refused(reason) => refusal(reason),
        Outcome::Failed(reason) => {
            format!(
                "Trasferimento di «{name}» non riuscito: {}",
                failure(reason)
            )
        }
        Outcome::Interrupted => {
            format!("Trasferimento di «{name}» interrotto: la connessione è terminata")
        }
    }
}

fn refusal(reason: &TransferError) -> String {
    match reason {
        TransferError::NotAllowed => {
            "L'altro computer non accetta file, o il controllo remoto non è consentito".into()
        }
        TransferError::Invalid => "L'altro computer ha rifiutato i file".into(),
        TransferError::Unavailable => "I file non sono più disponibili".into(),
        known => format!("Trasferimento non riuscito: {}", cause(known)),
    }
}

/// Why a transfer failed, as the end of a sentence.
fn failure(failure: &Failure) -> String {
    match failure {
        Failure::Cause(reason) => cause(reason),
        Failure::NoLongerAllowed => "non più consentito su questo computer".into(),
        Failure::Detail(detail) => detail.clone(),
    }
}

fn cause(reason: &TransferError) -> String {
    match reason {
        TransferError::NotAllowed => "non consentito dall'altro computer".into(),
        TransferError::Invalid => "i file sono stati rifiutati perché non validi".into(),
        TransferError::Unavailable => "i file non sono più disponibili".into(),
        TransferError::TooManyTransfers => "troppi trasferimenti contemporanei".into(),
        TransferError::NoFreeName => "nessun nome libero per i file".into(),
        TransferError::CannotCreateFolders(detail) => {
            format!("impossibile creare le cartelle ({detail})")
        }
        TransferError::PlanningFailed(detail) => {
            format!("preparazione dei file interrotta ({detail})")
        }
        TransferError::Failed(detail) => detail.clone(),
    }
}

/// Where received files are, for the notification.
pub fn received_in(folder: &Path) -> String {
    format!("Salvati in {}", folder.display())
}

#[cfg(test)]
mod tests {
    use protocol::transfer::TransferId;

    use super::*;
    use crate::transfer::{Kind, TransferInfo};

    #[test]
    fn sizes_read_as_people_say_them() {
        assert_eq!(size_label(0), "0 byte");
        assert_eq!(size_label(999), "999 byte");
        assert_eq!(size_label(12_345), "12,3 kB");
        assert_eq!(size_label(4_500_000), "4,5 MB");
        assert_eq!(size_label(1_000_000_000), "1,0 GB");
    }

    #[test]
    fn the_summary_names_one_transfer_or_counts_several() {
        let one = Summary {
            transfers: 1,
            name: Some("foto.zip".into()),
            files: 1,
            direction: Some(Direction::Receiving),
            done_bytes: 500_000,
            total_bytes: 2_000_000,
        };
        assert_eq!(
            transfer_summary(&one),
            "Ricezione di «foto.zip»: 25% (500,0 kB di 2,0 MB)"
        );
        let several = Summary {
            transfers: 3,
            name: None,
            direction: None,
            ..one
        };
        assert!(transfer_summary(&several).starts_with("3 trasferimenti: 25%"));
    }

    #[test]
    fn every_end_has_its_words() {
        let info = TransferInfo {
            id: TransferId(1),
            direction: Direction::Receiving,
            kind: Kind::Push,
            name: "Foto".into(),
            files: 2,
            total_bytes: 9,
        };
        let ended = |outcome| {
            transfer_ended(&Ended {
                info: info.clone(),
                outcome,
            })
        };
        assert_eq!(
            ended(Outcome::Completed { folder: None }),
            "Ricevuto «Foto»"
        );
        assert!(ended(Outcome::Cancelled { by_peer: true }).contains("dall'altro computer"));
        assert!(ended(Outcome::Refused(TransferError::NotAllowed)).contains("non accetta"));
        assert!(
            ended(Outcome::Failed(Failure::Detail("disco pieno".into()))).ends_with("disco pieno")
        );
        assert!(ended(Outcome::Interrupted).contains("interrotto"));
    }

    #[test]
    fn a_transfer_that_failed_says_why_in_italian() {
        let info = TransferInfo {
            id: TransferId(1),
            direction: Direction::Receiving,
            kind: Kind::Push,
            name: "Foto".into(),
            files: 2,
            total_bytes: 9,
        };
        let ended = |outcome| {
            transfer_ended(&Ended {
                info: info.clone(),
                outcome,
            })
        };
        let failed = |failure| ended(Outcome::Failed(failure));
        let cause = |reason| failed(Failure::Cause(reason));
        for (text, expected) in [
            (
                cause(TransferError::NotAllowed),
                "Trasferimento di «Foto» non riuscito: non consentito dall'altro computer",
            ),
            (
                cause(TransferError::Invalid),
                "Trasferimento di «Foto» non riuscito: i file sono stati rifiutati perché non validi",
            ),
            (
                cause(TransferError::Unavailable),
                "Trasferimento di «Foto» non riuscito: i file non sono più disponibili",
            ),
            (
                cause(TransferError::TooManyTransfers),
                "Trasferimento di «Foto» non riuscito: troppi trasferimenti contemporanei",
            ),
            (
                cause(TransferError::NoFreeName),
                "Trasferimento di «Foto» non riuscito: nessun nome libero per i file",
            ),
            (
                cause(TransferError::CannotCreateFolders("accesso negato".into())),
                "Trasferimento di «Foto» non riuscito: impossibile creare le cartelle (accesso negato)",
            ),
            (
                cause(TransferError::PlanningFailed("panic".into())),
                "Trasferimento di «Foto» non riuscito: preparazione dei file interrotta (panic)",
            ),
            (
                failed(Failure::NoLongerAllowed),
                "Trasferimento di «Foto» non riuscito: non più consentito su questo computer",
            ),
        ] {
            assert_eq!(text, expected);
        }
    }

    #[test]
    fn a_refusal_for_a_known_cause_is_italian_too() {
        let refused = |reason| refusal(&reason);
        assert_eq!(
            refused(TransferError::TooManyTransfers),
            "Trasferimento non riuscito: troppi trasferimenti contemporanei"
        );
        assert_eq!(
            refused(TransferError::NoFreeName),
            "Trasferimento non riuscito: nessun nome libero per i file"
        );
    }
}
