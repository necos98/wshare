//! Fakes of the ports, recording what the controller asked for.

use std::cell::RefCell;
use std::rc::Rc;

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use anyhow::{Result, bail};
use discovery::MacAddress;
use net::Fingerprint;
use protocol::transfer::TransferId;

use super::*;
use crate::access_mode::AccessMode;
use crate::gui::ports::{ChoicePurpose, MonitorChoice};
use crate::host::{ApprovalAnswer, ApprovalId, HostEvent, HostFailure};
use crate::options::HostOptions;
use crate::transfer::TransferCommand;
use crate::trust::KnownHost;
use crate::unattended::ServiceSettings;

/// Shared log of what the fakes were asked to do.
#[derive(Default)]
pub(super) struct Log {
    pub(super) calls: Vec<String>,
    pub(super) fail_start: Option<HostFailure>,
    pub(super) fail_launch: bool,
    pub(super) started: Vec<(HostOptions, u64)>,
    pub(super) launched: Vec<(ViewerId, LaunchRequest)>,
    /// What the known-hosts store holds.
    pub(super) known: Vec<KnownHost>,
    pub(super) fail_forget: bool,
    pub(super) fail_favorite: bool,
    pub(super) fail_note: bool,
    pub(super) fail_wake: bool,
    /// The wake-up signals sent: the MAC and the last address given.
    pub(super) woken: Vec<(MacAddress, Option<IpAddr>)>,
    /// What the settings file holds, and the warning loading it gives.
    pub(super) settings: Settings,
    pub(super) settings_warning: Option<String>,
    pub(super) saved: Vec<Settings>,
    pub(super) fail_save: bool,
    pub(super) fail_autostart: bool,
    pub(super) monitors: Vec<MonitorChoice>,
    pub(super) fail_monitors: bool,
    /// The dialogs opened, and the folders opened in Explorer.
    pub(super) chosen: Vec<ChoicePurpose>,
    pub(super) opened: Vec<PathBuf>,
    pub(super) fail_open: bool,
    /// The files given to the service to send; `no_viewer`: it has none.
    pub(super) sent: Vec<Vec<PathBuf>>,
    pub(super) no_viewer: bool,
    /// The file commands handed to viewer windows.
    pub(super) commands: Vec<(ViewerId, TransferCommand)>,
    pub(super) fail_command: bool,
    /// Whether the unattended-access service is installed, and whether
    /// its state cannot be read.
    pub(super) unattended_installed: bool,
    pub(super) unattended_unreadable: bool,
    /// The host settings handed to the last install of the service.
    pub(super) unattended_settings: Option<ServiceSettings>,
}

pub(super) type Shared = Rc<RefCell<Log>>;

pub(super) struct FakeService(Shared);
struct FakeControl(Shared);
struct FakePendingStop(Shared);
pub(super) struct FakeLauncher(Shared);
pub(super) struct FakeKnownHosts(Shared);
pub(super) struct FakeSettings(Shared);
pub(super) struct FakeWake(Shared);
pub(super) struct FakeDesktop(Shared);
pub(super) struct FakeUnattended(Shared);

impl SharingService for FakeService {
    fn start(&mut self, options: HostOptions, generation: u64) -> Result<Box<dyn SharingControl>> {
        let mut log = self.0.borrow_mut();
        if let Some(failure) = log.fail_start.clone() {
            return Err(failure.into());
        }
        log.started.push((options, generation));
        Ok(Box::new(FakeControl(self.0.clone())))
    }
}

