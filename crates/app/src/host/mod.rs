//! Sharing this PC: a background service any front-end (CLI, GUI) can
//! start, steer and observe.
//!
//! CONTRACT shared by the CLI and the GUI. The public items below are the
//! API; their bodies are implemented in this module's private submodules:
//!
//! - `service`: the service thread and runtime, setup, stop.
//! - `accept`: one viewer at a time until stopped.
//! - `session`: one viewer from Hello to goodbye.
//! - `streaming`: the capture/encode thread of a session.
//! - `input_gate`: remote input behind the "allow input" switch.
//! - `state`: PIN, input flag and end signals shared with the handle.
//! - `events`: delivery to the observer, rate limiting.
//! - `console` + `interrupt`: the CLI front-end (`run`).

mod accept;
mod console;
mod events;
mod input_gate;
mod interrupt;
mod service;
mod session;
mod state;
mod streaming;
#[cfg(test)]
mod tests;

use std::net::SocketAddr;

use anyhow::Result;
use pipeline::HostStats;

use crate::cli::HostOptions;

/// Something that happened while sharing. Front-ends render these; the
/// service never prints.
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    /// Ready for viewers.
    Listening {
        computer_name: String,
        port: u16,
        pin: String,
        fingerprint: String,
        monitor: String,
        width: u32,
        height: u32,
    },
    /// The PIN changed (see [`HostHandle::new_pin`]).
    PinChanged {
        pin: String,
    },
    ViewerConnected {
        viewer_name: String,
        address: SocketAddr,
    },
    ViewerRejected {
        address: SocketAddr,
        reason: String,
    },
    /// `error` is set when the session ended abnormally on our side.
    ViewerDisconnected {
        address: SocketAddr,
        stats: HostStats,
        error: Option<String>,
    },
    /// Windows refused injected input (elevated window, UAC, lock screen).
    /// Sent once per blocked stretch.
    InputBlocked,
    /// Something non-fatal worth showing (e.g. LAN announcement failed).
    Warning(String),
    /// The service ended; `error` explains why if it was not asked to stop.
    Stopped {
        error: Option<String>,
    },
}

/// Receives the service's events. Called from the service's own threads,
/// so implementations must be quick (e.g. forward to a channel and wake the
/// UI) and thread-safe.
pub trait HostObserver: Send + Sync + 'static {
    fn on_event(&self, event: HostEvent);
}

/// Control handle of a running sharing service. Dropping it stops sharing.
pub struct HostHandle {
    service: service::Service,
}

impl HostHandle {
    /// Starts sharing in the background (own thread and async runtime).
    /// Fails fast on setup errors (port in use, monitor missing); later
    /// problems are reported through `observer`. The first event is
    /// `Listening`.
    pub fn start(options: HostOptions, observer: impl HostObserver) -> Result<Self> {
        let service = service::Service::start(options, events::EventSink::new(observer))?;
        Ok(Self { service })
    }

    /// Allows or forbids remote mouse/keyboard, effective immediately, also
    /// for a viewer already connected (input it sends is then ignored).
    ///
    /// Forbidding it mid-session releases whatever the viewer holds. The
    /// viewer learns the setting only at connect time (Welcome), so a
    /// viewer admitted while input was forbidden typically sends none.
    pub fn set_input_allowed(&self, allowed: bool) {
        self.service.state().set_input_allowed(allowed);
    }

    /// Generates a new random PIN for the next connections and reports it
    /// with `PinChanged`. A connected viewer is not affected.
    pub fn new_pin(&self) -> String {
        self.service.new_pin()
    }

    /// Ends the current viewing session, if any (the viewer is told Bye).
    pub fn disconnect_viewer(&self) {
        self.service.state().end_current_session();
    }

    /// Stops sharing and waits for the service to finish; ends with
    /// `Stopped { error: None }`.
    pub fn stop(self) {
        self.service.stop();
    }
}

/// CLI front-end: `wshare host` runs until the service stops.
pub fn run(options: HostOptions) -> Result<()> {
    console::run(options)
}
