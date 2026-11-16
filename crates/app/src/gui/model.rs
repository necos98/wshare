//! Pure state of the desktop interface. No I/O: the controller feeds it
//! events, the view only reads it.

use std::net::SocketAddr;

use discovery::{DiscoveryEvent, HostList};
use net::Fingerprint;

use crate::host::HostEvent;
use crate::options::HostOptions;
use crate::text;
use crate::trust::AuthorizedDevice;
use crate::viewer::report::FailureCause;

use super::approval::ApprovalPrompt;
use super::host_row::{self, HostRow};
use super::notifications::Notification;
use super::ports::MonitorChoice;
use super::remembered::RememberedHosts;
use super::settings::Settings;
use super::settings_state::Page;

/// Identifies a viewer process launched by the interface.
pub type ViewerId = u64;

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
    /// The screen being shared, as a line for people (known while sharing).
    pub screen: Option<String>,
    /// A viewer waiting for the click that accepts or refuses it.
    pub approval: Option<ApprovalPrompt>,
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
            screen: None,
            approval: None,
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
                pin,
                fingerprint,
                monitor,
                width,
                height,
                ..
            } => {
                self.listening = true;
                self.pin = Some(pin);
                self.screen = Some(text::shared_screen(&monitor, width, height));
                return Some(Fingerprint::host_id_of_hex(&fingerprint));
            }
            HostEvent::PinChanged { pin } => self.pin = Some(pin),
            HostEvent::AuthorizedDevices(devices) => self.authorized = devices,
            HostEvent::ApprovalRequested(request) => {
                self.approval = Some(ApprovalPrompt::new(request));
            }
            HostEvent::ApprovalClosed { id } => {
                if self.approval.as_ref().is_some_and(|prompt| prompt.id == id) {
                    self.approval = None;
                }
            }
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
                self.notice = Some(text::viewer_rejected(address.ip(), &reason));
            }
            HostEvent::ViewerDisconnected { error, .. } => {
                self.viewer = None;
                self.input_blocked = false;
                self.notice = error.map(|e| text::session_ended(&e));
            }
            HostEvent::ScreenChanged {
                monitor,
                width,
                height,
            } => self.screen = Some(text::shared_screen(&monitor, width, height)),
            HostEvent::InputBlocked => self.input_blocked = true,
            HostEvent::Warning(warning) => self.notice = Some(text::host_warning(&warning)),
            HostEvent::Stopped { error } => {
                self.stopped(error.map(|failure| text::sharing_failed(&failure)));
            }
        }
        None
    }

    /// The line under the toggle while sharing is off.
    pub fn off_line(&self) -> &'static str {
        if self.stopping {
            text::SHARING_STOPPING
        } else {
            text::SHARING_OFF_HINT
        }
    }

    /// The status line under the PIN.
    pub fn status_line(&self) -> String {
        match (&self.viewer, self.listening) {
            (Some(v), _) => text::sharing_connected(&v.name, v.address.ip()),
            (None, true) => text::SHARING_WAITING.to_string(),
            (None, false) => text::SHARING_STARTING.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerStatus {
    Connecting,
    /// The host asked the person at its PC to accept this one.
    AwaitingApproval,
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

    /// Still being made: it can be given up.
    pub fn is_connecting(&self) -> bool {
        matches!(
            self.status,
            ViewerStatus::Connecting | ViewerStatus::AwaitingApproval
        )
    }

    pub fn status_line(&self) -> String {
        match &self.status {
            ViewerStatus::Connecting => text::viewer_connecting(&self.label),
            ViewerStatus::AwaitingApproval => text::viewer_awaiting_approval(&self.label),
            ViewerStatus::Connected => text::viewer_connected(&self.label),
            ViewerStatus::Failed(message) => text::viewer_failed(&self.label, message),
        }
    }
}

/// What a launched viewer process reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerUpdate {
    /// The host asked the person at its PC to accept this one; the answer
    /// is still to come.
    AwaitingApproval,
    /// The handshake succeeded and its window is open.
    Connected,
    /// The process ended. `cause` is why it failed, from its status line or
    /// else from its exit code; `None` when it said nothing.
    Exited {
        success: bool,
        code: Option<i32>,
        cause: Option<FailureCause>,
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
    /// What happened to the last Wake-on-LAN request that went out.
    pub wake_notice: Option<String>,
    pub viewers: Vec<ViewerSession>,
    /// Hosts this PC connects to without a PIN.
    pub remembered: RememberedHosts,
    /// Counts the times the selected host asked for the PIN again; the
    /// window empties and focuses the PIN field whenever it changes.
    pub pin_prompts: u64,
    /// The saved preferences (see `settings`).
    pub settings: Settings,
    pub page: Page,
    /// The monitors that can be shared; refreshed when the settings open.
    pub monitors: Vec<MonitorChoice>,
    /// A problem with the settings (unreadable file, not saved, ...).
    pub settings_notice: Option<String>,
    /// What the running sharing was started with, to tell when the saved
    /// streaming settings differ from it.
    pub applied: Option<HostOptions>,
    /// The sharing is being restarted to apply new streaming settings.
    pub restarting: bool,
    /// Balloons waiting to be shown (see `notifications`).
    pub notifications: Vec<Notification>,
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

    /// The computers to list: those on the LAN and the ones known but not
    /// online, in display order (see `host_row::rows`).
    pub fn host_rows(&self) -> Vec<HostRow> {
        host_row::rows(&self.hosts, &self.remembered, self.own_host_id.as_deref())
    }

    pub fn host_row(&self, id: &str) -> Option<HostRow> {
        self.host_rows().into_iter().find(|row| row.id == id)
    }

    pub fn viewer_mut(&mut self, id: ViewerId) -> Option<&mut ViewerSession> {
        self.viewers.iter_mut().find(|v| v.id == id)
    }

    pub fn apply_viewer_update(&mut self, id: ViewerId, update: ViewerUpdate) {
        match update {
            ViewerUpdate::AwaitingApproval => {
                if let Some(viewer) = self.viewer_mut(id).filter(|v| v.is_connecting()) {
                    viewer.status = ViewerStatus::AwaitingApproval;
                }
            }
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
                cause,
            } => {
                let Some(viewer) = self.viewer_mut(id) else {
                    return;
                };
                // The PIN can be asked again in the row of a host that was
                // picked from the list and is still there.
                let listed_host = viewer
                    .host_id
                    .clone()
                    .filter(|host_id| self.host_row(host_id).is_some());
                let status = match (cause, listed_host) {
                    (
                        Some(pin_cause @ (FailureCause::PinRequired | FailureCause::WrongPin)),
                        Some(host_id),
                    ) => {
                        self.ask_pin_again(id, host_id, &pin_cause);
                        return;
                    }
                    (Some(cause), _) => ViewerStatus::Failed(text::viewer_failure(&cause)),
                    (None, _) => ViewerStatus::Failed(text::viewer_exited_unexpectedly(code)),
                };
                if let Some(viewer) = self.viewer_mut(id) {
                    viewer.status = status;
                }
            }
        }
    }

    /// A listed host refused the connection for want of a (right) PIN: its
    /// row stays selected with an empty PIN field that takes the keyboard
    /// and says why, instead of a failed viewer line.
    fn ask_pin_again(&mut self, viewer: ViewerId, host_id: String, cause: &FailureCause) {
        self.viewers.retain(|v| v.id != viewer);
        if *cause == FailureCause::PinRequired {
            self.remembered.pin_required(&host_id);
        }
        self.selected_host = Some(host_id);
        self.connect_error = Some(
            match cause {
                FailureCause::WrongPin => text::PIN_WRONG,
                _ => text::PIN_AGAIN,
            }
            .to_string(),
        );
        self.pin_prompts += 1;
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
            (Some(v), _, _) => text::tray_connected(&v.name),
            (None, true, Some(pin)) => text::tray_waiting(&format_pin(pin)),
            (None, true, None) => text::TRAY_STARTING.to_string(),
            (None, false, _) if s.stopping => text::TRAY_STOPPING.to_string(),
            (None, false, _) => text::TRAY_OFF.to_string(),
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
    use discovery::{DiscoveredHost, HostAnnouncement};
    use protocol::control::{PROTOCOL_VERSION, RejectReason};

    use super::*;
    use crate::gui::remembered::tests::known;
    use crate::host::{HostFailure, HostWarning, SessionFailure};
    use crate::trust::Timestamp;

    const NOW: Timestamp = Timestamp::from_unix_seconds(0);

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
                mac: None,
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
    fn the_screen_shared_is_known_from_listening_and_follows_changes() {
        let mut s = Sharing::default();
        s.started();
        assert_eq!(s.screen, None);
        s.apply(HostEvent::Listening {
            computer_name: "QUESTO-PC".into(),
            port: 7420,
            pin: "111111".into(),
            fingerprint: "ff".into(),
            monitor: r"\\.\DISPLAY1".into(),
            width: 1920,
            height: 1080,
        });
        assert_eq!(
            s.screen.as_deref(),
            Some("Schermo condiviso: DISPLAY1 (1920×1080)")
        );
        s.apply(HostEvent::ScreenChanged {
            monitor: "DISPLAY2".into(),
            width: 2560,
            height: 1440,
        });
        assert_eq!(
            s.screen.as_deref(),
            Some("Schermo condiviso: DISPLAY2 (2560×1440)")
        );
        s.stopped(None);
        assert_eq!(s.screen, None, "known only while sharing");
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
            error: Some(SessionFailure("encoder failed".into())),
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
        s.apply(HostEvent::Warning(HostWarning::LanAnnouncementFailed(
            "no route".into(),
        )));
        assert_eq!(
            s.notice.as_deref(),
            Some(
                text::host_warning(&HostWarning::LanAnnouncementFailed("no route".into())).as_str()
            )
        );
    }

    #[test]
    fn a_pin_rotated_after_wrong_attempts_is_shown_with_its_reason() {
        let mut s = Sharing::default();
        s.started();
        s.apply(listening("111111", "ff"));
        s.apply(HostEvent::PinChanged {
            pin: "222222".into(),
        });
        s.apply(HostEvent::Warning(
            HostWarning::PinRotatedAfterWrongAttempts,
        ));
        assert_eq!(s.pin.as_deref(), Some("222222"));
        assert_eq!(
            s.notice.as_deref(),
            Some("PIN cambiato dopo troppi tentativi errati.")
        );
    }

    #[test]
    fn turning_away_a_second_viewer_is_noted() {
        let mut s = Sharing::default();
        s.apply(HostEvent::ViewerRejected {
            address: addr("10.0.0.3:1"),
            reason: RejectReason::Busy,
        });
        assert_eq!(
            s.notice.as_deref(),
            Some(
                "Connessione rifiutata da 10.0.0.3: computer già in uso da un altro visualizzatore"
            )
        );
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
            error: Some(HostFailure::PortInUse { port: 7420 }),
        });
        assert!(!s.enabled);
        assert_eq!(s.pin, None);
        assert!(!s.input_allowed);
        assert_eq!(
            s.error.as_deref(),
            Some(
                "Impossibile condividere: la porta 7420 è già in uso (un altro wshare è aperto?)."
            )
        );
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
        assert_eq!(rows[0].details(NOW), "2560×1440 · 192.168.1.3:7420");
        assert!(!rows[1].compatible);
        assert!(!rows[1].connectable());
        assert!(rows[1].details(NOW).contains("non compatibile"));
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
                cause: None,
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
        state.apply_viewer_update(4, failed_with(FailureCause::WrongPin));
        assert_eq!(
            state.viewers[0].status,
            ViewerStatus::Failed("PIN errato.".into())
        );
        assert!(state.active_viewer_for(address).is_none());
    }

    #[test]
    fn a_busy_or_pausing_host_is_explained_on_the_viewer_line() {
        let (mut state, id) = remembered_office();
        state.apply_viewer_update(7, failed_with(FailureCause::HostBusy));
        assert_eq!(
            state.viewers[0].status_line(),
            "UFFICIO: Computer già in uso da un altro visualizzatore: riprova quando si libera."
        );
        assert!(
            state.host_row(&id).unwrap().remembered,
            "a busy host keeps its remembered access"
        );

        let mut state = GuiState::default();
        state.viewers.push(ViewerSession {
            id: 3,
            label: "10.0.0.5".into(),
            address: addr("10.0.0.5:7420"),
            host_id: None,
            status: ViewerStatus::Connecting,
        });
        state.apply_viewer_update(3, failed_with(FailureCause::TooManyAttempts));
        assert_eq!(
            state.viewers[0].status_line(),
            "10.0.0.5: Troppi PIN errati: attendi un minuto e riprova."
        );
    }

    fn failed_with(cause: FailureCause) -> ViewerUpdate {
        ViewerUpdate::Exited {
            success: false,
            code: Some(cause.exit_code()),
            cause: Some(cause),
        }
    }

    /// The listed host `UFFICIO`, remembered, with a viewer opening on it.
    fn remembered_office() -> (GuiState, String) {
        let office = known(0xab, "UFFICIO");
        let id = office.fingerprint.host_id();
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
            row.details(NOW),
            "2560×1440 · 192.168.1.3:7420 · Accesso memorizzato"
        );
    }

    #[test]
    fn pin_required_brings_back_the_pin_field() {
        let (mut state, id) = remembered_office();
        state.apply_viewer_update(7, failed_with(FailureCause::PinRequired));
        assert!(!state.host_row(&id).unwrap().remembered, "PIN field shown");
        assert_eq!(state.selected_host.as_deref(), Some(id.as_str()));
        assert_eq!(state.connect_error.as_deref(), Some(text::PIN_AGAIN));
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
    fn a_wrong_pin_keeps_the_host_selected_and_asks_again_in_its_row() {
        let (mut state, id) = remembered_office();
        state.remembered.pin_required(&id);
        let prompts = state.pin_prompts;
        state.apply_viewer_update(7, failed_with(FailureCause::WrongPin));
        assert_eq!(state.selected_host.as_deref(), Some(id.as_str()));
        assert_eq!(state.connect_error.as_deref(), Some(text::PIN_WRONG));
        assert!(state.viewers.is_empty(), "the row says it, not a line");
        assert_eq!(state.pin_prompts, prompts + 1, "the PIN field is refocused");
    }

    #[test]
    fn a_wrong_pin_for_an_address_is_a_failed_line() {
        let mut state = GuiState::default();
        state.viewers.push(ViewerSession {
            id: 2,
            label: "10.0.0.5".into(),
            address: addr("10.0.0.5:7420"),
            host_id: None,
            status: ViewerStatus::Connecting,
        });
        state.apply_viewer_update(2, failed_with(FailureCause::WrongPin));
        assert_eq!(state.viewers[0].status_line(), "10.0.0.5: PIN errato.");
        assert_eq!(state.selected_host, None);
    }

    #[test]
    fn a_wrong_pin_for_a_host_gone_and_forgotten_is_a_failed_line() {
        let (mut state, id) = remembered_office();
        state.apply_discovery(DiscoveryEvent::Lost { id });
        state.remembered.set(vec![], None);
        state.apply_viewer_update(7, failed_with(FailureCause::WrongPin));
        assert_eq!(state.selected_host, None);
        assert_eq!(state.viewers[0].status_line(), "UFFICIO: PIN errato.");
    }

    #[test]
    fn a_wrong_pin_for_a_known_host_that_is_not_online_asks_again_in_its_row() {
        let (mut state, id) = remembered_office();
        state.apply_discovery(DiscoveryEvent::Lost { id: id.clone() });
        state.apply_viewer_update(7, failed_with(FailureCause::WrongPin));
        assert_eq!(state.selected_host.as_deref(), Some(id.as_str()));
        assert_eq!(state.connect_error.as_deref(), Some(text::PIN_WRONG));
        assert!(state.viewers.is_empty(), "the row says it, not a line");
    }

    #[test]
    fn the_end_of_a_session_says_who_ended_it() {
        let cases = [
            (
                FailureCause::HostLeft,
                "UFFICIO: Il computer ha interrotto la condivisione.",
            ),
            (
                FailureCause::HostDisconnected,
                "UFFICIO: Disconnesso dall'altro computer.",
            ),
            (FailureCause::ConnectionLost, "UFFICIO: Connessione persa."),
        ];
        for (cause, line) in cases {
            let (mut state, _) = remembered_office();
            state.apply_viewer_update(7, ViewerUpdate::Connected);
            state.apply_viewer_update(7, failed_with(cause));
            assert_eq!(state.viewers.len(), 1, "the entry stays, with the reason");
            assert_eq!(state.viewers[0].status_line(), line);
            assert!(!state.viewers[0].is_active());
        }
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
        state.apply_viewer_update(2, failed_with(FailureCause::PinRequired));
        assert_eq!(
            state.viewers[0].status_line(),
            "10.0.0.5: Questo computer richiede il PIN."
        );
    }

    #[test]
    fn a_changed_identity_is_reported_on_the_viewer_line() {
        let (mut state, id) = remembered_office();
        state.apply_viewer_update(7, failed_with(FailureCause::HostChanged));
        let ViewerStatus::Failed(message) = &state.viewers[0].status else {
            panic!()
        };
        assert!(message.contains("identità"), "{message}");
        assert!(
            state.host_row(&id).unwrap().remembered,
            "still offers Rimuovi"
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
