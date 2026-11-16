//! State shared between the control handle (any thread, e.g. the GUI) and
//! the running service (its own runtime): PIN, input, clipboard and file
//! permissions, the sessions in progress, the way to the viewer's file
//! transfers and the "please end" signals.
//!
//! Everything here is read at the moment it matters (the PIN per connection
//! attempt, the input flag per event) so changes take effect immediately
//! without restarting the service.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::watch;

use super::sessions::{SessionId, SessionLimits, Sessions};
use crate::access_mode::AccessMode;
use crate::transfer::TransferCommand;

/// Fires a one-shot cancellation that async code can await.
///
/// Built on `watch` rather than `Notify` because a watch value is sticky: a
/// listener that starts waiting after `cancel` still sees it, so there is no
/// lost-wakeup window between loop iterations.
pub(super) struct Cancel(watch::Sender<bool>);

/// Listening side of a [`Cancel`].
#[derive(Clone)]
pub(super) struct Cancelled(watch::Receiver<bool>);

impl Cancel {
    pub(super) fn new() -> (Self, Cancelled) {
        let (tx, rx) = watch::channel(false);
        (Self(tx), Cancelled(rx))
    }

    pub(super) fn cancel(&self) {
        self.0.send_replace(true);
    }

    pub(super) fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
}

impl Cancelled {
    /// Resolves once cancelled. A dropped [`Cancel`] counts as cancelled:
    /// nobody could ever lift it, and ending is the safe default.
    pub(super) async fn wait(&mut self) {
        let _ = self.0.wait_for(|cancelled| *cancelled).await;
    }

    pub(super) fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
}

/// Why a connection may not start a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionRefusal {
    /// The service is stopping.
    Stopping,
    /// As many connections as allowed are being admitted already.
    TooManyPending,
}

pub(super) struct SharedState {
    pin: Mutex<String>,
    access: Mutex<AccessMode>,
    input_allowed: watch::Sender<bool>,
    clipboard_shared: watch::Sender<bool>,
    receive_files: watch::Sender<bool>,
    /// The file transfers of the viewer connected now, if any.
    transfers: Mutex<Option<UnboundedSender<TransferCommand>>>,
    stop: Cancel,
    stop_listener: Cancelled,
    /// Under one lock with the stop check, so "stop" and "a new session
    /// begins" cannot interleave and leave a session running after stop.
    sessions: Mutex<Sessions>,
}

impl SharedState {
    pub(super) fn new(
        pin: String,
        input_allowed: bool,
        clipboard_shared: bool,
        receive_files: bool,
        access: AccessMode,
        limits: SessionLimits,
    ) -> Self {
        let (stop, stop_listener) = Cancel::new();
        Self {
            pin: Mutex::new(pin),
            access: Mutex::new(access),
            input_allowed: watch::channel(input_allowed).0,
            clipboard_shared: watch::channel(clipboard_shared).0,
            receive_files: watch::channel(receive_files).0,
            transfers: Mutex::new(None),
            stop,
            stop_listener,
            sessions: Mutex::new(Sessions::new(limits)),
        }
    }

    /// The PIN a connecting viewer must give right now.
    pub(super) fn pin(&self) -> String {
        lock(&self.pin).clone()
    }

    /// Replaces the PIN for future connection attempts; returns the new one.
    pub(super) fn replace_pin(&self, pin: String) -> String {
        *lock(&self.pin) = pin.clone();
        pin
    }

    /// How an unknown viewer is let in, for the next connection attempts.
    pub(super) fn access(&self) -> AccessMode {
        *lock(&self.access)
    }

    pub(super) fn set_access(&self, access: AccessMode) {
        *lock(&self.access) = access;
    }

    /// Changes the input permission; sessions watching it are woken only on
    /// an actual change.
    pub(super) fn set_input_allowed(&self, allowed: bool) {
        self.input_allowed.send_if_modified(|current| {
            let changed = *current != allowed;
            *current = allowed;
            changed
        });
    }

    /// A receiver that wakes when the input permission changes.
    pub(super) fn watch_input(&self) -> watch::Receiver<bool> {
        self.input_allowed.subscribe()
    }

