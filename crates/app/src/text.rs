//! Every Italian text the user reads in the interface, the tray icon and
//! the viewer window, in one place. Causes are typed where they happen
//! (see `viewer::report` and `host::cause`); this module only turns them
//! into words, so rewording an error cannot change what the program does.
//! The terminal output of the CLI stays English and lives next to the code
//! that prints it.

use protocol::control::RejectReason;
use protocol::session::{MonitorEntry, Quality};

use crate::host::{HostFailure, HostWarning, SessionFailure};
use crate::viewer::report::FailureCause;

// --- Why a viewer ended -------------------------------------------------

/// What a viewer that failed for `cause` means for the user.
pub fn viewer_failure(cause: &FailureCause) -> String {
    match cause {
        FailureCause::PinRequired => "Questo computer richiede il PIN.".to_string(),
        FailureCause::WrongPin => "PIN errato.".to_string(),
        FailureCause::UnsupportedVersion => format!(
            "Il computer ha rifiutato la connessione: {}.",
            VERSION_MISMATCH
        ),
        FailureCause::HostChanged => {
            "L'identità del computer è cambiata: potrebbe essere un altro computer. Se è stato \
             reinstallato, usa «Dimentica» e connettiti di nuovo con il PIN."
                .to_string()
        }
        FailureCause::Unreachable => {
            "Impossibile raggiungere il computer: è acceso e raggiungibile?".to_string()
        }
        FailureCause::TimedOut => {
            "Il computer non risponde: è acceso e raggiungibile? Controlla il firewall.".to_string()
        }
        FailureCause::HostLeft => "Il computer ha interrotto la condivisione.".to_string(),
        FailureCause::HostDisconnected => "Disconnesso dall'altro computer.".to_string(),
        FailureCause::ConnectionLost => "Connessione persa.".to_string(),
        FailureCause::HostBusy => {
            "Computer già in uso da un altro visualizzatore: riprova quando si libera.".to_string()
        }
        FailureCause::TooManyAttempts => {
            "Troppi PIN errati: attendi un minuto e riprova.".to_string()
        }
        FailureCause::HostCannotStream(detail) => format!(
            "Il computer ha rifiutato la connessione: {}.",
            cannot_stream(detail)
        ),
        FailureCause::Other(detail) => format!("Errore: {detail}"),
    }
}

/// A viewer that ended with failure without saying why (killed, crashed).
pub fn viewer_exited_unexpectedly(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("Il visualizzatore si è chiuso inaspettatamente (codice {code})."),
        None => "Il visualizzatore si è chiuso inaspettatamente.".to_string(),
    }
}

pub fn viewer_start_failed(detail: &str) -> String {
    format!("impossibile avviare il visualizzatore ({detail})")
}

// --- What the host reports ----------------------------------------------

const VERSION_MISMATCH: &str = "versione di wshare diversa, aggiorna entrambi i computer";

fn cannot_stream(detail: &str) -> String {
    format!("impossibile trasmettere lo schermo ({detail})")
}

/// Why the host refused a viewer, for the host's own notice line.
pub fn reject_reason(reason: &RejectReason) -> String {
    match reason {
        RejectReason::PinRequired => "PIN non inserito".to_string(),
        RejectReason::WrongPin => "PIN errato".to_string(),
        RejectReason::UnsupportedVersion { .. } => VERSION_MISMATCH.to_string(),
        RejectReason::CannotStream(detail) => cannot_stream(detail),
        RejectReason::Busy => "computer già in uso da un altro visualizzatore".to_string(),
        RejectReason::TooManyAttempts => "troppi PIN errati, accesso sospeso".to_string(),
        RejectReason::Other(text) => text.clone(),
    }
}

pub fn viewer_rejected(ip: impl std::fmt::Display, reason: &RejectReason) -> String {
    format!("Connessione rifiutata da {ip}: {}", reject_reason(reason))
}

pub fn session_ended(failure: &SessionFailure) -> String {
    format!("Sessione terminata: {failure}")
}

