//! Sharing this PC: a background service any front-end (CLI, GUI) can
//! start, steer and observe.
//!
//! CONTRACT shared by the CLI and the GUI. The public items below are the
//! API; their bodies are implemented in this module's private submodules:
//!
//! - `service`: the service thread and runtime, setup, stop.
//! - `adapter_mac`: the MAC announced on the LAN, for Wake-on-LAN.
//! - `accept`: every connection on its own task until stopped.
//! - `sessions`: the connections being admitted and the viewers, within
//!   their limits (one viewer at a time).
//! - `admission`: Hello and its verdict (remembered device, PIN or the
//!   click of the person at the host; busy).
//! - `approvals` + `approval_limiter`: the requests to accept or refuse a
//!   viewer, one at a time and limited per address.
//! - `pin_attempts` + `pin_guard`: wrong PINs pause their address and,
//!   too many overall, rotate the PIN.
//! - `session`: one viewer from Hello to goodbye.
//! - `devices`: the viewers admitted without a PIN, shared with the handle.
//! - `streaming`: the capture/encode thread of a session; `stream_setup`
//!   opens its capture and encoder.
//! - `screen_choice`: the monitor and quality a viewer chose.
//! - `monitor_list`: the monitors as the viewer and remote input see them.
//! - `screen_switch`: a session switching screens (requests, aftermath).
//! - `rate_adaptation`: the encoder's bitrate fitted to the network.
//! - `input_gate`: remote input behind the "allow input" switch.
//! - `input_switch`: what a session does when that switch flips.
//! - `clipboard`: the shared clipboard of a session.
//! - `state`: PIN, input and clipboard flags, the shared monitor and end
//!   signals shared with the handle.
//! - `cause`: the typed reasons of failures and warnings.
//! - `events`: delivery to the observer, rate limiting.
//! - `console` + `interrupt`: the CLI front-end (`run`).

mod accept;
mod adapter_mac;
mod admission;
mod approval_limiter;
mod approvals;
mod cause;
mod clipboard;
mod console;
mod devices;
mod events;
mod input_gate;
mod input_switch;
mod interrupt;
mod monitor_list;
mod pin_attempts;
mod pin_guard;
mod rate_adaptation;
mod screen_choice;
mod screen_switch;
mod service;
mod session;
mod sessions;
mod state;
mod stream_setup;
mod streaming;
#[cfg(test)]
mod test_context;
#[cfg(test)]
mod tests;

use std::net::SocketAddr;

use anyhow::Result;
use net::Fingerprint;
use pipeline::HostStats;
use protocol::control::RejectReason;

pub use approvals::{ApprovalAnswer, ApprovalId, ApprovalRequest};
pub use cause::{HostFailure, HostWarning, SessionFailure};

use crate::access_mode::AccessMode;
use crate::options::HostOptions;
use crate::trust::AuthorizedDevice;

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
    PinChanged { pin: String },
    /// The viewers that connect without a PIN, oldest first. Sent right
    /// after `Listening` and whenever the list changes (a viewer admitted
    /// or removed, a last access updated).
    AuthorizedDevices(Vec<AuthorizedDevice>),
    /// A viewer that is not authorized waits for the person at this PC to
    /// accept or refuse it (see [`HostHandle::answer_approval`]). It is let
    /// in only by an explicit `Accept`; without an answer it is refused
    /// after a while.
    ApprovalRequested(ApprovalRequest),
    /// The request is over, answered or not: the prompt can go.
    ApprovalClosed { id: ApprovalId },
    ViewerConnected {
        viewer_name: String,
        address: SocketAddr,
    },
    ViewerRejected {
        address: SocketAddr,
        reason: RejectReason,
    },
    /// `error` is set when the session ended abnormally on our side.
    ViewerDisconnected {
        address: SocketAddr,
        stats: HostStats,
        error: Option<SessionFailure>,
    },
    /// The screen being shared changed: the viewer chose another monitor,
    /// or the monitor switched resolution. Viewers that connect later get
    /// this monitor too.
    ScreenChanged {
        monitor: String,
        width: u32,
        height: u32,
    },
    /// Windows refused injected input (elevated window, UAC, lock screen).
    /// Sent once per blocked stretch.
    InputBlocked,
    /// Something non-fatal worth showing.
    Warning(HostWarning),
    /// The service ended; `error` explains why if it was not asked to stop.
    Stopped { error: Option<HostFailure> },
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
    /// Forbidding it mid-session releases whatever the viewer holds. A
    /// connected viewer is told every actual change (`InputAllowed`); a new
    /// one learns the setting in Welcome.
    pub fn set_input_allowed(&self, allowed: bool) {
        self.service.state().set_input_allowed(allowed);
    }

    /// Turns the shared clipboard on or off, effective immediately, also for
    /// a viewer already connected. Off, nothing is sent to the viewer and
    /// nothing it sends is written to this PC's clipboard.
    pub fn set_clipboard_shared(&self, shared: bool) {
        self.service.state().set_clipboard_shared(shared);
    }

    /// Changes how a viewer that is not authorized is let in, effective for
    /// the next connection attempts. Viewers connected now, and requests
    /// already open, are not affected.
    pub fn set_access_mode(&self, access: AccessMode) {
        self.service.state().set_access(access);
    }

    /// Answers the request `id` (see [`HostEvent::ApprovalRequested`]).
    /// Does nothing if it is not open anymore (expired, or the viewer left).
    pub fn answer_approval(&self, id: ApprovalId, answer: ApprovalAnswer) {
        self.service.approvals().answer(id, answer);
    }

    /// Generates a new random PIN for the next connections and reports it
    /// with `PinChanged`. A connected viewer is not affected, and viewers
    /// already authorized still connect without a PIN. The service also
    /// rotates it by itself after too many wrong PINs (`PinChanged`, then
    /// the warning [`HostWarning::PinRotatedAfterWrongAttempts`]).
    pub fn new_pin(&self) -> String {
        self.service.new_pin()
    }

    /// Removes a viewer from the authorized devices (reported with
    /// `AuthorizedDevices`): its next connection needs the PIN. A session
    /// it has open now goes on.
    pub fn forget_device(&self, fingerprint: &Fingerprint) {
        self.service.devices().remove(fingerprint);
    }

    /// Ends the current viewing session, if any (the viewer is told Bye).
    /// Connections still being admitted are not affected.
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