    /// Turns the shared clipboard on or off; sessions watching it are woken
    /// only on an actual change.
    pub(super) fn set_clipboard_shared(&self, shared: bool) {
        self.clipboard_shared.send_if_modified(|current| {
            let changed = *current != shared;
            *current = shared;
            changed
        });
    }

    /// A receiver that wakes when the clipboard is turned on or off.
    pub(super) fn watch_clipboard(&self) -> watch::Receiver<bool> {
        self.clipboard_shared.subscribe()
    }

    /// Lets the viewer's files in, or not, from the next one on.
    pub(super) fn set_receive_files(&self, receive: bool) {
        self.receive_files.send_replace(receive);
    }

    pub(super) fn watch_receive_files(&self) -> watch::Receiver<bool> {
        self.receive_files.subscribe()
    }

    /// The admitted viewer's transfers take commands on `commands` (`None`:
    /// no viewer any more).
    pub(super) fn set_transfers(&self, commands: Option<UnboundedSender<TransferCommand>>) {
        *lock(&self.transfers) = commands;
    }

    /// Hands `command` to the viewer's transfers; `false` without a viewer.
    pub(super) fn transfer_command(&self, command: TransferCommand) -> bool {
        lock(&self.transfers)
            .as_ref()
            .is_some_and(|commands| commands.send(command).is_ok())
    }

    /// Registers a new connection to admit. The session lasts as long as
    /// the returned ticket.
    pub(super) fn begin_session(self: &Arc<Self>) -> Result<SessionTicket, SessionRefusal> {
        let mut sessions = lock(&self.sessions);
        if self.stop.is_cancelled() {
            return Err(SessionRefusal::Stopping);
        }
        let (id, cancelled) = sessions.begin().ok_or(SessionRefusal::TooManyPending)?;
        Ok(SessionTicket {
            state: Arc::clone(self),
            id,
            cancelled,
        })
    }

    /// Whether as many viewers as allowed are watching already.
    pub(super) fn viewers_full(&self) -> bool {
        lock(&self.sessions).is_full()
    }

    /// Asks the admitted viewers to end; the service keeps running.
    pub(super) fn end_current_session(&self) {
        lock(&self.sessions).cancel_viewers();
    }

    /// Asks the service to stop: no new sessions, the running ones end.
    pub(super) fn request_stop(&self) {
        let sessions = lock(&self.sessions);
        self.stop.cancel();
        sessions.cancel_all();
    }

    /// Resolves once [`Self::request_stop`] was called.
    pub(super) fn stop_requested(&self) -> Cancelled {
        self.stop_listener.clone()
    }
}

/// One registered session, from its connection to its end; dropping it
/// unregisters the session (also when its task unwinds).
pub(super) struct SessionTicket {
    state: Arc<SharedState>,
    id: SessionId,
    cancelled: Cancelled,
}

impl SessionTicket {
    /// Fires when the host wants this session over (disconnect or stop).
    pub(super) fn cancelled(&self) -> Cancelled {
        self.cancelled.clone()
    }

    /// Makes this session a viewer; `false` if as many viewers as allowed
    /// are watching already, or if the session was cancelled.
    pub(super) fn admit(&self) -> bool {
        lock(&self.state.sessions).admit(self.id)
    }
}

impl Drop for SessionTicket {
    fn drop(&mut self) {
        lock(&self.state.sessions).end(self.id);
    }
}