/// Why sharing could not start or stopped on its own.
pub fn sharing_failed(failure: &HostFailure) -> String {
    match failure {
        HostFailure::PortInUse { port } => format!(
            "Impossibile condividere: la porta {port} è già in uso (un altro wshare è aperto?)."
        ),
        HostFailure::MonitorNotFound { .. } => {
            "Impossibile condividere: monitor non trovato.".to_string()
        }
        HostFailure::Crashed(detail) => {
            format!("Il servizio di condivisione si è bloccato: {detail}")
        }
        HostFailure::Failed(detail) => format!("Impossibile condividere: {detail}"),
    }
}

pub fn host_warning(warning: &HostWarning) -> String {
    match warning {
        HostWarning::LanAnnouncementFailed(detail) => format!(
            "Questo PC non compare nell'elenco degli altri computer: si può raggiungere solo \
             con l'indirizzo IP ({detail})."
        ),
        HostWarning::AuthorizedDevicesUnreadable(detail) => format!(
            "Impossibile leggere i dispositivi autorizzati: serve di nuovo il PIN ({detail})."
        ),
        HostWarning::DeviceNotRemembered { name, detail } => {
            format!("Impossibile ricordare «{name}»: {detail}")
        }
        HostWarning::DeviceRemovalNotSaved(detail) => {
            format!("Impossibile salvare la rimozione del dispositivo: {detail}")
        }
        HostWarning::PinRotatedAfterWrongAttempts => {
            "PIN cambiato dopo troppi tentativi errati.".to_string()
        }
        HostWarning::InputFailed {
            detail,
            suppressed: 0,
        } => format!("Controllo remoto non riuscito: {detail}"),
        HostWarning::InputFailed { detail, suppressed } => {
            format!(
                "Controllo remoto non riuscito: {detail} ({suppressed} errori simili non mostrati)"
            )
        }
        HostWarning::BitrateFixed(detail) => format!(
            "L'encoder non può adattare il bitrate alla rete: resta fisso per questa \
             sessione ({detail})."
        ),
        HostWarning::ClipboardUnavailable(detail) => {
            format!("Impossibile condividere gli appunti in questa sessione ({detail}).")
        }
        HostWarning::ClipboardTooLarge => format!(
            "Il testo copiato è troppo lungo per essere condiviso (oltre {} KiB).",
            protocol::control::MAX_CLIPBOARD_BYTES / 1024
        ),
        HostWarning::ClipboardFailed(detail) => {
            format!("Condivisione degli appunti non riuscita: {detail}")
        }
        HostWarning::ScreenUnavailable(detail) => format!(
            "Impossibile condividere lo schermo scelto dal visualizzatore: resta quello di \
             prima ({detail})."
        ),
    }
}

// --- State lines of the interface ---------------------------------------

/// Shown under a remembered host that refused to connect without a PIN.
pub const PIN_AGAIN: &str = "Questo computer richiede di nuovo il PIN.";
/// Shown under a listed host that refused the PIN just typed.
pub const PIN_WRONG: &str = "PIN errato: riprova.";
pub const SHARING_STOPPING: &str = "Arresto in corso…";
pub const SHARING_OFF_HINT: &str =
    "Attivalo per far vedere e controllare questo PC da un altro computer in rete.";
pub const SHARING_STARTING: &str = "Avvio in corso…";
pub const SHARING_WAITING: &str = "In attesa di connessioni…";

pub fn sharing_connected(name: &str, ip: impl std::fmt::Display) -> String {
    format!("Connesso: {name} ({ip})")
}

/// The monitor being shared; `device_name` as the capture names it.
pub fn shared_screen(device_name: &str, width: u32, height: u32) -> String {
    format!(
        "Schermo condiviso: {} ({width}×{height})",
        capture::display_name(device_name)
    )
}

pub const HOST_INCOMPATIBLE: &str = "Versione di wshare diversa: non compatibile";
pub const HOST_REMEMBERED: &str = "Accesso memorizzato";

pub fn host_details(
    resolution: &str,
    address: Option<std::net::SocketAddr>,
    remembered: bool,
) -> String {
    let line = match address {
        Some(address) => format!("{resolution} · {address}"),
        None => format!("{resolution} · nessun indirizzo raggiungibile"),
    };
    if remembered {
        format!("{line} · {HOST_REMEMBERED}")
    } else {
        line
    }
}

pub fn viewer_connecting(label: &str) -> String {
    format!("Connessione a {label} in corso…")
}

pub fn viewer_connected(label: &str) -> String {
    format!("Connesso a {label}")
}

