//! Boundaries between the interface logic and the outside world. Real
//! implementations live in `sharing`, `launcher` and `discovery_source`;
//! tests use fakes.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use anyhow::Result;
use discovery::DiscoveryEvent;

use crate::cli::HostOptions;
use crate::host::HostEvent;

use super::model::{ViewerId, ViewerUpdate};

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

/// What the user did on the tray icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Open,
    ToggleSharing,
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
    fn new_pin(&self) -> String;
    fn disconnect_viewer(&self);
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

/// Opens viewer windows.
pub trait ViewerLauncher {
    /// Starts a viewer connecting to `target` with `pin`; its progress
    /// arrives as [`AppEvent::Viewer`] with `id`.
    fn launch(&mut self, id: ViewerId, target: SocketAddr, pin: &str) -> Result<()>;
}