/// A panic elsewhere must not make PIN/flag changes panic too: the data
/// behind these locks is always consistent (plain assignments).
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Arc<SharedState> {
        Arc::new(SharedState::new(
            "111111".into(),
            true,
            true,
            true,
            AccessMode::PinOnly,
            SessionLimits::default(),
        ))
    }

    #[test]
    fn pin_is_replaced_for_later_reads() {
        let state = state();
        assert_eq!(state.pin(), "111111");
        assert_eq!(state.replace_pin("222222".into()), "222222");
        assert_eq!(state.pin(), "222222");
    }

    #[test]
    fn the_access_mode_is_read_at_each_attempt_and_starts_as_given() {
        let state = state();
        assert_eq!(state.access(), AccessMode::PinOnly);
        state.set_access(AccessMode::Confirmation);
        assert_eq!(state.access(), AccessMode::Confirmation);
    }

    #[test]
    fn input_flag_changes_wake_watchers_only_on_real_changes() {
        let state = state();
        let mut watcher = state.watch_input();
        state.set_input_allowed(true);
        assert!(
            !watcher.has_changed().unwrap(),
            "same value is not a change"
        );
        state.set_input_allowed(false);
        assert!(watcher.has_changed().unwrap());
        assert!(!*watcher.borrow_and_update());
        assert!(!*state.watch_input().borrow());
    }

    #[test]
    fn the_clipboard_flag_wakes_watchers_only_on_real_changes() {
        let state = state();
        let mut watcher = state.watch_clipboard();
        state.set_clipboard_shared(true);
        assert!(!watcher.has_changed().unwrap());
        state.set_clipboard_shared(false);
        assert!(watcher.has_changed().unwrap());
        assert!(!*watcher.borrow_and_update());
        assert!(!*state.watch_clipboard().borrow());
    }

    #[test]
    fn file_commands_reach_the_session_that_takes_them_and_only_it() {
        let state = state();
        assert!(
            !state.transfer_command(TransferCommand::CancelAll),
            "no viewer"
        );
        let (commands, mut taken) = tokio::sync::mpsc::unbounded_channel();
        state.set_transfers(Some(commands));
        assert!(state.transfer_command(TransferCommand::CancelAll));
        assert_eq!(taken.try_recv(), Ok(TransferCommand::CancelAll));
        state.set_transfers(None);
        assert!(!state.transfer_command(TransferCommand::CancelAll));
    }

    #[test]
    fn the_reception_of_files_is_read_live() {
        let state = state();
        let watcher = state.watch_receive_files();
        assert!(*watcher.borrow());
        state.set_receive_files(false);
        assert!(!*watcher.borrow());
    }

    #[test]
    fn ending_the_current_session_leaves_the_service_running() {
        let state = state();
        let viewer = state.begin_session().expect("not stopping");
        assert!(viewer.admit());
        state.end_current_session();
        assert!(viewer.cancelled().is_cancelled());
        assert!(!state.stop_requested().is_cancelled());
        drop(viewer);
        assert!(
            !state.begin_session().unwrap().cancelled().is_cancelled(),
            "next session is fresh"
        );
    }

    #[test]
    fn ending_the_current_session_spares_admissions_in_progress() {
        let state = state();
        let pending = state.begin_session().unwrap();
        state.end_current_session();
        assert!(!pending.cancelled().is_cancelled());
    }

    #[test]
    fn a_dropped_ticket_frees_the_viewer_place() {
        let state = state();
        let viewer = state.begin_session().unwrap();
        let next = state.begin_session().unwrap();
        assert!(viewer.admit());
        assert!(state.viewers_full());
        assert!(!next.admit(), "one viewer at a time");
        drop(viewer);
        assert!(!state.viewers_full());
        assert!(next.admit());
    }

    #[test]
    fn too_many_admissions_at_once_are_refused() {
        let state = state();
        let tickets: Vec<SessionTicket> = (0..SessionLimits::default().max_pending)
            .map(|_| state.begin_session().unwrap())
            .collect();
        assert_eq!(
            state.begin_session().err(),
            Some(SessionRefusal::TooManyPending)
        );
        drop(tickets);
        assert!(state.begin_session().is_ok());
    }

    #[test]
    fn stop_ends_every_session_and_refuses_new_ones() {
        let state = state();
        let viewer = state.begin_session().unwrap();
        assert!(viewer.admit());
        let pending = state.begin_session().unwrap();
        state.request_stop();
        assert!(viewer.cancelled().is_cancelled());
        assert!(pending.cancelled().is_cancelled());
        assert!(state.stop_requested().is_cancelled());
        assert!(
            !pending.admit(),
            "a stopped admission never becomes a viewer"
        );
        assert_eq!(state.begin_session().err(), Some(SessionRefusal::Stopping));
    }

    #[tokio::test]
    async fn cancelled_resolves_after_cancel_and_when_dropped() {
        let (cancel, mut listener) = Cancel::new();
        cancel.cancel();
        listener.wait().await;

        let (cancel, mut listener) = Cancel::new();
        drop(cancel);
        listener.wait().await;
    }
}