pub fn viewer_failed(label: &str, message: &str) -> String {
    format!("{label}: {message}")
}

// --- Tray icon ----------------------------------------------------------

pub const TRAY_OPEN: &str = "Apri wshare";
pub const TRAY_SHARING: &str = "Condivisione attiva";
pub const TRAY_NEW_PIN: &str = "Nuovo PIN";
pub const TRAY_ALLOW_INPUT: &str = "Consenti controllo remoto";
pub const TRAY_QUIT: &str = "Esci";

pub fn tray_disconnect(name: &str) -> String {
    format!("Disconnetti {name}")
}

pub fn tray_connected(name: &str) -> String {
    format!("wshare - connesso: {name}")
}

pub fn tray_waiting(pin: &str) -> String {
    format!("wshare - in attesa (PIN {pin})")
}

pub const TRAY_STARTING: &str = "wshare - avvio condivisione…";
pub const TRAY_STOPPING: &str = "wshare - arresto condivisione…";
pub const TRAY_OFF: &str = "wshare - condivisione disattivata";

// --- Requests of the user -----------------------------------------------

pub fn discovery_unavailable(detail: &str) -> String {
    format!("Ricerca in rete non disponibile: {detail}")
}

pub const HOST_GONE: &str = "Il computer non è più in rete.";
pub const PIN_MISSING: &str = "Inserisci il PIN mostrato sull'altro computer.";
pub const HOST_VERSION_DIFFERS: &str = "Versione di wshare diversa: aggiorna entrambi i computer.";
pub const HOST_NO_ADDRESS: &str = "Nessun indirizzo raggiungibile.";
pub const ADDRESS_INVALID: &str = "Indirizzo non valido: scrivi un IP, ad esempio 192.168.1.20";
pub const VIEWER_ALREADY_OPEN: &str = "Una finestra per questo computer è già aperta.";

pub fn forget_failed(detail: &str) -> String {
    format!("Impossibile dimenticare il computer: {detail}")
}

// --- Labels of the window -----------------------------------------------

pub const SHARE_THIS_SCREEN: &str = "Condividi questo schermo";
pub const NEW_PIN: &str = "Nuovo PIN";
pub const DISCONNECT: &str = "Disconnetti";
pub const ALLOW_REMOTE_CONTROL: &str = "Consenti controllo remoto (mouse e tastiera)";
pub const INPUT_BLOCKED: &str =
    "Input bloccato da Windows (finestra amministratore o schermata protetta).";
pub const COMPUTERS_ON_NETWORK: &str = "Computer in rete";
pub const SEARCHING: &str = "Ricerca in corso… Sull'altro computer apri wshare e attiva la \
     condivisione. Se non compare, controlla che il firewall lasci passare mDNS (UDP 5353) su \
     entrambi i computer, oppure usa «Connetti a un indirizzo IP» qui sotto.";
pub const CONNECT_TO_ADDRESS: &str = "Connetti a un indirizzo IP";
pub const CONNECT: &str = "Connetti";
pub const CLOSE: &str = "Chiudi";
pub const CANCEL: &str = "Annulla";
pub const AUTHORIZED_DEVICES: &str = "Dispositivi autorizzati";
pub const AUTHORIZED_HINT: &str =
    "Si connettono senza PIN. Rimuovendoli, la prossima volta servirà il PIN.";
pub const REMOVE: &str = "Rimuovi";
pub const FORGET: &str = "Dimentica";
pub const FORGET_HINT: &str = "La prossima volta servirà di nuovo il PIN.";

pub fn remembered_unavailable(detail: &str) -> String {
    format!("Computer memorizzati non disponibili: {detail}")
}

pub fn last_access(when: &str) -> String {
    format!("Ultimo accesso: {when}")
}

// --- Relative times -----------------------------------------------------

pub const AGO_NOW: &str = "adesso";
pub const AGO_YESTERDAY: &str = "ieri";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgoUnit {
    Minutes,
    Hours,
    Days,
}

/// "5 minuti fa", "1 ora fa".
pub fn ago_count(n: u64, unit: AgoUnit) -> String {
    let (one, many) = match unit {
        AgoUnit::Minutes => ("minuto", "minuti"),
        AgoUnit::Hours => ("ora", "ore"),
        AgoUnit::Days => ("giorno", "giorni"),
    };
    format!("{n} {} fa", if n == 1 { one } else { many })
}

