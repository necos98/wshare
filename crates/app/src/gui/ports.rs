//! Boundaries between the interface logic and the outside world. Real
//! implementations live in `sharing`, `launcher`, `discovery_source`,
//! `known_hosts_file` and `system_settings`; tests use fakes.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use anyhow::Result;
use discovery::DiscoveryEvent;
use net::Fingerprint;

use crate::host::HostEvent;
use crate::options::HostOptions;
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
}

/// Opens viewer windows.
pub trait ViewerLauncher {
    /// Starts a viewer; its progress arrives as [`AppEvent::Viewer`] with
    /// `id`.
    fn launch(&mut self, id: ViewerId, request: &LaunchRequest) -> Result<()>;
    /// Ends the viewer `id` (still connecting). Its late events are ignored
    /// by the interface, which has already dropped the entry.
    fn cancel(&mut self, id: ViewerId);
}

/// The hosts this PC is remembered by (written by the viewers it launches).
pub trait KnownHostsStore {
    /// The current list; on failure an empty one and the reason.
    fn load(&self) -> (Vec<KnownHost>, Option<String>);
    /// Forgets a host: it is neither pinned nor connected to without a PIN.
    fn forget(&self, fingerprint: &Fingerprint) -> Result<()>;
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
