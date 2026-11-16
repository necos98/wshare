//! The Italian words of why a transfer failed: the causes the two PCs
//! know, and why the files picked here could not even be proposed. Only
//! the technical detail of a system error (in parentheses) is not ours.

use protocol::transfer::{MAX_PATH_UNITS, MAX_TRANSFER_ENTRIES, TransferError};

use crate::transfer::Failure;
use crate::transfer::listing::ListingError;
use crate::transfer::safe_path::PathError;

/// Why a transfer failed, as the end of a sentence.
pub(super) fn failure(failure: &Failure) -> String {
    match failure {
        Failure::Cause(reason) => cause(reason),
        Failure::NoLongerAllowed => "non più consentito su questo computer".into(),
        Failure::Listing(error) => listing_error(error),
    }
}

/// A cause of this PC or of the other one, as the end of a sentence.
pub(super) fn cause(reason: &TransferError) -> String {
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
        TransferError::StreamFailed(detail) => {
            format!("la connessione di un file si è interrotta ({detail})")
        }
        TransferError::CannotReadFile(detail) => format!("impossibile leggere un file ({detail})"),
        TransferError::FileShrank => "un file si è accorciato dopo essere stato scelto".into(),
        TransferError::CannotCreateFile(detail) => {
            format!("impossibile creare un file ({detail})")
        }
        TransferError::CannotWriteFile(detail) => {
            format!("impossibile scrivere un file ({detail})")
        }
        TransferError::CannotSaveFile(detail) => format!("impossibile salvare un file ({detail})"),
        TransferError::TooMuchData => "un file contiene più dati di quelli annunciati".into(),
        TransferError::Incomplete => "un file è arrivato incompleto".into(),
        TransferError::NotInList => "è arrivato un file che non è nell'elenco".into(),
        TransferError::NotSent => "l'altro computer non ha inviato il file".into(),
        TransferError::NotAsked => "il file arrivato non è quello richiesto".into(),
    }
}

/// Why the files picked or copied here cannot be sent, as the end of a
/// sentence.
pub fn listing_error(error: &ListingError) -> String {
    match error {
        ListingError::Nothing => "nessun file da inviare".into(),
        ListingError::TooMany => format!("più di {MAX_TRANSFER_ENTRIES} file e cartelle"),
        ListingError::Unsendable { path, reason } => {
            format!("«{}»: {}", path.display(), path_error(reason))
        }
        ListingError::Unreadable { path, detail } => {
            format!("impossibile leggere «{}» ({detail})", path.display())
        }
        ListingError::Unnamed(path) => {
            format!("«{}» non è un file né una cartella", path.display())
        }
    }
}

/// Why a path cannot be created on the other PC.
fn path_error(error: &PathError) -> String {
    match error {
        PathError::Empty => "percorso vuoto".into(),
        PathError::TooLong => format!("percorso più lungo di {MAX_PATH_UNITS} caratteri"),
        PathError::NotRelative => "percorso non valido".into(),
        PathError::BadName(name) => format!("nome non consentito sull'altro computer («{name}»)"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn a_file_that_failed_on_its_way_says_why_in_italian() {
        let detail = |cause: fn(String) -> TransferError| cause("accesso negato".into());
        for (reason, expected) in [
            (
                detail(TransferError::StreamFailed),
                "la connessione di un file si è interrotta (accesso negato)",
            ),
            (
                detail(TransferError::CannotReadFile),
                "impossibile leggere un file (accesso negato)",
            ),
            (
                TransferError::FileShrank,
                "un file si è accorciato dopo essere stato scelto",
            ),
            (
                detail(TransferError::CannotCreateFile),
                "impossibile creare un file (accesso negato)",
            ),
            (
                detail(TransferError::CannotWriteFile),
                "impossibile scrivere un file (accesso negato)",
            ),
            (
                detail(TransferError::CannotSaveFile),
                "impossibile salvare un file (accesso negato)",
            ),
            (
                TransferError::TooMuchData,
                "un file contiene più dati di quelli annunciati",
            ),
            (TransferError::Incomplete, "un file è arrivato incompleto"),
            (
                TransferError::NotInList,
                "è arrivato un file che non è nell'elenco",
            ),
            (
                TransferError::NotSent,
                "l'altro computer non ha inviato il file",
            ),
            (
                TransferError::NotAsked,
                "il file arrivato non è quello richiesto",
            ),
        ] {
            assert_eq!(failure(&Failure::Cause(reason)), expected);
        }
    }

    #[test]
    fn files_that_cannot_be_listed_say_why_in_italian() {
        let path = PathBuf::from("C:\\Foto\\a.jpg");
        let unsendable = |reason| ListingError::Unsendable {
            path: path.clone(),
            reason,
        };
        for (error, expected) in [
            (ListingError::Nothing, "nessun file da inviare".to_string()),
            (
                ListingError::TooMany,
                format!("più di {MAX_TRANSFER_ENTRIES} file e cartelle"),
            ),
            (
                unsendable(PathError::Empty),
                "«C:\\Foto\\a.jpg»: percorso vuoto".into(),
            ),
            (
                unsendable(PathError::TooLong),
                format!("«C:\\Foto\\a.jpg»: percorso più lungo di {MAX_PATH_UNITS} caratteri"),
            ),
            (
                unsendable(PathError::NotRelative),
                "«C:\\Foto\\a.jpg»: percorso non valido".into(),
            ),
            (
                unsendable(PathError::BadName("a?.jpg".into())),
                "«C:\\Foto\\a.jpg»: nome non consentito sull'altro computer («a?.jpg»)".into(),
            ),
            (
                ListingError::Unreadable {
                    path: path.clone(),
                    detail: "accesso negato".into(),
                },
                "impossibile leggere «C:\\Foto\\a.jpg» (accesso negato)".into(),
            ),
            (
                ListingError::Unnamed(PathBuf::from("C:\\")),
                "«C:\\» non è un file né una cartella".into(),
            ),
        ] {
            assert_eq!(listing_error(&error), expected);
            assert_eq!(failure(&Failure::Listing(error)), expected);
        }
    }
}