impl SharingControl for FakeControl {
    fn set_input_allowed(&self, allowed: bool) {
        self.0.borrow_mut().calls.push(format!("input {allowed}"));
    }
    fn set_clipboard_shared(&self, shared: bool) {
        self.0
            .borrow_mut()
            .calls
            .push(format!("clipboard {shared}"));
    }
    fn set_receive_files(&self, receive: bool) {
        self.0
            .borrow_mut()
            .calls
            .push(format!("receive_files {receive}"));
    }
    fn set_audio_shared(&self, shared: bool) {
        self.0.borrow_mut().calls.push(format!("audio {shared}"));
    }
    fn send_files(&self, picked: Vec<PathBuf>) -> bool {
        let mut log = self.0.borrow_mut();
        if log.no_viewer {
            return false;
        }
        log.sent.push(picked);
        true
    }
    fn cancel_transfer(&self, id: Option<TransferId>) {
        let call = format!(
            "cancel_transfer {}",
            id.map_or("all".into(), |id| id.0.to_string())
        );
        self.0.borrow_mut().calls.push(call);
    }
    fn set_access_mode(&self, access: AccessMode) {
        self.0.borrow_mut().calls.push(format!("access {access:?}"));
    }
    fn answer_approval(&self, id: ApprovalId, answer: ApprovalAnswer) {
        let call = format!("approval {id} {answer:?}");
        self.0.borrow_mut().calls.push(call);
    }
    fn new_pin(&self) -> String {
        self.0.borrow_mut().calls.push("new_pin".into());
        "654321".into()
    }
    fn disconnect_viewer(&self) {
        self.0.borrow_mut().calls.push("disconnect".into());
    }
    fn forget_device(&self, fingerprint: &Fingerprint) {
        let call = format!("forget_device {}", short(fingerprint));
        self.0.borrow_mut().calls.push(call);
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
    fn launch(&mut self, id: ViewerId, request: &LaunchRequest) -> Result<()> {
        let mut log = self.0.borrow_mut();
        if log.fail_launch {
            bail!("file not found");
        }
        log.launched.push((id, request.clone()));
        Ok(())
    }

    fn cancel(&mut self, id: ViewerId) {
        self.0.borrow_mut().calls.push(format!("cancel {id}"));
    }

    fn command(&mut self, id: ViewerId, command: TransferCommand) -> Result<()> {
        let mut log = self.0.borrow_mut();
        if log.fail_command {
            bail!("the viewer is not running");
        }
        log.commands.push((id, command));
        Ok(())
    }
}

impl Desktop for FakeDesktop {
    fn choose(&self, purpose: ChoicePurpose) {
        self.0.borrow_mut().chosen.push(purpose);
    }

    fn open_folder(&self, folder: &std::path::Path) -> Result<()> {
        let mut log = self.0.borrow_mut();
        if log.fail_open {
            bail!("access denied");
        }
        log.opened.push(folder.to_path_buf());
        Ok(())
    }
}

impl KnownHostsStore for FakeKnownHosts {
    fn load(&self) -> (Vec<KnownHost>, Option<String>) {
        (self.0.borrow().known.clone(), None)
    }

    fn forget(&self, fingerprint: &Fingerprint) -> Result<()> {
        let mut log = self.0.borrow_mut();
        log.calls
            .push(format!("forget_host {}", short(fingerprint)));
        if log.fail_forget {
            bail!("access denied");
        }
        log.known.retain(|h| &h.fingerprint != fingerprint);
        Ok(())
    }

    fn set_favorite(&self, fingerprint: &Fingerprint, favorite: bool) -> Result<()> {
        let mut log = self.0.borrow_mut();
        log.calls
            .push(format!("favorite {} {favorite}", short(fingerprint)));
        if log.fail_favorite {
            bail!("access denied");
        }
        for host in log
            .known
            .iter_mut()
            .filter(|h| &h.fingerprint == fingerprint)
        {
            host.favorite = favorite;
        }
        Ok(())
    }