// --- Startup ------------------------------------------------------------

pub fn window_failed(detail: &str) -> String {
    format!(
        "wshare non riesce ad aprire la finestra:
{detail}"
    )
}

// --- Viewer window ------------------------------------------------------

pub const BAR_FULLSCREEN: &str = "Schermo intero";
pub const BAR_WINDOWED: &str = "Finestra";
pub const BAR_STATS: &str = "Statistiche";
pub const BAR_DISCONNECT: &str = "Disconnetti";

/// Hover texts of the control bar's buttons, with their shortcuts.
pub const BAR_TIP_FULLSCREEN: &str = "Schermo intero (Ctrl+Alt+Invio)";
pub const BAR_TIP_WINDOWED: &str = "Torna alla finestra (Ctrl+Alt+Invio)";
pub const BAR_TIP_STATS: &str = "Mostra o nascondi le statistiche (Ctrl+Alt+I)";
pub const BAR_TIP_DISCONNECT: &str = "Chiudi la connessione";

pub const BAR_SCREEN: &str = "Schermo";
pub const BAR_QUALITY: &str = "Qualità";
pub const BAR_TIP_SCREEN: &str = "Scegli quale monitor dell'altro computer guardare";
pub const BAR_TIP_QUALITY: &str = "Scegli la qualità del video";

/// A row of the control bar's "Qualità" menu.
pub fn quality_label(quality: Quality) -> &'static str {
    match quality {
        Quality::Automatic => "Automatica",
        Quality::High => "Alta",
        Quality::Medium => "Media",
        Quality::Low => "Bassa",
    }
}

/// A row of the control bar's "Schermo" menu: the host's monitor at
/// `position` (0-based) of its list.
pub fn monitor_entry_label(position: usize, monitor: &MonitorEntry) -> String {
    let main = if monitor.primary { ", principale" } else { "" };
    format!(
        "{}. {} ({}×{}{main})",
        position + 1,
        monitor.name,
        monitor.width,
        monitor.height
    )
}

/// Shown over the picture for a few seconds when the session starts.
pub const NOTICE_SHORTCUTS: [&str; 2] =
    ["Ctrl+Alt+Invio: schermo intero", "Ctrl+Alt+I: statistiche"];
/// Shown while the picture does not move on.
pub const NOTICE_UNSTABLE: &str = "Connessione instabile…";
/// Shown while the viewer re-establishes a connection that dropped.
pub const NOTICE_RECONNECTING: &str = "Riconnessione in corso…";
pub const NOTICE_CONTROL_OFF: &str = "Controllo remoto disattivato dall'altro computer";
pub const NOTICE_CONTROL_ON: &str = "Controllo remoto riattivato dall'altro computer";

pub const STAT_FPS: &str = "Frame/s";
pub const STAT_BITRATE: &str = "Bitrate";
/// The bitrate the host's encoder aims at, adapted to the network.
pub const STAT_TARGET: &str = "Obiettivo";
pub const STAT_RTT: &str = "RTT rete";
pub const STAT_DECODE: &str = "Decodifica";
pub const STAT_ENCODER: &str = "Encoder";
pub const STAT_DECODER: &str = "Decoder";
/// Frames decoded but not shown because a newer one was already there.
pub const STAT_SUPERSEDED: &str = "Non mostrati";
/// Frames saved by resending the datagrams they had lost.
pub const STAT_RECOVERED: &str = "Recuperati";
pub const STATS_HOTKEYS: &str = "Ctrl+Alt+I nascondi - Ctrl+Alt+Invio schermo intero";

pub fn stats_losses(lost: u64, skipped: u64, keyframes: u64) -> String {
    format!("Persi {lost}  Scartati {skipped}  Keyframe richiesti {keyframes}")
}

// --- Settings -----------------------------------------------------------

/// Comment lines at the top of `settings.toml`.
pub const SETTINGS_FILE_HEADER: &str = "Impostazioni di wshare. Si cambiano dalla finestra \
     (ingranaggio); un file illeggibile viene ignorato e si riparte dai valori predefiniti.";

pub const SETTINGS_TITLE: &str = "Impostazioni";
pub const SETTINGS_BACK: &str = "Indietro";
pub const SETTINGS_OPEN_TIP: &str = "Impostazioni";

