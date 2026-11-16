//! Boundaries between the interface logic and the outside world. Real
//! implementations live in `sharing`, `launcher`, `discovery_source`, `wake_on_lan`,
//! `known_hosts_file`, `system_settings` and `shell`; tests use fakes.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Sender;

use anyhow::Result;
use discovery::{DiscoveryEvent, MacAddress};
use net::Fingerprint;
use protocol::transfer::TransferId;

use crate::access_mode::AccessMode;
use crate::host::{ApprovalAnswer, ApprovalId, HostEvent};
use crate::options::HostOptions;
use crate::transfer::TransferCommand;
use crate::trust::KnownHost;

use super::model::{ViewerId, ViewerUpdate};
use super::settings::{Settings, ViewerDisplay};

/// Everything that reaches the interface from background threads.
#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    /// An event of the sharing service started as `generation`.
    Host {
        generation: u64,
        event: HostEvent,
    },
    /// The sharing service started as `generation` finished stopping (see
    /// [`SharingControl::stop_in_background`]).
    SharingStopped {
        generation: u64,
    },
    Discovery(DiscoveryEvent),
    DiscoveryFailed(String),
    Viewer {
        id: ViewerId,
        update: ViewerUpdate,
    },
    Tray(TrayAction),
    /// What the user chose in a dialog opened for `purpose` (nothing: the
    /// dialog was cancelled).
    Chosen {
        purpose: ChoicePurpose,
        paths: Vec<PathBuf>,
    },
    /// Turning unattended access on or off (see [`UnattendedAccess`])
    /// is over.
    Unattended(UnattendedOutcome),
}

/// Why a file or folder dialog was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoicePurpose {
    /// Files to send to the viewer connected to this PC.
    SendToViewer,
    /// Files the viewer window `ViewerId` sends to its host.
    SendFromViewer(ViewerId),
    /// The folder where received files land.
    Downloads,
}

/// What the user did on the tray icon or the connection indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Open,
    ToggleSharing,
    /// "Disconnetti <nome>", also the button of the connection indicator.
    DisconnectViewer,
    NewPin,
    ToggleInputAllowed,
    Quit,
}

/// Wakes the UI thread so it processes new events (even while hidden).
pub trait Waker: Send + Sync + 'static {
    fn wake(&self);
}

/// Sending side of the interface's event queue.
#[derive(Clone)]
pub struct Inbox {
    tx: Sender<AppEvent>,
    waker: Arc<dyn Waker>,
}

impl Inbox {
    pub fn new(tx: Sender<AppEvent>, waker: Arc<dyn Waker>) -> Self {
        Self { tx, waker }
    }

    pub fn send(&self, event: AppEvent) {
        if self.tx.send(event).is_ok() {
            self.waker.wake();
        }
    }
}

/// Starts the sharing service.
pub trait SharingService {
    /// Starts sharing; its events arrive as [`AppEvent::Host`] tagged with
    /// `generation`. Fails on setup errors (port in use, ...).
    fn start(&mut self, options: HostOptions, generation: u64) -> Result<Box<dyn SharingControl>>;
}

/// Controls a running sharing service (see [`crate::host::HostHandle`]).
pub trait SharingControl {
    fn set_input_allowed(&self, allowed: bool);
    /// Turns the shared clipboard on or off, also for a viewer connected now.
    fn set_clipboard_shared(&self, shared: bool);
    /// Lets the viewer's files in, or not, also for a viewer connected now.
    fn set_receive_files(&self, receive: bool);
    /// Lets the viewer hear this PC's sound, or not, also a viewer
    /// connected now.
    fn set_audio_shared(&self, shared: bool);
    /// Sends files to the connected viewer; `false` without one.
    fn send_files(&self, picked: Vec<PathBuf>) -> bool;
    /// Cancels one transfer of the connected viewer (`None`: all).
    fn cancel_transfer(&self, id: Option<TransferId>);
    /// Changes how a viewer that is not authorized is let in.
    fn set_access_mode(&self, access: AccessMode);
    /// Answers the request of a viewer waiting for a click.
    fn answer_approval(&self, id: ApprovalId, answer: ApprovalAnswer);
    fn new_pin(&self) -> String;
    fn disconnect_viewer(&self);
    /// Removes an authorized viewer: its next connection needs the PIN.
    fn forget_device(&self, fingerprint: &Fingerprint);
    /// Stops sharing and waits until it is over (keys released, viewer told
    /// Bye, port closed). Blocks for up to about a second.
    fn stop(self: Box<Self>);
    /// Like [`Self::stop`] without blocking: the stop runs elsewhere and
    /// [`AppEvent::SharingStopped`] arrives when it is over. The returned
    /// handle waits for it (e.g. before the process exits).
    fn stop_in_background(self: Box<Self>) -> Box<dyn PendingStop>;
}

