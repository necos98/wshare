//! Applies user intents and background events to the [`GuiState`],
//! driving the outside world through the ports. No direct I/O.

use net::Fingerprint;

use crate::host::HostFailure;
use crate::options::{DEFAULT_PORT, HostOptions, StreamOptions};
use crate::target::{Target, parse_target};
use crate::text;

use super::model::{GuiState, ViewerId, ViewerSession, ViewerStatus, ViewerUpdate};
use super::ports::{
    AppEvent, KnownHostsStore, LaunchRequest, PendingStop, SharingControl, SharingService,
    TrayAction, ViewerLauncher,
};

/// What the user asked for (from the window or the tray).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    SetSharing(bool),
    ToggleSharing,
    NewPin,
    DisconnectViewer,
    SetInputAllowed(bool),
    /// Removes a viewer from this PC's authorized devices.
    RemoveDevice(Fingerprint),
    SelectHost(Option<String>),
    /// `pin` may be empty for a remembered host.
    ConnectHost {
        id: String,
        pin: String,
    },
    /// `pin` may be empty: the host then must recognise this PC.
    ConnectAddress {
        address: String,
        pin: String,
    },
    /// This PC stops remembering a listed host.
    ForgetHost {
        id: String,
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

pub struct Controller<S: SharingService, L: ViewerLauncher, K: KnownHostsStore> {
    state: GuiState,
    service: S,
    launcher: L,
    known_hosts: K,
    sharing: Option<Box<dyn SharingControl>>,
    /// A service switched off and still stopping in the background.
    stopping: Option<Box<dyn PendingStop>>,
    /// Tags the running service's events, to drop late ones of a previous run.
    generation: u64,
    next_viewer: ViewerId,
}

impl<S: SharingService, L: ViewerLauncher, K: KnownHostsStore> Controller<S, L, K> {
    pub fn new(service: S, launcher: L, known_hosts: K) -> Self {
        let mut controller = Self {
            state: GuiState::default(),
            service,
            launcher,
            known_hosts,
            sharing: None,
            stopping: None,
            generation: 0,
            next_viewer: 1,
        };
        controller.reload_known_hosts();
        controller
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
            Intent::RemoveDevice(fingerprint) => {
                // The list is updated when the service reports the change.
                if let Some(sharing) = &self.sharing {
                    sharing.forget_device(&fingerprint);
                }
            }
            Intent::SelectHost(id) => {
                self.state.selected_host = id;
                self.state.connect_error = None;
            }
            Intent::ConnectHost { id, pin } => self.connect_host(&id, &pin),
            Intent::ConnectAddress { address, pin } => self.connect_address(&address, &pin),
            Intent::ForgetHost { id } => self.forget_host(&id),
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
                self.state.discovery_error = Some(text::discovery_unavailable(&error));
            }
            AppEvent::Viewer { id, update } => {
                let connected = update == ViewerUpdate::Connected;
                self.state.apply_viewer_update(id, update);
                // The viewer process has just remembered the host.
                if connected {
                    self.reload_known_hosts();
                }
            }
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
            self.state.connect_error = Some(text::HOST_GONE.into());
            return;
        };
        let pin = non_empty(pin);
        if pin.is_none() && !row.remembered {
            self.state.connect_error = Some(text::PIN_MISSING.into());
            return;
        }
        match row.address {
            Some(target) if row.compatible => {
                let request = LaunchRequest {
                    target,
                    // Announced on the LAN, so possibly malformed: the
                    // viewer would refuse such an id.
                    host_id: Fingerprint::is_host_id(&row.id).then_some(row.id),
                    pin,
                };
                self.launch(row.name, request);
            }
            Some(_) => {
                self.state.connect_error = Some(text::HOST_VERSION_DIFFERS.into());
            }
            None => self.state.connect_error = Some(text::HOST_NO_ADDRESS.into()),
        }
    }

    /// Any PIN is optional here: an address may belong to a host that
    /// remembers this PC; if not, the viewer reports that a PIN is needed.
    fn connect_address(&mut self, address: &str, pin: &str) {
        match parse_target(Some(address.trim()), DEFAULT_PORT) {
            Target::Address(target) => {
                let label = if target.port() == DEFAULT_PORT {
                    target.ip().to_string()
                } else {
                    target.to_string()
                };
                let request = LaunchRequest {
                    target,
                    host_id: None,
                    pin: non_empty(pin),
                };
                self.launch(label, request);
            }
            _ => {
                self.state.connect_error = Some(text::ADDRESS_INVALID.into());
            }
        }
    }

    fn launch(&mut self, label: String, request: LaunchRequest) {
        let address = request.target;
        if self.state.active_viewer_for(address).is_some() {
            self.state.connect_error = Some(text::VIEWER_ALREADY_OPEN.into());
            return;
        }
        let id = self.next_viewer;
        self.next_viewer += 1;
        let status = match self.launcher.launch(id, &request) {
            Ok(()) => ViewerStatus::Connecting,
            Err(e) => ViewerStatus::Failed(text::viewer_start_failed(&format!("{e:#}"))),
        };
        self.state.viewers.retain(|v| v.address != address);
        self.state.viewers.push(ViewerSession {
            id,
            label,
            address,
            host_id: request.host_id,
            status,
        });
        self.state.selected_host = None;
        self.state.connect_error = None;
    }

    fn forget_host(&mut self, id: &str) {
        let Some(host) = self.state.remembered.known(id) else {
            return;
        };
        let fingerprint = host.fingerprint;
        let forgotten = self.known_hosts.forget(&fingerprint);
        self.reload_known_hosts();
        self.state.connect_error = forgotten
            .err()
            .map(|e| text::forget_failed(&format!("{e:#}")));
    }

    fn reload_known_hosts(&mut self) {
        let (known, warning) = self.known_hosts.load();
        self.state.remembered.set(known, warning);
    }
}

/// The PIN as typed, without surrounding blanks; `None` if nothing was.
fn non_empty(pin: &str) -> Option<String> {
    let pin = pin.trim();
    (!pin.is_empty()).then(|| pin.to_string())
}

/// A setup failure of the sharing service, for the user.
fn setup_error(error: &anyhow::Error) -> String {
    text::sharing_failed(&HostFailure::of(error))
}

#[cfg(test)]
mod fakes;
#[cfg(test)]
mod pairing_tests;
#[cfg(test)]
mod tests;
