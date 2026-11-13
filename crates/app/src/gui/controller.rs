//! Applies user intents and background events to the [`GuiState`],
//! driving the outside world through the ports. No direct I/O.

use crate::cli::{DEFAULT_PORT, HostOptions, StreamOptions};
use crate::target::{Target, parse_target};

use super::model::{GuiState, ViewerId, ViewerSession, ViewerStatus};
use super::ports::{
    AppEvent, PendingStop, SharingControl, SharingService, TrayAction, ViewerLauncher,
};

/// What the user asked for (from the window or the tray).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    SetSharing(bool),
    ToggleSharing,
    NewPin,
    DisconnectViewer,
    SetInputAllowed(bool),
    SelectHost(Option<String>),
    ConnectHost {
        id: String,
        pin: String,
    },
    ConnectAddress {
        address: String,
        pin: String,
    },
    DismissViewer(ViewerId),
    ShowWindow,
    /// The window's close button.
    CloseWindow,
    Quit,
}

/// What the window itself must do after an intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowCommand {
    Show,
    HideToTray,
    Quit,
}

/// Closing the window while sharing keeps the app alive in the tray, so
/// the screen stays reachable; otherwise there is nothing left to do.
pub fn close_decision(sharing_enabled: bool) -> WindowCommand {
    if sharing_enabled {
        WindowCommand::HideToTray
    } else {
        WindowCommand::Quit
    }
}

pub struct Controller<S: SharingService, L: ViewerLauncher> {
    state: GuiState,
    service: S,
    launcher: L,
    sharing: Option<Box<dyn SharingControl>>,
    /// A service switched off and still stopping in the background.
    stopping: Option<Box<dyn PendingStop>>,
    /// Tags the running service's events, to drop late ones of a previous run.
    generation: u64,
    next_viewer: ViewerId,
}

impl<S: SharingService, L: ViewerLauncher> Controller<S, L> {
    pub fn new(service: S, launcher: L) -> Self {
        Self {
            state: GuiState::default(),
            service,
            launcher,
            sharing: None,
            stopping: None,
            generation: 0,
            next_viewer: 1,
        }
    }

    pub fn state(&self) -> &GuiState {
        &self.state
    }

    pub fn handle(&mut self, intent: Intent) -> Option<WindowCommand> {
        match intent {
            Intent::SetSharing(on) => self.set_sharing(on),
            Intent::ToggleSharing => self.set_sharing(!self.state.sharing.enabled),
            Intent::NewPin => {
                if let Some(sharing) = &self.sharing {
                    let pin = sharing.new_pin();
                    self.state.sharing.pin = Some(pin);
                }
            }
            Intent::DisconnectViewer => {
                if let Some(sharing) = &self.sharing {
                    sharing.disconnect_viewer();
                }
            }
            Intent::SetInputAllowed(allowed) => {
                self.state.sharing.input_allowed = allowed;
                if let Some(sharing) = &self.sharing {
                    sharing.set_input_allowed(allowed);
                }
            }
            Intent::SelectHost(id) => {
                self.state.selected_host = id;
                self.state.connect_error = None;
            }
            Intent::ConnectHost { id, pin } => self.connect_host(&id, &pin),
            Intent::ConnectAddress { address, pin } => self.connect_address(&address, &pin),
            Intent::DismissViewer(id) => self.state.viewers.retain(|v| v.id != id),
            Intent::ShowWindow => return Some(WindowCommand::Show),
            Intent::CloseWindow => {
                let decision = close_decision(self.state.sharing.enabled);
                if decision == WindowCommand::Quit {
                    self.shutdown();
                }
                return Some(decision);
            }
            Intent::Quit => {
                self.shutdown();
                return Some(WindowCommand::Quit);
            }
        }
        None
    }

