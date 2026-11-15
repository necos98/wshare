//! Pure state of the desktop interface. No I/O: the controller feeds it
//! events, the view only reads it.

use std::net::SocketAddr;

use discovery::{DiscoveredHost, DiscoveryEvent, HostList};
use protocol::control::PROTOCOL_VERSION;

use crate::host::HostEvent;
use crate::trust::{AuthorizedDevice, HOST_ID_LEN};
use crate::viewer::failure::FailureKind;

use super::remembered::RememberedHosts;
use super::viewer_error;

/// Identifies a viewer process launched by the interface.
pub type ViewerId = u64;

/// Shown under a remembered host that refused to connect without a PIN.
pub const PIN_AGAIN: &str = "Questo computer richiede di nuovo il PIN.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedViewer {
    pub name: String,
    pub address: SocketAddr,
}

/// "Condividi questo schermo".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sharing {
    /// Position of the toggle.
    pub enabled: bool,
    /// Switched off, the service is still winding down: the toggle cannot
    /// be switched on again yet.
    pub stopping: bool,
    /// The service reported `Listening`.
    pub listening: bool,
    pub pin: Option<String>,
    pub viewer: Option<ConnectedViewer>,
    /// User preference, kept while sharing is off.
    pub input_allowed: bool,
    /// Windows is refusing injected input right now.
    pub input_blocked: bool,
    /// Something non-fatal worth a small line (warning, rejected viewer).
    pub notice: Option<String>,
    /// Why sharing is off although the user switched it on.
    pub error: Option<String>,
    /// Viewers that connect without a PIN (known while sharing).
    pub authorized: Vec<AuthorizedDevice>,
}

impl Default for Sharing {
    fn default() -> Self {
        Self {
            enabled: false,
            stopping: false,
            listening: false,
            pin: None,
            viewer: None,
            input_allowed: true,
            input_blocked: false,
            notice: None,
            error: None,
            authorized: Vec::new(),
        }
    }
}

impl Sharing {
    /// Switches the toggle on (the service is starting).
    pub fn started(&mut self) {
        *self = Self {
            enabled: true,
            input_allowed: self.input_allowed,
            ..Self::default()
        };
    }

    /// Switches the toggle off, keeping the preferences; `error` says why.
    pub fn stopped(&mut self, error: Option<String>) {
        *self = Self {
            input_allowed: self.input_allowed,
            error,
            ..Self::default()
        };
    }

    /// Switches the toggle off while the service winds down in the
    /// background (until [`Self::stop_finished`]).
    pub fn begin_stopping(&mut self) {
        *self = Self {
            input_allowed: self.input_allowed,
            stopping: true,
            ..Self::default()
        };
    }

    /// The background stop is over: the toggle can be switched on again.
    pub fn stop_finished(&mut self) {
        self.stopping = false;
    }

    /// Applies an event of the running service. Returns the id this PC
    /// announces on the LAN when it becomes known.
    pub fn apply(&mut self, event: HostEvent) -> Option<String> {
        match event {
            HostEvent::Listening {
                pin, fingerprint, ..
            } => {
                self.listening = true;
                self.pin = Some(pin);
                return Some(fingerprint.chars().take(HOST_ID_LEN).collect());
            }
            HostEvent::PinChanged { pin } => self.pin = Some(pin),
            HostEvent::AuthorizedDevices(devices) => self.authorized = devices,
            HostEvent::ViewerConnected {
                viewer_name,
                address,
            } => {
                self.viewer = Some(ConnectedViewer {
                    name: viewer_name,
                    address,
                });
                self.input_blocked = false;
                self.notice = None;
            }
            HostEvent::ViewerRejected { address, reason } => {
                self.notice = Some(format!(
                    "Connessione rifiutata da {}: {}",
                    address.ip(),
                    viewer_error::reason_text(&reason)
                ));
            }
            HostEvent::ViewerDisconnected { error, .. } => {
                self.viewer = None;
                self.input_blocked = false;
                self.notice = error.map(|e| format!("Sessione terminata: {e}"));
            }
            HostEvent::InputBlocked => self.input_blocked = true,
            HostEvent::Warning(warning) => self.notice = Some(warning),
            HostEvent::Stopped { error } => self.stopped(error),
        }
        None
    }