pub const SETTINGS_SHARING: &str = "Condivisione";
pub const SETTINGS_MONITOR: &str = "Monitor";
pub const SETTINGS_FPS: &str = "Fotogrammi al secondo";
pub const SETTINGS_MAX_MBPS: &str = "Qualità massima (Mbps)";
pub const SETTINGS_MAX_MBPS_HINT: &str =
    "Con la rete congestionata la qualità scende da sola, senza superare questo valore.";
pub const SETTINGS_CODEC: &str = "Codec";
pub const SETTINGS_PORT: &str = "Porta";
pub const SETTINGS_ALLOW_INPUT: &str = "Consenti controllo remoto per impostazione predefinita";
pub const SETTINGS_SHARE_CLIPBOARD: &str =
    "Condividi appunti (il testo copiato passa da un computer all'altro)";

pub const SETTINGS_VIEWING: &str = "Visualizzazione";
pub const SETTINGS_VIEWING_HINT: &str = "Valgono per le finestre aperte da ora in poi.";
pub const SETTINGS_FULLSCREEN: &str = "Apri a schermo intero";
pub const SETTINGS_STATS: &str = "Mostra le statistiche";
pub const SETTINGS_VIEW_ONLY: &str = "Solo visualizzazione (niente mouse e tastiera)";

pub const SETTINGS_STARTUP: &str = "Avvio";
pub const SETTINGS_START_WITH_WINDOWS: &str = "Avvia con Windows";
pub const SETTINGS_START_MINIMIZED: &str = "Avvia ridotto a icona";
pub const SETTINGS_SHARE_ON_START: &str = "Attiva la condivisione all'avvio";

pub const SETTINGS_FEEDBACK: &str = "Avvisi";
pub const SETTINGS_NOTIFICATIONS: &str = "Notifiche di connessione e disconnessione";
pub const SETTINGS_INDICATOR: &str = "Indicatore sempre visibile quando qualcuno è connesso";

pub const SETTINGS_RESTART_NEEDED: &str = "Riavvia la condivisione per applicare.";
pub const SETTINGS_RESTART: &str = "Riavvia la condivisione";
pub const SETTINGS_RESTARTING: &str = "Riavvio della condivisione…";

pub fn monitor_label(index: usize, name: &str, width: u32, height: u32) -> String {
    format!("{} - {name} ({width}×{height})", index + 1)
}

pub fn monitor_missing(index: usize) -> String {
    format!("Monitor {} (non collegato)", index + 1)
}

pub fn settings_unreadable(detail: &str) -> String {
    format!("Impostazioni non leggibili, uso quelle predefinite: {detail}")
}

pub fn settings_not_saved(detail: &str) -> String {
    format!("Impostazioni non salvate: {detail}")
}

pub fn autostart_failed(detail: &str) -> String {
    format!("Impossibile cambiare l'avvio con Windows: {detail}")
}

pub fn monitors_unavailable(detail: &str) -> String {
    format!("Elenco dei monitor non disponibile: {detail}")
}

// --- Notifications and connection indicator -----------------------------

pub const NOTIFY_TITLE: &str = "wshare";

pub fn notify_connected(name: &str) -> String {
    format!("{name} si è connesso")
}

/// `name` is `None` when the viewer that left is not known any more.
pub fn notify_disconnected(name: Option<&str>) -> String {
    match name {
        Some(name) => format!("{name} si è disconnesso"),
        None => "Il visualizzatore si è disconnesso".to_string(),
    }
}