    pub fn on_event(&mut self, event: AppEvent) -> Option<WindowCommand> {
        match event {
            AppEvent::Host { generation, event } => {
                if generation == self.generation && self.sharing.is_some() {
                    let stopped = matches!(event, crate::host::HostEvent::Stopped { .. });
                    self.state.apply_host_event(event);
                    if stopped && let Some(sharing) = self.sharing.take() {
                        sharing.stop();
                    }
                }
            }
            AppEvent::SharingStopped { generation } => {
                if generation == self.generation
                    && let Some(stop) = self.stopping.take()
                {
                    stop.wait(); // already over: only joins its thread
                    self.state.sharing.stop_finished();
                }
            }
            AppEvent::Discovery(event) => {
                self.state.discovery_error = None;
                self.state.apply_discovery(event);
            }
            AppEvent::DiscoveryFailed(error) => {
                self.state.discovery_error =
                    Some(format!("Ricerca in rete non disponibile: {error}"));
            }
            AppEvent::Viewer { id, update } => self.state.apply_viewer_update(id, update),
            AppEvent::Tray(action) => {
                let intent = match action {
                    TrayAction::Open => Intent::ShowWindow,
                    TrayAction::ToggleSharing => Intent::ToggleSharing,
                    TrayAction::Quit => Intent::Quit,
                };
                return self.handle(intent);
            }
        }
        None
    }

    /// Stops sharing (if on) and waits until it is over, also for a stop
    /// already running in the background. Call before the process exits so
    /// keys are released, the viewer is told Bye and the LAN announcement
    /// is withdrawn.
    pub fn shutdown(&mut self) {
        let running = self.sharing.take();
        let stopping = self.stopping.take();
        if running.is_none() && stopping.is_none() {
            return;
        }
        if let Some(sharing) = running {
            sharing.stop();
        }
        if let Some(stop) = stopping {
            stop.wait();
        }
        self.state.sharing.stopped(None);
    }

    fn set_sharing(&mut self, on: bool) {
        match (on, self.sharing.is_some()) {
            // The previous run is still stopping: it must finish first.
            (true, false) if self.stopping.is_some() => {}
            (true, false) => {
                self.generation += 1;
                let options = HostOptions {
                    stream: StreamOptions::default(),
                    port: DEFAULT_PORT,
                    pin: None,
                    view_only: !self.state.sharing.input_allowed,
                };
                match self.service.start(options, self.generation) {
                    Ok(sharing) => {
                        sharing.set_input_allowed(self.state.sharing.input_allowed);
                        self.sharing = Some(sharing);
                        self.state.sharing.started();
                    }
                    Err(e) => self.state.sharing.stopped(Some(setup_error(&e))),
                }
            }
            (false, true) => {
                // Stopping takes up to a second (Bye, keys, port): not on
                // the UI thread. `SharingStopped` ends the transient state.
                if let Some(sharing) = self.sharing.take() {
                    self.stopping = Some(sharing.stop_in_background());
                }
                self.state.sharing.begin_stopping();
            }
            _ => {}
        }
    }

    fn connect_host(&mut self, id: &str, pin: &str) {
        let Some(row) = self.state.host_row(id) else {
            self.state.connect_error = Some("Il computer non è più in rete.".into());
            return;
        };
        match row.address {
            Some(address) if row.compatible => self.launch(row.name, address, pin),
            Some(_) => {
                self.state.connect_error =
                    Some("Versione di wshare diversa: aggiorna entrambi i computer.".into());
            }
            None => self.state.connect_error = Some("Nessun indirizzo raggiungibile.".into()),
        }
    }

    fn connect_address(&mut self, text: &str, pin: &str) {
        match parse_target(Some(text.trim()), DEFAULT_PORT) {
            Target::Address(address) => {
                let label = if address.port() == DEFAULT_PORT {
                    address.ip().to_string()
                } else {
                    address.to_string()
                };
                self.launch(label, address, pin)
            }
            _ => {
                self.state.connect_error =
                    Some("Indirizzo non valido: scrivi un IP, ad esempio 192.168.1.20".into());
            }
        }
    }

    fn launch(&mut self, label: String, address: std::net::SocketAddr, pin: &str) {
        let pin = pin.trim();
        if pin.is_empty() {
            self.state.connect_error =
                Some("Inserisci il PIN mostrato sull'altro computer.".into());
            return;
        }
        if self.state.active_viewer_for(address).is_some() {
            self.state.connect_error =
                Some("Una finestra per questo computer è già aperta.".into());
            return;
        }
        let id = self.next_viewer;
        self.next_viewer += 1;
        let status = match self.launcher.launch(id, address, pin) {
            Ok(()) => ViewerStatus::Connecting,
            Err(e) => {
                ViewerStatus::Failed(format!("impossibile avviare il visualizzatore ({e:#})"))
            }
        };
        self.state.viewers.retain(|v| v.address != address);
        self.state.viewers.push(ViewerSession {
            id,
            label,
            address,
            status,
        });
        self.state.selected_host = None;
        self.state.connect_error = None;
    }
}