    /// The line under the toggle while sharing is off.
    pub fn off_line(&self) -> &'static str {
        if self.stopping {
            "Arresto in corso…"
        } else {
            "Attivalo per far vedere e controllare questo PC da un altro computer in rete."
        }
    }

    /// The status line under the PIN.
    pub fn status_line(&self) -> String {
        match (&self.viewer, self.listening) {
            (Some(v), _) => format!("Connesso: {} ({})", v.name, v.address.ip()),
            (None, true) => "In attesa di connessioni…".to_string(),
            (None, false) => "Avvio in corso…".to_string(),
        }
    }
}

/// A host of the "Computer in rete" list, ready to display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRow {
    pub id: String,
    pub name: String,
    pub resolution: String,
    pub address: Option<SocketAddr>,
    /// Same protocol version as this program.
    pub compatible: bool,
    /// This PC connects to it without a PIN.
    pub remembered: bool,
}

impl HostRow {
    fn new(host: &DiscoveredHost, remembered: bool) -> Self {
        let a = &host.announcement;
        Self {
            id: a.id.clone(),
            name: a.display_name.clone(),
            resolution: format!("{}×{}", a.width, a.height),
            address: host.preferred_address(),
            compatible: a.protocol_version == PROTOCOL_VERSION,
            remembered,
        }
    }

    pub fn connectable(&self) -> bool {
        self.compatible && self.address.is_some()
    }

    /// Second line of the row.
    pub fn details(&self) -> String {
        if !self.compatible {
            return "Versione di wshare diversa: non compatibile".to_string();
        }
        let line = match self.address {
            Some(address) => format!("{} · {address}", self.resolution),
            None => format!("{} · nessun indirizzo raggiungibile", self.resolution),
        };
        if self.remembered {
            format!("{line} · Accesso memorizzato")
        } else {
            line
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerStatus {
    Connecting,
    Connected,
    Failed(String),
}

/// A viewer window launched from the interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerSession {
    pub id: ViewerId,
    pub label: String,
    pub address: SocketAddr,
    /// LAN id of the host, when it was picked from the list.
    pub host_id: Option<String>,
    pub status: ViewerStatus,
}

impl ViewerSession {
    pub fn is_active(&self) -> bool {
        !matches!(self.status, ViewerStatus::Failed(_))
    }

    pub fn status_line(&self) -> String {
        match &self.status {
            ViewerStatus::Connecting => format!("Connessione a {} in corso…", self.label),
            ViewerStatus::Connected => format!("Connesso a {}", self.label),
            ViewerStatus::Failed(message) => format!("{}: {message}", self.label),
        }
    }
}

/// What a launched viewer process reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerUpdate {
    /// The handshake succeeded and its window is open.
    Connected,
    /// The process ended. `stderr` is everything it wrote there.
    Exited {
        success: bool,
        code: Option<i32>,
        stderr: String,
    },
}

/// Everything the window shows.
#[derive(Debug, Clone, Default)]
pub struct GuiState {
    pub sharing: Sharing,
    /// Id under which this PC announces itself, to hide it from the list.
    pub own_host_id: Option<String>,
    pub hosts: HostList,
    pub discovery_error: Option<String>,
    pub selected_host: Option<String>,
    /// Problem with the last connection request (bad address, empty PIN).
    pub connect_error: Option<String>,
    pub viewers: Vec<ViewerSession>,
    /// Hosts this PC connects to without a PIN.
    pub remembered: RememberedHosts,
}

impl GuiState {
    pub fn apply_host_event(&mut self, event: HostEvent) {
        if let Some(id) = self.sharing.apply(event) {
            self.own_host_id = Some(id);
        }
    }

    pub fn apply_discovery(&mut self, event: DiscoveryEvent) {
        if let DiscoveryEvent::Lost { id } = &event
            && self.selected_host.as_ref() == Some(id)
        {
            self.selected_host = None;
        }
        self.hosts.apply(event);
    }