    fn note_announced(
        &self,
        fingerprint: &Fingerprint,
        address: Option<SocketAddr>,
        mac: Option<MacAddress>,
    ) -> Result<()> {
        let mut log = self.0.borrow_mut();
        log.calls.push(format!("note {}", short(fingerprint)));
        if log.fail_note {
            bail!("access denied");
        }
        for host in log
            .known
            .iter_mut()
            .filter(|h| &h.fingerprint == fingerprint)
        {
            host.address = address.or(host.address);
            host.mac = mac.or(host.mac);
        }
        Ok(())
    }
}

/// First byte of a fingerprint, enough to tell the fakes' ones apart.
pub(super) fn short(fingerprint: &Fingerprint) -> String {
    format!("{:02x}", fingerprint.0[0])
}

pub(super) type TestController = Controller<
    FakeService,
    FakeLauncher,
    FakeKnownHosts,
    FakeSettings,
    FakeWake,
    FakeDesktop,
    FakeUnattended,
>;

/// Every port, on the fakes of `log`.
fn ports(
    log: &Shared,
) -> Ports<
    FakeService,
    FakeLauncher,
    FakeKnownHosts,
    FakeSettings,
    FakeWake,
    FakeDesktop,
    FakeUnattended,
> {
    Ports {
        service: FakeService(log.clone()),
        launcher: FakeLauncher(log.clone()),
        known_hosts: FakeKnownHosts(log.clone()),
        backend: FakeSettings(log.clone()),
        wake: FakeWake(log.clone()),
        desktop: FakeDesktop(log.clone()),
        unattended: FakeUnattended(log.clone()),
    }
}

/// A controller whose known-hosts store holds `known`.
pub(super) fn controller_knowing(known: Vec<KnownHost>) -> (TestController, Shared) {
    let log = Shared::default();
    log.borrow_mut().known = known;
    let controller = Controller::new(ports(&log));
    (controller, log)
}

pub(super) fn controller() -> (TestController, Shared) {
    controller_knowing(Vec::new())
}

pub(super) fn calls(log: &Shared) -> Vec<String> {
    std::mem::take(&mut log.borrow_mut().calls)
}

pub(super) fn host(generation: u64, event: HostEvent) -> AppEvent {
    AppEvent::Host { generation, event }
}

impl SettingsBackend for FakeSettings {
    fn load(&self) -> (Settings, Option<String>) {
        let log = self.0.borrow();
        (log.settings.clone(), log.settings_warning.clone())
    }

    fn save(&self, settings: &Settings) -> Result<()> {
        let mut log = self.0.borrow_mut();
        if log.fail_save {
            bail!("disk full");
        }
        log.saved.push(settings.clone());
        log.settings = settings.clone();
        Ok(())
    }

    fn set_autostart(&self, enabled: bool, minimized: bool) -> Result<()> {
        let mut log = self.0.borrow_mut();
        log.calls.push(format!("autostart {enabled} {minimized}"));
        if log.fail_autostart {
            bail!("registry is read-only");
        }
        Ok(())
    }

    fn monitors(&self) -> Result<Vec<MonitorChoice>> {
        let log = self.0.borrow();
        if log.fail_monitors {
            bail!("no display");
        }
        Ok(log.monitors.clone())
    }
}

/// A controller on the fakes of `log`, as the test prepared it.
pub(super) fn controller_from(log: &Shared) -> TestController {
    Controller::new(ports(log))
}

impl WakeOnLan for FakeWake {
    fn wake(&self, mac: MacAddress, last_address: Option<IpAddr>) -> Result<()> {
        let mut log = self.0.borrow_mut();
        if log.fail_wake {
            bail!("network is down");
        }
        log.woken.push((mac, last_address));
        Ok(())
    }
}

impl UnattendedAccess for FakeUnattended {
    fn installed(&self) -> Result<bool> {
        let log = self.0.borrow();
        if log.unattended_unreadable {
            bail!("no access to the service manager");
        }
        Ok(log.unattended_installed)
    }

    fn install(&self, settings: ServiceSettings, password: Option<String>) {
        let mut log = self.0.borrow_mut();
        log.calls
            .push(format!("unattended install {} {password:?}", settings.port));
        log.unattended_settings = Some(settings);
    }

    fn uninstall(&self) {
        self.0
            .borrow_mut()
            .calls
            .push("unattended uninstall".to_string());
    }
}