/// A setup failure of the sharing service, for the user.
fn setup_error(error: &anyhow::Error) -> String {
    let text = format!("{error:#}");
    let lower = text.to_ascii_lowercase();
    if lower.contains("10048")
        || lower.contains("address already in use")
        || lower.contains("binding udp")
    {
        format!(
            "Impossibile condividere: la porta {DEFAULT_PORT} è già in uso (un altro wshare è aperto?)."
        )
    } else if lower.contains("monitor") && lower.contains("not found") {
        "Impossibile condividere: monitor non trovato.".into()
    } else {
        format!("Impossibile condividere: {text}")
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::net::SocketAddr;
    use std::rc::Rc;

    use anyhow::{Result, bail};
    use protocol::control::PROTOCOL_VERSION;

    use super::*;
    use crate::gui::model::ViewerUpdate;
    use crate::gui::model::tests::{found, listening};
    use crate::host::HostEvent;

    /// Shared log of what the fakes were asked to do.
    #[derive(Default)]
    struct Log {
        calls: Vec<String>,
        fail_start: Option<&'static str>,
        fail_launch: bool,
        started: Vec<(HostOptions, u64)>,
        launched: Vec<(ViewerId, SocketAddr, String)>,
    }

    type Shared = Rc<RefCell<Log>>;

    struct FakeService(Shared);
    struct FakeControl(Shared);
    struct FakePendingStop(Shared);
    struct FakeLauncher(Shared);

    impl SharingService for FakeService {
        fn start(
            &mut self,
            options: HostOptions,
            generation: u64,
        ) -> Result<Box<dyn SharingControl>> {
            let mut log = self.0.borrow_mut();
            if let Some(error) = log.fail_start {
                bail!("{error}");
            }
            log.started.push((options, generation));
            Ok(Box::new(FakeControl(self.0.clone())))
        }
    }

    impl SharingControl for FakeControl {
        fn set_input_allowed(&self, allowed: bool) {
            self.0.borrow_mut().calls.push(format!("input {allowed}"));
        }
        fn new_pin(&self) -> String {
            self.0.borrow_mut().calls.push("new_pin".into());
            "654321".into()
        }
        fn disconnect_viewer(&self) {
            self.0.borrow_mut().calls.push("disconnect".into());
        }
        fn stop(self: Box<Self>) {
            self.0.borrow_mut().calls.push("stop".into());
        }
        fn stop_in_background(self: Box<Self>) -> Box<dyn PendingStop> {
            self.0.borrow_mut().calls.push("stop_in_background".into());
            Box::new(FakePendingStop(self.0.clone()))
        }
    }

    impl PendingStop for FakePendingStop {
        fn wait(self: Box<Self>) {
            self.0.borrow_mut().calls.push("wait".into());
        }
    }

    impl ViewerLauncher for FakeLauncher {
        fn launch(&mut self, id: ViewerId, target: SocketAddr, pin: &str) -> Result<()> {
            let mut log = self.0.borrow_mut();
            if log.fail_launch {
                bail!("file not found");
            }
            log.launched.push((id, target, pin.to_string()));
            Ok(())
        }
    }

    fn controller() -> (Controller<FakeService, FakeLauncher>, Shared) {
        let log = Shared::default();
        (
            Controller::new(FakeService(log.clone()), FakeLauncher(log.clone())),
            log,
        )
    }

    fn calls(log: &Shared) -> Vec<String> {
        std::mem::take(&mut log.borrow_mut().calls)
    }

    fn host(generation: u64, event: HostEvent) -> AppEvent {
        AppEvent::Host { generation, event }
    }

    #[test]
    fn toggle_on_and_off() {
        let (mut c, log) = controller();
        assert_eq!(c.handle(Intent::SetSharing(true)), None);
        assert!(c.state().sharing.enabled);
        let (options, generation) = log.borrow().started[0].clone();
        assert_eq!(options.port, DEFAULT_PORT);
        assert!(!options.view_only);
        assert_eq!(calls(&log), ["input true"]);

        c.on_event(host(generation, listening("123456", "abcd")));
        assert_eq!(c.state().sharing.pin.as_deref(), Some("123456"));

        c.handle(Intent::SetSharing(false));
        assert!(!c.state().sharing.enabled);
        assert!(c.state().sharing.stopping);
        assert_eq!(c.state().sharing.pin, None);
        assert_eq!(
            calls(&log),
            ["stop_in_background"],
            "the UI thread does not wait"
        );
        c.handle(Intent::SetSharing(false));
        assert!(calls(&log).is_empty(), "already off");

        c.on_event(AppEvent::SharingStopped { generation });
        assert!(!c.state().sharing.stopping);
        assert_eq!(calls(&log), ["wait"], "the finished stop thread is joined");
        c.on_event(AppEvent::SharingStopped { generation });
        assert!(calls(&log).is_empty(), "a repeated report is ignored");
    }

    #[test]
    fn setup_error_resets_toggle() {
        let (mut c, log) = controller();
        log.borrow_mut().fail_start = Some("binding UDP 0.0.0.0:7420: os error 10048");
        c.handle(Intent::SetSharing(true));
        let sharing = &c.state().sharing;
        assert!(!sharing.enabled);
        assert!(
            sharing
                .error
                .as_deref()
                .unwrap()
                .contains("porta 7420 è già in uso")
        );

        log.borrow_mut().fail_start = None;
        c.handle(Intent::ToggleSharing);
        assert!(c.state().sharing.enabled);
        assert_eq!(c.state().sharing.error, None);
    }

    #[test]
    fn service_stopping_by_itself_turns_toggle_off() {
        let (mut c, log) = controller();
        c.handle(Intent::SetSharing(true));
        calls(&log);
        c.on_event(host(
            1,
            HostEvent::Stopped {
                error: Some("capture failed".into()),
            },
        ));
        assert!(!c.state().sharing.enabled);
        assert_eq!(c.state().sharing.error.as_deref(), Some("capture failed"));
        assert_eq!(calls(&log), ["stop"], "the finished service is joined");
    }

    #[test]
    fn events_of_a_previous_run_are_ignored() {
        let (mut c, _log) = controller();
        c.handle(Intent::SetSharing(true));
        c.handle(Intent::SetSharing(false));
        c.on_event(AppEvent::SharingStopped { generation: 1 });
        c.handle(Intent::SetSharing(true));
        c.on_event(host(1, HostEvent::Stopped { error: None }));
        c.on_event(AppEvent::SharingStopped { generation: 1 });
        assert!(c.state().sharing.enabled, "stale Stopped of run 1");
        c.on_event(host(2, listening("222222", "ff")));
        assert_eq!(c.state().sharing.pin.as_deref(), Some("222222"));
    }

    #[test]
    fn new_pin_and_disconnect_reach_the_service() {
        let (mut c, log) = controller();
        c.handle(Intent::NewPin);
        c.handle(Intent::DisconnectViewer);
        assert!(calls(&log).is_empty(), "nothing to do while off");

        c.handle(Intent::SetSharing(true));
        c.on_event(host(1, listening("111111", "ff")));
        calls(&log);
        c.handle(Intent::NewPin);
        assert_eq!(c.state().sharing.pin.as_deref(), Some("654321"));
        c.handle(Intent::DisconnectViewer);
        assert_eq!(calls(&log), ["new_pin", "disconnect"]);
    }

    #[test]
    fn viewer_connect_and_disconnect_events() {
        let (mut c, _log) = controller();
        c.handle(Intent::SetSharing(true));
        let address: SocketAddr = "192.168.1.9:6000".parse().unwrap();
        c.on_event(host(1, listening("111111", "ff")));
        c.on_event(host(
            1,
            HostEvent::ViewerConnected {
                viewer_name: "PC2".into(),
                address,
            },
        ));
        assert_eq!(
            c.state().sharing.status_line(),
            "Connesso: PC2 (192.168.1.9)"
        );
        c.on_event(host(
            1,
            HostEvent::ViewerDisconnected {
                address,
                stats: Default::default(),
                error: None,
            },
        ));
        assert_eq!(c.state().sharing.status_line(), "In attesa di connessioni…");
    }

    #[test]
    fn input_allowed_flag() {
        let (mut c, log) = controller();
        c.handle(Intent::SetInputAllowed(false));
        assert!(!c.state().sharing.input_allowed);
        assert!(calls(&log).is_empty());

        c.handle(Intent::SetSharing(true));
        assert!(
            log.borrow().started[0].0.view_only,
            "preference applies at start"
        );
        assert_eq!(calls(&log), ["input false"]);
        c.handle(Intent::SetInputAllowed(true));
        assert_eq!(calls(&log), ["input true"]);
        assert!(c.state().sharing.input_allowed);
    }

    #[test]
    fn host_list_updates_and_discovery_errors() {
        let (mut c, _log) = controller();
        c.on_event(AppEvent::DiscoveryFailed("socket".into()));
        assert!(c.state().discovery_error.is_some());
        c.on_event(AppEvent::Discovery(found(
            "bbbb",
            "Ufficio",
            "192.168.1.3",
            PROTOCOL_VERSION,
        )));
        assert_eq!(c.state().discovery_error, None);
        assert_eq!(c.state().host_rows().len(), 1);
    }

    #[test]
    fn connect_to_a_listed_host() {
        let (mut c, log) = controller();
        c.on_event(AppEvent::Discovery(found(
            "bbbb",
            "Ufficio",
            "192.168.1.3",
            PROTOCOL_VERSION,
        )));
        c.handle(Intent::SelectHost(Some("bbbb".into())));
        c.handle(Intent::ConnectHost {
            id: "bbbb".into(),
            pin: " 123456 ".into(),
        });
        let address: SocketAddr = "192.168.1.3:7420".parse().unwrap();
        assert_eq!(log.borrow().launched, [(1, address, "123456".to_string())]);
        let viewer = &c.state().viewers[0];
        assert_eq!(viewer.status_line(), "Connessione a Ufficio in corso…");
        assert_eq!(c.state().selected_host, None);

        c.on_event(AppEvent::Viewer {
            id: 1,
            update: ViewerUpdate::Connected,
        });
        assert_eq!(c.state().viewers[0].status, ViewerStatus::Connected);

        c.handle(Intent::ConnectHost {
            id: "bbbb".into(),
            pin: "1".into(),
        });
        assert!(
            c.state()
                .connect_error
                .as_deref()
                .unwrap()
                .contains("già aperta")
        );
        assert_eq!(log.borrow().launched.len(), 1);
    }

    #[test]
    fn viewer_failure_message_and_dismiss() {
        let (mut c, _log) = controller();
        c.handle(Intent::ConnectAddress {
            address: "10.0.0.5".into(),
            pin: "1".into(),
        });
        c.on_event(AppEvent::Viewer {
            id: 1,
            update: ViewerUpdate::Exited {
                success: false,
                code: Some(1),
                stderr: "error: the host refused: wrong PIN".into(),
            },
        });
        assert_eq!(c.state().viewers[0].status_line(), "10.0.0.5: PIN errato.");
        c.handle(Intent::DismissViewer(1));
        assert!(c.state().viewers.is_empty());
    }

    #[test]
    fn retry_replaces_the_failed_entry() {
        let (mut c, log) = controller();
        c.handle(Intent::ConnectAddress {
            address: "10.0.0.5:9000".into(),
            pin: "1".into(),
        });
        c.on_event(AppEvent::Viewer {
            id: 1,
            update: ViewerUpdate::Exited {
                success: false,
                code: Some(1),
                stderr: String::new(),
            },
        });
        c.handle(Intent::ConnectAddress {
            address: "10.0.0.5:9000".into(),
            pin: "2".into(),
        });
        assert_eq!(c.state().viewers.len(), 1);
        assert_eq!(c.state().viewers[0].id, 2);
        assert_eq!(log.borrow().launched[1].1, "10.0.0.5:9000".parse().unwrap());
    }

    #[test]
    fn launch_failure_is_shown() {
        let (mut c, log) = controller();
        log.borrow_mut().fail_launch = true;
        c.handle(Intent::ConnectAddress {
            address: "10.0.0.5".into(),
            pin: "1".into(),
        });
        let ViewerStatus::Failed(message) = &c.state().viewers[0].status else {
            panic!()
        };
        assert!(message.contains("file not found"), "{message}");
    }

    #[test]
    fn invalid_connection_requests() {
        let (mut c, log) = controller();
        c.handle(Intent::ConnectAddress {
            address: "non-un-ip".into(),
            pin: "1".into(),
        });
        assert!(
            c.state()
                .connect_error
                .as_deref()
                .unwrap()
                .starts_with("Indirizzo non valido")
        );
        c.handle(Intent::ConnectAddress {
            address: "10.0.0.5".into(),
            pin: "  ".into(),
        });
        assert!(c.state().connect_error.as_deref().unwrap().contains("PIN"));
        c.handle(Intent::ConnectHost {
            id: "sparito".into(),
            pin: "1".into(),
        });
        assert!(
            c.state()
                .connect_error
                .as_deref()
                .unwrap()
                .contains("non è più in rete")
        );
        c.on_event(AppEvent::Discovery(found(
            "old",
            "Vecchio",
            "10.0.0.6",
            PROTOCOL_VERSION + 1,
        )));
        c.handle(Intent::ConnectHost {
            id: "old".into(),
            pin: "1".into(),
        });
        assert!(
            c.state()
                .connect_error
                .as_deref()
                .unwrap()
                .contains("Versione")
        );
        assert!(log.borrow().launched.is_empty());
        c.handle(Intent::SelectHost(None));
        assert_eq!(c.state().connect_error, None);
    }

    #[test]
    fn close_to_tray_decision() {
        assert_eq!(close_decision(true), WindowCommand::HideToTray);
        assert_eq!(close_decision(false), WindowCommand::Quit);

        let (mut c, log) = controller();
        assert_eq!(c.handle(Intent::CloseWindow), Some(WindowCommand::Quit));
        c.handle(Intent::SetSharing(true));
        calls(&log);
        assert_eq!(
            c.handle(Intent::CloseWindow),
            Some(WindowCommand::HideToTray)
        );
        assert!(c.state().sharing.enabled, "sharing continues in the tray");
        assert!(calls(&log).is_empty());
    }

    #[test]
    fn tray_actions() {
        let (mut c, log) = controller();
        assert_eq!(
            c.on_event(AppEvent::Tray(TrayAction::Open)),
            Some(WindowCommand::Show)
        );
        c.on_event(AppEvent::Tray(TrayAction::ToggleSharing));
        assert!(c.state().sharing.enabled);
        calls(&log);
        assert_eq!(
            c.on_event(AppEvent::Tray(TrayAction::Quit)),
            Some(WindowCommand::Quit)
        );
        assert!(!c.state().sharing.enabled);
        assert_eq!(calls(&log), ["stop"], "quitting stops sharing");
    }

    #[test]
    fn cannot_switch_on_while_stopping() {
        let (mut c, log) = controller();
        c.handle(Intent::SetSharing(true));
        c.handle(Intent::SetSharing(false));
        c.handle(Intent::SetSharing(true));
        c.on_event(AppEvent::Tray(TrayAction::ToggleSharing));
        assert_eq!(
            log.borrow().started.len(),
            1,
            "no second start while stopping"
        );
        assert!(!c.state().sharing.enabled);
        assert!(c.state().sharing.stopping);

        c.on_event(AppEvent::SharingStopped { generation: 1 });
        c.handle(Intent::ToggleSharing);
        assert!(c.state().sharing.enabled);
        assert_eq!(log.borrow().started[1].1, 2);
    }

    #[test]
    fn late_events_of_the_stopping_run_are_ignored() {
        let (mut c, _log) = controller();
        c.handle(Intent::SetSharing(true));
        c.on_event(host(1, listening("111111", "ff")));
        c.handle(Intent::SetSharing(false));
        c.on_event(host(
            1,
            HostEvent::Stopped {
                error: Some("late".into()),
            },
        ));
        let sharing = &c.state().sharing;
        assert!(sharing.stopping);
        assert_eq!(sharing.error, None);
        assert_eq!(sharing.pin, None);
    }

    #[test]
    fn quitting_while_stopping_waits_for_the_stop() {
        let (mut c, log) = controller();
        c.handle(Intent::SetSharing(true));
        c.handle(Intent::SetSharing(false));
        calls(&log);
        assert_eq!(c.handle(Intent::Quit), Some(WindowCommand::Quit));
        assert_eq!(calls(&log), ["wait"]);
        assert!(!c.state().sharing.stopping);
        c.shutdown();
        assert!(calls(&log).is_empty(), "nothing left to wait for");
    }

    #[test]
    fn closing_the_window_while_stopping_quits_after_the_stop() {
        let (mut c, log) = controller();
        c.handle(Intent::SetSharing(true));
        c.handle(Intent::SetSharing(false));
        calls(&log);
        assert_eq!(c.handle(Intent::CloseWindow), Some(WindowCommand::Quit));
        assert_eq!(calls(&log), ["wait"]);
    }
}