    /// The other computers on the LAN, sorted by name.
    pub fn host_rows(&self) -> Vec<HostRow> {
        self.hosts
            .hosts()
            .into_iter()
            .filter(|h| Some(h.id()) != self.own_host_id.as_deref())
            .map(|h| HostRow::new(h, self.remembered.is_remembered(h.id())))
            .collect()
    }

    pub fn host_row(&self, id: &str) -> Option<HostRow> {
        self.host_rows().into_iter().find(|row| row.id == id)
    }

    pub fn viewer_mut(&mut self, id: ViewerId) -> Option<&mut ViewerSession> {
        self.viewers.iter_mut().find(|v| v.id == id)
    }

    pub fn apply_viewer_update(&mut self, id: ViewerId, update: ViewerUpdate) {
        match update {
            ViewerUpdate::Connected => {
                let Some(viewer) = self.viewer_mut(id) else {
                    return;
                };
                viewer.status = ViewerStatus::Connected;
                if let Some(host_id) = viewer.host_id.clone() {
                    self.remembered.connected(&host_id);
                }
            }
            ViewerUpdate::Exited { success: true, .. } => self.viewers.retain(|v| v.id != id),
            ViewerUpdate::Exited {
                success: false,
                code,
                stderr,
            } => {
                let Some(viewer) = self.viewer_mut(id) else {
                    return;
                };
                let failure = viewer_error::describe(&stderr, code);
                match (failure.kind, viewer.host_id.clone()) {
                    (Some(FailureKind::PinRequired), Some(host_id)) => {
                        self.ask_pin_again(id, host_id)
                    }
                    _ => viewer.status = ViewerStatus::Failed(failure.message),
                }
            }
        }
    }

    /// A listed host refused to connect without a PIN: its row opens with
    /// the PIN field and says why, instead of a failed viewer line.
    fn ask_pin_again(&mut self, viewer: ViewerId, host_id: String) {
        self.viewers.retain(|v| v.id != viewer);
        self.remembered.pin_required(&host_id);
        self.selected_host = Some(host_id);
        self.connect_error = Some(PIN_AGAIN.to_string());
    }

    /// A viewer already open (or opening) on `address`.
    pub fn active_viewer_for(&self, address: SocketAddr) -> Option<&ViewerSession> {
        self.viewers
            .iter()
            .find(|v| v.address == address && v.is_active())
    }