/// A stop running in the background.
pub trait PendingStop {
    /// Blocks until the stop is over.
    fn wait(self: Box<Self>);
}

/// Where and how a viewer connects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    pub target: SocketAddr,
    /// The LAN id of the host, when picked from the list: a remembered host
    /// is then pinned and connected to without a PIN.
    pub host_id: Option<String>,
    /// `None`: the host must recognise this PC (or it refuses).
    pub pin: Option<String>,
    /// How the viewer window opens (from the settings).
    pub display: ViewerDisplay,
    /// Where the host's files land; `None`: the default folder.
    pub downloads: Option<PathBuf>,
}

/// Opens viewer windows.
pub trait ViewerLauncher {
    /// Starts a viewer; its progress arrives as [`AppEvent::Viewer`] with
    /// `id`.
    fn launch(&mut self, id: ViewerId, request: &LaunchRequest) -> Result<()>;
    /// Ends the viewer `id` (still connecting). Its late events are ignored
    /// by the interface, which has already dropped the entry.
    fn cancel(&mut self, id: ViewerId);
    /// Hands a file command to the viewer `id` (sending files, cancelling
    /// transfers).
    fn command(&mut self, id: ViewerId, command: TransferCommand) -> Result<()>;
}

/// The Windows shell: dialogs and Explorer.
pub trait Desktop {
    /// Opens the dialog for `purpose` without waiting for it; the choice
    /// arrives as [`AppEvent::Chosen`].
    fn choose(&self, purpose: ChoicePurpose);
    /// Opens `folder` in Explorer.
    fn open_folder(&self, folder: &Path) -> Result<()>;
}

/// The hosts this PC is remembered by (written by the viewers it launches).
pub trait KnownHostsStore {
    /// The current list; on failure an empty one and the reason.
    fn load(&self) -> (Vec<KnownHost>, Option<String>);
    /// Forgets a host: it is neither pinned nor connected to without a PIN.
    fn forget(&self, fingerprint: &Fingerprint) -> Result<()>;
    /// Pins a host to the top of the list, or takes it off.
    fn set_favorite(&self, fingerprint: &Fingerprint, favorite: bool) -> Result<()>;
    /// Stores the address and MAC a known host announces now, if they differ.
    fn note_announced(
        &self,
        fingerprint: &Fingerprint,
        address: Option<SocketAddr>,
        mac: Option<MacAddress>,
    ) -> Result<()>;
}

/// A monitor of this PC the user can share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorChoice {
    /// The index `wshare monitors` prints and `--monitor` takes.
    pub index: usize,
    pub label: String,
}

/// Where the settings live and what changing them touches outside the
/// window: the settings file, the Windows start-up entry and the monitor
/// list.
pub trait SettingsBackend {
    /// The saved settings; on a missing, damaged or unreadable file the
    /// defaults and, unless it is just missing, the reason.
    fn load(&self) -> (Settings, Option<String>);
    fn save(&self, settings: &Settings) -> Result<()>;
    /// Registers (`enabled`) or removes the start-up of wshare with
    /// Windows; `minimized` makes it open hidden in the tray.
    fn set_autostart(&self, enabled: bool, minimized: bool) -> Result<()>;
    /// The monitors that can be captured now.
    fn monitors(&self) -> Result<Vec<MonitorChoice>>;
}

/// Wakes a computer that is switched off (Wake-on-LAN).
pub trait WakeOnLan {
    /// Sends the wake-up signal for `mac`. `last_address` is where the
    /// computer was last seen: its network is also reached directly.
    fn wake(&self, mac: MacAddress, last_address: Option<IpAddr>) -> Result<()>;
}

/// How turning unattended access on or off ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnattendedOutcome {
    Done,
    /// The user said no to Windows' request for administrator rights.
    Cancelled,
    Failed(String),
}

/// The unattended-access service (see `unattended`).
pub trait UnattendedAccess {
    /// Whether the service is installed now.
    fn installed(&self) -> Result<bool>;
    /// Installs the service (or updates it: authorized devices copied again,
    /// password replaced) without waiting: Windows asks the user for
    /// administrator rights, and [`AppEvent::Unattended`] tells how it
    /// ended. `password`: the one viewers must give; `None` for none.
    fn install(&self, port: u16, password: Option<String>);
    /// Removes the service and its files, like [`Self::install`].
    fn uninstall(&self);
}