pub fn indicator_connected(name: &str) -> String {
    format!("{name} è connesso")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_viewer_failure_has_its_italian_text() {
        let cases = [
            (
                FailureCause::PinRequired,
                "Questo computer richiede il PIN.",
            ),
            (FailureCause::WrongPin, "PIN errato."),
            (
                FailureCause::UnsupportedVersion,
                "Il computer ha rifiutato la connessione: versione di wshare diversa, aggiorna \
                 entrambi i computer.",
            ),
            (
                FailureCause::HostChanged,
                "L'identità del computer è cambiata: potrebbe essere un altro computer. Se è \
                 stato reinstallato, usa «Dimentica» e connettiti di nuovo con il PIN.",
            ),
            (
                FailureCause::Unreachable,
                "Impossibile raggiungere il computer: è acceso e raggiungibile?",
            ),
            (
                FailureCause::TimedOut,
                "Il computer non risponde: è acceso e raggiungibile? Controlla il firewall.",
            ),
            (
                FailureCause::HostLeft,
                "Il computer ha interrotto la condivisione.",
            ),
            (
                FailureCause::HostDisconnected,
                "Disconnesso dall'altro computer.",
            ),
            (FailureCause::ConnectionLost, "Connessione persa."),
            (
                FailureCause::HostBusy,
                "Computer già in uso da un altro visualizzatore: riprova quando si libera.",
            ),
            (
                FailureCause::TooManyAttempts,
                "Troppi PIN errati: attendi un minuto e riprova.",
            ),
            (
                FailureCause::HostCannotStream("no encoder".into()),
                "Il computer ha rifiutato la connessione: impossibile trasmettere lo schermo \
                 (no encoder).",
            ),
            (FailureCause::Other("boom".into()), "Errore: boom"),
        ];
        for (cause, text) in cases {
            assert_eq!(viewer_failure(&cause), text);
        }
    }

    #[test]
    fn an_empty_list_points_to_the_firewall_mdns_and_the_ip_address() {
        assert!(SEARCHING.contains("firewall"));
        assert!(SEARCHING.contains("mDNS (UDP 5353)"));
        assert!(SEARCHING.contains(&format!("«{CONNECT_TO_ADDRESS}»")));
    }

    #[test]
    fn the_viewer_window_notices_and_hints() {
        assert_eq!(NOTICE_UNSTABLE, "Connessione instabile…");
        assert_eq!(NOTICE_RECONNECTING, "Riconnessione in corso…");
        assert_eq!(
            NOTICE_CONTROL_OFF,
            "Controllo remoto disattivato dall'altro computer"
        );
        assert_eq!(
            NOTICE_CONTROL_ON,
            "Controllo remoto riattivato dall'altro computer"
        );
        assert!(BAR_TIP_FULLSCREEN.contains("Ctrl+Alt+Invio"));
        assert!(BAR_TIP_WINDOWED.contains("Ctrl+Alt+Invio"));
        assert!(BAR_TIP_STATS.contains("Ctrl+Alt+I)"));
    }

    #[test]
    fn the_session_menus_name_monitors_and_qualities() {
        assert_eq!(
            monitor_entry_label(0, &MonitorEntry::new("DISPLAY1", 1920, 1080, true)),
            "1. DISPLAY1 (1920×1080, principale)"
        );
        assert_eq!(
            monitor_entry_label(1, &MonitorEntry::new("DISPLAY2", 2560, 1440, false)),
            "2. DISPLAY2 (2560×1440)"
        );
        let labels: Vec<&str> = Quality::ALL.into_iter().map(quality_label).collect();
        assert_eq!(labels, ["Automatica", "Alta", "Media", "Bassa"]);
    }

    #[test]
    fn an_unexplained_exit_shows_its_code() {
        assert_eq!(
            viewer_exited_unexpectedly(Some(3)),
            "Il visualizzatore si è chiuso inaspettatamente (codice 3)."
        );
        assert_eq!(
            viewer_exited_unexpectedly(None),
            "Il visualizzatore si è chiuso inaspettatamente."
        );
    }

    #[test]
    fn rejection_reasons_for_the_host() {
        assert_eq!(reject_reason(&RejectReason::WrongPin), "PIN errato");
        assert_eq!(
            reject_reason(&RejectReason::PinRequired),
            "PIN non inserito"
        );
        assert_eq!(
            reject_reason(&RejectReason::UnsupportedVersion { host: 4 }),
            "versione di wshare diversa, aggiorna entrambi i computer"
        );
        assert_eq!(
            reject_reason(&RejectReason::CannotStream("no encoder".into())),
            "impossibile trasmettere lo schermo (no encoder)"
        );
        assert_eq!(
            reject_reason(&RejectReason::Busy),
            "computer già in uso da un altro visualizzatore"
        );
        assert_eq!(
            reject_reason(&RejectReason::TooManyAttempts),
            "troppi PIN errati, accesso sospeso"
        );
        assert_eq!(reject_reason(&RejectReason::Other("busy".into())), "busy");
        assert_eq!(
            viewer_rejected("10.0.0.2", &RejectReason::WrongPin),
            "Connessione rifiutata da 10.0.0.2: PIN errato"
        );
    }

    #[test]
    fn every_sharing_failure_has_its_italian_text() {
        assert_eq!(
            sharing_failed(&HostFailure::PortInUse { port: 7420 }),
            "Impossibile condividere: la porta 7420 è già in uso (un altro wshare è aperto?)."
        );
        assert_eq!(
            sharing_failed(&HostFailure::MonitorNotFound { index: 2 }),
            "Impossibile condividere: monitor non trovato."
        );
        assert_eq!(
            sharing_failed(&HostFailure::Crashed("boom".into())),
            "Il servizio di condivisione si è bloccato: boom"
        );
        assert_eq!(
            sharing_failed(&HostFailure::Failed("disk full".into())),
            "Impossibile condividere: disk full"
        );
        assert_eq!(
            session_ended(&SessionFailure("encoder lost".into())),
            "Sessione terminata: encoder lost"
        );
    }

    #[test]
    fn every_host_warning_has_its_italian_text() {
        let cases = [
            (
                HostWarning::LanAnnouncementFailed("no route".into()),
                "Questo PC non compare nell'elenco degli altri computer: si può raggiungere \
                 solo con l'indirizzo IP (no route).",
            ),
            (
                HostWarning::AuthorizedDevicesUnreadable("corrupt".into()),
                "Impossibile leggere i dispositivi autorizzati: serve di nuovo il PIN (corrupt).",
            ),
            (
                HostWarning::DeviceNotRemembered {
                    name: "PORTATILE".into(),
                    detail: "disk full".into(),
                },
                "Impossibile ricordare «PORTATILE»: disk full",
            ),
            (
                HostWarning::DeviceRemovalNotSaved("disk full".into()),
                "Impossibile salvare la rimozione del dispositivo: disk full",
            ),
            (
                HostWarning::PinRotatedAfterWrongAttempts,
                "PIN cambiato dopo troppi tentativi errati.",
            ),
            (
                HostWarning::InputFailed {
                    detail: "SendInput failed".into(),
                    suppressed: 0,
                },
                "Controllo remoto non riuscito: SendInput failed",
            ),
            (
                HostWarning::InputFailed {
                    detail: "SendInput failed".into(),
                    suppressed: 2,
                },
                "Controllo remoto non riuscito: SendInput failed (2 errori simili non mostrati)",
            ),
            (
                HostWarning::BitrateFixed("driver refused".into()),
                "L'encoder non può adattare il bitrate alla rete: resta fisso per questa \
                 sessione (driver refused).",
            ),
            (
                HostWarning::ClipboardUnavailable("no window".into()),
                "Impossibile condividere gli appunti in questa sessione (no window).",
            ),
            (
                HostWarning::ClipboardTooLarge,
                "Il testo copiato è troppo lungo per essere condiviso (oltre 256 KiB).",
            ),
            (
                HostWarning::ClipboardFailed("busy".into()),
                "Condivisione degli appunti non riuscita: busy",
            ),
            (
                HostWarning::ScreenUnavailable("in use".into()),
                "Impossibile condividere lo schermo scelto dal visualizzatore: resta quello di \
                 prima (in use).",
            ),
        ];
        for (warning, text) in cases {
            assert_eq!(host_warning(&warning), text);
        }
    }

    #[test]
    fn host_rows_and_viewer_lines() {
        let address = "10.0.0.2:7420".parse().ok();
        assert_eq!(
            host_details("2560×1440", address, false),
            "2560×1440 · 10.0.0.2:7420"
        );
        assert_eq!(
            host_details("2560×1440", address, true),
            "2560×1440 · 10.0.0.2:7420 · Accesso memorizzato"
        );
        assert_eq!(
            host_details("2560×1440", None, false),
            "2560×1440 · nessun indirizzo raggiungibile"
        );
        assert_eq!(
            viewer_connecting("UFFICIO"),
            "Connessione a UFFICIO in corso…"
        );
        assert_eq!(viewer_connected("UFFICIO"), "Connesso a UFFICIO");
        assert_eq!(
            viewer_failed("UFFICIO", "PIN errato."),
            "UFFICIO: PIN errato."
        );
    }
}