    /// Text of the tray icon's tooltip.
    pub fn tray_tooltip(&self) -> String {
        let s = &self.sharing;
        match (&s.viewer, s.enabled, &s.pin) {
            (Some(v), _, _) => format!("wshare - connesso: {}", v.name),
            (None, true, Some(pin)) => format!("wshare - in attesa (PIN {})", format_pin(pin)),
            (None, true, None) => "wshare - avvio condivisione…".to_string(),
            (None, false, _) if s.stopping => "wshare - arresto condivisione…".to_string(),
            (None, false, _) => "wshare - condivisione disattivata".to_string(),
        }
    }
}

/// Groups a 6-digit PIN as "123 456" for readability; other PINs unchanged.
pub fn format_pin(pin: &str) -> String {
    if pin.len() == 6 && pin.chars().all(|c| c.is_ascii_digit()) {
        format!("{} {}", &pin[..3], &pin[3..])
    } else {
        pin.to_string()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use discovery::HostAnnouncement;
    use protocol::control::RejectReason;

    use super::*;
    use crate::gui::remembered::tests::known;
    use crate::trust::host_id;

    pub fn listening(pin: &str, fingerprint: &str) -> HostEvent {
        HostEvent::Listening {
            computer_name: "QUESTO-PC".into(),
            port: 7420,
            pin: pin.into(),
            fingerprint: fingerprint.into(),
            monitor: "DISPLAY1".into(),
            width: 1920,
            height: 1080,
        }
    }

    pub fn found(id: &str, name: &str, ip: &str, version: u16) -> DiscoveryEvent {
        DiscoveryEvent::Found(DiscoveredHost::new(
            HostAnnouncement {
                id: id.into(),
                display_name: name.into(),
                port: 7420,
                protocol_version: version,
                width: 2560,
                height: 1440,
            },
            vec![ip.parse().unwrap()],
        ))
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn listening_sets_pin_and_own_id() {
        let mut state = GuiState::default();
        state.sharing.started();
        assert_eq!(state.sharing.status_line(), "Avvio in corso…");
        state.apply_host_event(listening("123456", "0123456789abcdef0011223344"));
        assert_eq!(state.sharing.pin.as_deref(), Some("123456"));
        assert_eq!(state.own_host_id.as_deref(), Some("0123456789abcdef"));
        assert_eq!(state.sharing.status_line(), "In attesa di connessioni…");
    }

    #[test]
    fn pin_change() {
        let mut s = Sharing::default();
        s.started();
        s.apply(listening("111111", "ff"));
        s.apply(HostEvent::PinChanged {
            pin: "222222".into(),
        });
        assert_eq!(s.pin.as_deref(), Some("222222"));
    }

    #[test]
    fn viewer_connects_and_disconnects() {
        let mut s = Sharing::default();
        s.started();
        s.apply(listening("111111", "ff"));
        s.apply(HostEvent::InputBlocked);
        s.apply(HostEvent::ViewerConnected {
            viewer_name: "PORTATILE".into(),
            address: addr("192.168.1.7:50000"),
        });
        assert_eq!(s.status_line(), "Connesso: PORTATILE (192.168.1.7)");
        assert!(!s.input_blocked, "a new session starts unblocked");
        s.apply(HostEvent::InputBlocked);
        assert!(s.input_blocked);
        s.apply(HostEvent::ViewerDisconnected {
            address: addr("192.168.1.7:50000"),
            stats: Default::default(),
            error: None,
        });
        assert_eq!(s.viewer, None);
        assert!(!s.input_blocked);
        assert_eq!(s.notice, None);
        assert_eq!(s.status_line(), "In attesa di connessioni…");
    }

    #[test]
    fn abnormal_disconnect_and_rejection_leave_a_notice() {
        let mut s = Sharing::default();
        s.apply(HostEvent::ViewerDisconnected {
            address: addr("10.0.0.2:1"),
            stats: Default::default(),
            error: Some("encoder failed".into()),
        });
        assert_eq!(
            s.notice.as_deref(),
            Some("Sessione terminata: encoder failed")
        );
        s.apply(HostEvent::ViewerRejected {
            address: addr("10.0.0.2:1"),
            reason: RejectReason::WrongPin,
        });
        assert_eq!(
            s.notice.as_deref(),
            Some("Connessione rifiutata da 10.0.0.2: PIN errato")
        );
        s.apply(HostEvent::Warning("annuncio fallito".into()));
        assert_eq!(s.notice.as_deref(), Some("annuncio fallito"));
    }

    #[test]
    fn stopped_resets_but_keeps_preference() {
        let mut s = Sharing {
            input_allowed: false,
            ..Sharing::default()
        };
        s.started();
        s.apply(listening("111111", "ff"));
        s.apply(HostEvent::Stopped {
            error: Some("capture lost".into()),
        });
        assert!(!s.enabled);
        assert_eq!(s.pin, None);
        assert!(!s.input_allowed);
        assert_eq!(s.error.as_deref(), Some("capture lost"));
        s.started();
        assert_eq!(s.error, None, "starting again clears the error");
        assert!(!s.input_allowed);
    }

    #[test]
    fn stopping_blocks_restart_until_finished() {
        let mut s = Sharing {
            input_allowed: false,
            ..Sharing::default()
        };
        s.started();
        s.apply(listening("111111", "ff"));
        s.begin_stopping();
        assert!(!s.enabled);
        assert!(s.stopping);
        assert_eq!(s.pin, None);
        assert!(!s.input_allowed, "preference kept");
        assert_eq!(s.off_line(), "Arresto in corso…");
        s.stop_finished();
        assert!(!s.stopping);
        assert!(s.off_line().starts_with("Attivalo"));
    }

    #[test]
    fn host_rows_hide_own_host_and_flag_incompatible() {
        let mut state = GuiState::default();
        state.apply_discovery(found(
            "0123456789abcdef",
            "QUESTO-PC",
            "192.168.1.2",
            PROTOCOL_VERSION,
        ));
        state.apply_discovery(found("bbbb", "Ufficio", "192.168.1.3", PROTOCOL_VERSION));
        state.apply_discovery(found(
            "aaaa",
            "Vecchio",
            "192.168.1.4",
            PROTOCOL_VERSION + 1,
        ));
        assert_eq!(state.host_rows().len(), 3, "own id unknown yet");

        state.apply_host_event(listening("1", "0123456789abcdef99"));
        let rows = state.host_rows();
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["Ufficio", "Vecchio"]);
        assert!(rows[0].connectable());
        assert_eq!(rows[0].details(), "2560×1440 · 192.168.1.3:7420");
        assert!(!rows[1].compatible);
        assert!(!rows[1].connectable());
        assert!(rows[1].details().contains("non compatibile"));
    }

    #[test]
    fn own_id_is_remembered_after_sharing_stops() {
        let mut state = GuiState::default();
        state.apply_host_event(listening("1", "0123456789abcdef99"));
        state.apply_host_event(HostEvent::Stopped { error: None });
        state.apply_discovery(found(
            "0123456789abcdef",
            "QUESTO-PC",
            "192.168.1.2",
            PROTOCOL_VERSION,
        ));
        assert!(state.host_rows().is_empty());
    }

    #[test]
    fn losing_the_selected_host_clears_selection() {
        let mut state = GuiState::default();
        state.apply_discovery(found("bbbb", "Ufficio", "192.168.1.3", PROTOCOL_VERSION));
        state.selected_host = Some("bbbb".into());
        state.apply_discovery(DiscoveryEvent::Lost { id: "bbbb".into() });
        assert_eq!(state.selected_host, None);
        assert!(state.host_rows().is_empty());
    }

    #[test]
    fn viewer_lifecycle() {
        let mut state = GuiState::default();
        let address = addr("192.168.1.3:7420");
        state.viewers.push(ViewerSession {
            id: 1,
            label: "Ufficio".into(),
            address,
            host_id: None,
            status: ViewerStatus::Connecting,
        });
        assert_eq!(
            state.viewers[0].status_line(),
            "Connessione a Ufficio in corso…"
        );
        assert!(state.active_viewer_for(address).is_some());
        state.apply_viewer_update(1, ViewerUpdate::Connected);
        assert_eq!(state.viewers[0].status_line(), "Connesso a Ufficio");
        state.apply_viewer_update(
            1,
            ViewerUpdate::Exited {
                success: true,
                code: Some(0),
                stderr: String::new(),
            },
        );
        assert!(
            state.viewers.is_empty(),
            "a window closed normally disappears"
        );
    }

    #[test]
    fn failed_viewer_shows_readable_error() {
        let mut state = GuiState::default();
        let address = addr("192.168.1.3:7420");
        state.viewers.push(ViewerSession {
            id: 4,
            label: "Ufficio".into(),
            address,
            host_id: None,
            status: ViewerStatus::Connecting,
        });
        state.apply_viewer_update(4, failed_with(FailureKind::WrongPin));
        assert_eq!(
            state.viewers[0].status,
            ViewerStatus::Failed("PIN errato.".into())
        );
        assert!(state.active_viewer_for(address).is_none());
    }

    fn failed_with(kind: FailureKind) -> ViewerUpdate {
        ViewerUpdate::Exited {
            success: false,
            code: Some(kind.exit_code()),
            stderr: "error: the host refused\n".into(),
        }
    }

    /// The listed host `UFFICIO`, remembered, with a viewer opening on it.
    fn remembered_office() -> (GuiState, String) {
        let office = known(0xab, "UFFICIO");
        let id = host_id(&office.fingerprint);
        let mut state = GuiState::default();
        state.apply_discovery(found(&id, "UFFICIO", "192.168.1.3", PROTOCOL_VERSION));
        state.remembered.set(vec![office], None);
        state.viewers.push(ViewerSession {
            id: 7,
            label: "UFFICIO".into(),
            address: addr("192.168.1.3:7420"),
            host_id: Some(id.clone()),
            status: ViewerStatus::Connecting,
        });
        (state, id)
    }

    #[test]
    fn remembered_hosts_are_marked_in_the_list() {
        let (state, id) = remembered_office();
        let row = state.host_row(&id).unwrap();
        assert!(row.remembered);
        assert_eq!(
            row.details(),
            "2560×1440 · 192.168.1.3:7420 · Accesso memorizzato"
        );
    }

    #[test]
    fn pin_required_brings_back_the_pin_field() {
        let (mut state, id) = remembered_office();
        state.apply_viewer_update(7, failed_with(FailureKind::PinRequired));
        assert!(!state.host_row(&id).unwrap().remembered, "PIN field shown");
        assert_eq!(state.selected_host.as_deref(), Some(id.as_str()));
        assert_eq!(state.connect_error.as_deref(), Some(PIN_AGAIN));
        assert!(
            state.viewers.is_empty(),
            "the row says it, not a failed line"
        );

        state.viewers.push(ViewerSession {
            id: 8,
            label: "UFFICIO".into(),
            address: addr("192.168.1.3:7420"),
            host_id: Some(id.clone()),
            status: ViewerStatus::Connecting,
        });
        state.apply_viewer_update(8, ViewerUpdate::Connected);
        assert!(state.host_row(&id).unwrap().remembered, "admitted again");
    }

    #[test]
    fn pin_required_without_a_listed_host_is_a_failed_line() {
        let mut state = GuiState::default();
        state.viewers.push(ViewerSession {
            id: 2,
            label: "10.0.0.5".into(),
            address: addr("10.0.0.5:7420"),
            host_id: None,
            status: ViewerStatus::Connecting,
        });
        state.apply_viewer_update(2, failed_with(FailureKind::PinRequired));
        assert_eq!(
            state.viewers[0].status_line(),
            "10.0.0.5: Questo computer richiede il PIN."
        );
    }

    #[test]
    fn a_changed_identity_is_reported_on_the_viewer_line() {
        let (mut state, id) = remembered_office();
        state.apply_viewer_update(7, failed_with(FailureKind::HostChanged));
        let ViewerStatus::Failed(message) = &state.viewers[0].status else {
            panic!()
        };
        assert!(message.contains("identità"), "{message}");
        assert!(
            state.host_row(&id).unwrap().remembered,
            "still offers Dimentica"
        );
    }

    #[test]
    fn authorized_devices_follow_the_service() {
        let mut s = Sharing::default();
        s.started();
        let device = AuthorizedDevice {
            fingerprint: net::Fingerprint([3; 32]),
            name: "PORTATILE".into(),
            first_access: crate::trust::Timestamp::from_unix_seconds(0),
            last_access: crate::trust::Timestamp::from_unix_seconds(0),
        };
        s.apply(HostEvent::AuthorizedDevices(vec![device.clone()]));
        assert_eq!(s.authorized, [device]);
        s.apply(HostEvent::Stopped { error: None });
        assert!(s.authorized.is_empty(), "known only while sharing");
    }

    #[test]
    fn tray_tooltip_follows_status() {
        let mut state = GuiState::default();
        assert_eq!(state.tray_tooltip(), "wshare - condivisione disattivata");
        state.sharing.started();
        state.apply_host_event(listening("123456", "ff"));
        assert_eq!(state.tray_tooltip(), "wshare - in attesa (PIN 123 456)");
        state.apply_host_event(HostEvent::ViewerConnected {
            viewer_name: "PORTATILE".into(),
            address: addr("192.168.1.7:5"),
        });
        assert_eq!(state.tray_tooltip(), "wshare - connesso: PORTATILE");
        state.sharing.begin_stopping();
        assert_eq!(state.tray_tooltip(), "wshare - arresto condivisione…");
    }

    #[test]
    fn pin_grouping() {
        assert_eq!(format_pin("123456"), "123 456");
        assert_eq!(format_pin("12345"), "12345");
        assert_eq!(format_pin("abcdef"), "abcdef");
    }
}
